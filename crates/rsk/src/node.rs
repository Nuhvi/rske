//! The [`Node`] handle: owns the chain store and drives the operations an
//! embedder asks for.

use std::collections::VecDeque;

use anyhow::{Context, Result, bail};
use bitcoin::Network;
use bitcoin::consensus::Encodable;
use rsk_core::Core;
use tracing::{info, warn};

use crate::config::Config;
use crate::peers;
use crate::peers::PeerCache;
use crate::sync::{self, SyncReport};

/// The Rske node: a chain store plus the configuration it was opened with.
///
/// Nothing runs in the background — call [`Node::sync`] to make progress. The
/// store is durable, so a later [`Node::open`] resumes from the tip.
pub struct Node {
    core: Core,
    config: Config,
    /// Node-level peer cache, separate from the core chain store.
    peer_cache: PeerCache,
}

impl Node {
    /// Open (creating the data directory and store if needed) the node.
    ///
    /// The core anchors the store before the handle exists: the embedded
    /// checkpoint spine on mainnet, genesis on every other network. An
    /// existing store whose checkpoints or network disagree is refused there.
    pub fn open(config: Config) -> Result<Self> {
        std::fs::create_dir_all(&config.data_dir)
            .with_context(|| format!("creating data dir {}", config.data_dir.display()))?;
        // Make the location unambiguous: log the absolute path, not the
        // relative one the caller passed (or the default).
        let data_dir = std::fs::canonicalize(&config.data_dir)
            .with_context(|| format!("resolving data dir {}", config.data_dir.display()))?;
        let store_path = data_dir.join("chain.redb");
        let fresh = !store_path.exists();
        info!("data directory: {}", data_dir.display());
        info!("chain store:    {}", store_path.display());

        let core = Core::open(&store_path, config.network)?;
        if fresh {
            let tip = core.tip_height()?.context("the store was just anchored")?;
            if config.network == Network::Bitcoin {
                info!(
                    "anchor: embedded checkpoint spine trusted up to height {tip}; syncing the tail above it"
                );
            } else {
                info!("anchor: genesis; validating the whole chain from block 1");
            }
        } else if let Some(tip) = core.tip_height()? {
            info!("resume: store already anchored at tip {tip}; continuing above it");
        }
        let peer_cache = PeerCache::load(&data_dir);
        Ok(Self {
            core,
            config,
            peer_cache,
        })
    }

    /// The chain store, for reads an embedder wants to do directly.
    pub fn core(&self) -> &Core {
        &self.core
    }

    /// The configuration the node was opened with.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Adopt the newest embedded checkpoint the store holds, sync the Bitcoin
    /// tail, then run the pipelined IBD pass: connect an RSK peer, scan
    /// coinbases backwards, fetch and score the headers each tag names as it
    /// arrives, and stop at the first final root. Header sync overlaps
    /// validation across the configured peers.
    pub fn sync(&self) -> Result<SyncReport> {
        let from = resume_from(&self.core)?;
        info!("syncing from height {from} …");

        let mut addrs = self.peer_cache.btc();
        addrs.extend(self.config.addrs.iter().copied());
        let (candidates, failed) = peers::candidates(addrs, &self.config.seeds);
        if !failed.is_empty() {
            warn!("seed(s) failed to resolve: {}", failed.join(", "));
        }
        if candidates.is_empty() {
            bail!("no candidate peers: every DNS seed failed to resolve");
        }
        let mut candidates: VecDeque<_> = candidates.into_iter().collect();
        let mut report = sync::run(
            &self.core,
            &self.config,
            &mut candidates,
            Some(&self.peer_cache),
        )?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        match runtime.block_on(crate::ibd::run(
            &self.core,
            &self.config,
            Some(&self.peer_cache),
        )) {
            Ok(ibd) => {
                report.coinbase = ibd.coinbase.clone();
                let final_note = match ibd.final_root {
                    Some(root) => format!(
                        " — FINAL RSK header #{} 0x{}",
                        root.number,
                        hex::encode(root.root)
                    ),
                    None => String::new(),
                };
                info!(
                    "ibd: {} tag(s) of {} needed, {} headers, {} roots, best {} endorse ({} required), {:.1} MB downloaded{final_note}",
                    ibd.tags,
                    ibd.needed,
                    ibd.headers,
                    ibd.roots,
                    ibd.endorse,
                    ibd.required,
                    ibd.downloaded_megabytes()
                );
                report.ibd = Some(ibd);
            }
            Err(e) => warn!("ibd: {e:#}"),
        }
        Ok(report)
    }

    /// The **checkpoints** — period-boundary headers at or below the finality
    /// mark — as `checkpoints.txt` text (one hex header per line, line `i` at
    /// height `i * DIFFICULTY_PERIOD`).
    ///
    /// Only boundaries the mark has reached are written: a checkpoint is a
    /// period start *and* finalized, so these are safe to treat as
    /// irreversible. The tip's nearest boundary is deliberately excluded.
    pub fn export_checkpoints(&self) -> Result<String> {
        let finalized = self
            .core
            .finalized_height()?
            .context("store is not seeded")?;
        let checkpoints: Vec<_> = self
            .core
            .finalized_checkpoints()?
            .collect::<Result<Vec<_>, _>>()?;
        if checkpoints.is_empty() {
            bail!("no checkpoints at or below the finality mark {finalized}");
        }

        let mut text = String::new();
        for (_, header) in &checkpoints {
            let mut bytes = Vec::with_capacity(80);
            header
                .consensus_encode(&mut bytes)
                .expect("encoding to vec is infallible");
            text.push_str(&hex::encode(&bytes));
            text.push('\n');
        }
        Ok(text)
    }
}

/// Adopt the newest embedded checkpoint the store can prove it holds, and
/// return the first height to download. A store anchored by an older, shorter
/// spine benefits from a newer one instead of staying stuck on its old mark.
fn resume_from(core: &Core) -> Result<u64> {
    let tip = core.tip_height()?.context("the store is not anchored")?;
    if let Some((from, to)) = core.reanchor_shipped()? {
        info!(
            "finality: adopted shipped checkpoint #{to} (was #{from}); \
             the next bar is measured from it"
        );
    }
    Ok(tip + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_a_fresh_mainnet_store_trusts_the_whole_spine() {
        let dir = tempfile::tempdir().unwrap();
        let node = Node::open(Config {
            data_dir: dir.path().to_path_buf(),
            ..Config::default()
        })
        .unwrap();
        let spine = rsk_core::btc::checkpoints::parse().unwrap();
        assert_eq!(
            node.core().tip_height().unwrap(),
            Some(spine.last().unwrap().0)
        );
        assert_eq!(
            resume_from(node.core()).unwrap(),
            spine.last().unwrap().0 + 1
        );
    }

    #[test]
    fn opening_a_fresh_non_mainnet_store_starts_from_genesis() {
        let dir = tempfile::tempdir().unwrap();
        let node = Node::open(Config {
            data_dir: dir.path().to_path_buf(),
            network: Network::Regtest,
            ..Config::default()
        })
        .unwrap();
        assert_eq!(node.core().tip_height().unwrap(), Some(0));
        assert_eq!(resume_from(node.core()).unwrap(), 1);
    }

    #[test]
    fn reopening_an_anchored_store_resumes_above_its_tip() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            data_dir: dir.path().to_path_buf(),
            ..Config::default()
        };
        let tip = {
            let node = Node::open(config.clone()).unwrap();
            node.core().tip_height().unwrap().unwrap()
        };
        let node = Node::open(config).unwrap();
        assert_eq!(resume_from(node.core()).unwrap(), tip + 1);
    }
}
