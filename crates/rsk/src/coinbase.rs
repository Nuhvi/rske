//! Coinbase sync: fetch the canonical coinbase and its inclusion proof for the
//! retained window `[finalized checkpoint, tip]`.
//!
//! The primary path is BIP37. BIP34 puts the block height in the coinbase
//! scriptSig as a data element; a bloom filter containing just that element
//! makes `getdata MSG_FILTERED_BLOCK` return the coinbase and a partial merkle
//! tree (~700 bytes) instead of the whole block (~740 KB). A peer that does not
//! serve BIP37 (or withholds the match) is dropped from the pool; if no bloom
//! peer is left, the same loop falls back to full blocks, which every
//! `NODE_NETWORK` peer serves.
//!
//! Everything is verified against the locally validated header before it is
//! stored: [`rsk_core::Core::store_coinbase`] re-checks the merkle proof, the
//! coinbase index, and the canonical block hash. A peer can omit data, but it
//! cannot make us store a coinbase that is not in the canonical block.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bitcoin::block::BlockHash;
use bitcoin::merkle_tree::MerkleBlock;
use bitcoin::p2p::ServiceFlags;
use bitcoin::p2p::message_bloom::BloomFlags;
use btc_p2p::BloomFilter;
use btc_p2p::session::Session;
use rsk_core::{Core, bip34_height};
use tracing::{info, warn};

use crate::config::Config;
use crate::peers;

/// BIP37 filter shape. One element, 100 bytes and five hash functions gives a
/// false-positive rate around 1e-11 per transaction, so a block normally
/// returns exactly the coinbase.
const FILTER_BYTES: usize = 100;
const FILTER_HASH_FUNCS: u32 = 5;

/// How long a coinbase peer may take to answer one request.
const PEER_TIMEOUT: Duration = Duration::from_secs(20);

/// What one coinbase sync did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CoinbaseReport {
    /// First height the retained window asked for, if the store has a tip.
    pub start: Option<u64>,
    /// Canonical tip at the end of the run.
    pub tip: Option<u64>,
    /// Coinbases written this run.
    pub stored: usize,
    /// Blocks that could not be fetched from any peer (retried next run).
    pub failed: usize,
    /// Coinbases already present and matching the canonical chain.
    pub cached: usize,
}

impl CoinbaseReport {
    fn is_complete(&self) -> bool {
        self.failed == 0
    }
}

/// Fetch and store every missing canonical coinbase in the retained window.
///
/// Missing means "no row, or a row whose block hash no longer matches the
/// canonical header". The window is recomputed from the store, so a resumed
/// run picks up where the last one stopped and a new finalized period prunes
/// the old rows before they are considered.
pub fn sync(core: &Core, config: &Config) -> Result<CoinbaseReport> {
    // Apply the retention rule up front too: a resumed store may have rows from
    // a period that became superseded while the process was stopped.
    core.prune_coinbases()?;

    let Some(start) = core.coinbase_start_height()? else {
        return Ok(CoinbaseReport::default());
    };
    let Some(tip) = core.tip_height()? else {
        return Ok(CoinbaseReport::default());
    };
    if start > tip {
        return Ok(CoinbaseReport {
            start: Some(start),
            tip: Some(tip),
            ..CoinbaseReport::default()
        });
    }

    let mut missing = Vec::new();
    let mut cached = 0usize;
    for height in start..=tip {
        let header = core
            .header_at(height)?
            .with_context(|| format!("no canonical header at {height}"))?;
        let wanted = header.block_hash();
        match core.coinbase_at(height)? {
            Some(coinbase) if coinbase.block_hash == wanted => cached += 1,
            _ => missing.push((height, wanted)),
        }
    }

    info!(
        "coinbase: window #{start}..#{tip} ({} block(s)), {cached} cached, {} to fetch",
        tip - start + 1,
        missing.len()
    );
    if missing.is_empty() {
        return Ok(CoinbaseReport {
            start: Some(start),
            tip: Some(tip),
            cached,
            ..CoinbaseReport::default()
        });
    }

    let (candidates, seed_failures) = peers::candidates(config.addrs.clone(), &config.seeds);
    for failure in &seed_failures {
        warn!("coinbase: seed failed: {failure}");
    }
    if candidates.is_empty() {
        warn!("coinbase: no candidate peers; will retry next sync");
        return Ok(CoinbaseReport {
            start: Some(start),
            tip: Some(tip),
            failed: missing.len(),
            cached,
            ..CoinbaseReport::default()
        });
    }

    let mut pool = Pool::new(candidates, config, tip);
    let mut stored = 0usize;
    let mut failed = 0usize;
    let total = missing.len();

    for (done, (height, hash)) in missing.iter().enumerate() {
        match pool.fetch(core, *height, *hash) {
            Ok(()) => stored += 1,
            Err(e) => {
                failed += 1;
                warn!("coinbase #{height}: {e:#}");
            }
        }
        if (done + 1) % 100 == 0 || done + 1 == total {
            info!(
                "coinbase: {}/{} fetched, {failed} failed (height #{height})",
                done + 1,
                total
            );
        }
    }

    let report = CoinbaseReport {
        start: Some(start),
        tip: Some(tip),
        stored,
        failed,
        cached,
    };
    if report.is_complete() {
        info!(
            "coinbase: complete — {stored} stored, {cached} already cached, window #{start}..#{tip}"
        );
    } else {
        warn!("coinbase: {failed} block(s) missing after the run; a later sync will retry them");
    }
    Ok(report)
}

/// One connected peer, classified by what it can serve.
struct Peer {
    addr: SocketAddr,
    session: Session<std::net::TcpStream>,
    /// The peer advertises BIP37 and is still trusted to answer it.
    bloom: bool,
    /// The peer advertises `NODE_NETWORK`, so full blocks are an option.
    full: bool,
}

/// A small, self-refilling peer pool. Sequential by design: one request at a
/// time is enough for a window of a few thousand blocks, and keeps the code
/// (and the peer's filter state) simple.
struct Pool {
    peers: Vec<Peer>,
    candidates: VecDeque<SocketAddr>,
    user_agent: String,
    start_height: i32,
    want: usize,
    max_attempts: usize,
    attempts: usize,
    cursor: usize,
}

impl Pool {
    fn new(candidates: Vec<SocketAddr>, config: &Config, tip: u64) -> Self {
        Self {
            peers: Vec::new(),
            candidates: candidates.into(),
            user_agent: config.user_agent.clone(),
            start_height: tip.min(i32::MAX as u64) as i32,
            want: config.peers.max(1),
            // BIP37 is a minority service, so try harder than the header pool
            // before concluding there is no bloom peer.
            max_attempts: config.limit.max(32),
            attempts: 0,
            cursor: 0,
        }
    }

    /// Fill toward `want` bloom peers and at least one full-history peer.
    fn fill(&mut self) {
        while self.attempts < self.max_attempts {
            let bloom_count = self.peers.iter().filter(|p| p.bloom).count();
            let full_count = self.peers.iter().filter(|p| p.full).count();
            if bloom_count >= self.want && full_count >= 1 {
                break;
            }
            let Some(addr) = self.candidates.pop_front() else {
                break;
            };
            self.attempts += 1;

            let services = ServiceFlags::BLOOM | ServiceFlags::WITNESS;
            match Session::connect_with_services(
                addr,
                &self.user_agent,
                self.start_height,
                services,
            ) {
                Ok((mut session, handshake)) => {
                    let bloom = handshake.services.has(ServiceFlags::BLOOM);
                    let full = handshake.services.has(ServiceFlags::NETWORK);
                    if !bloom && !full {
                        continue;
                    }
                    if !bloom && self.peers.iter().filter(|p| p.full).count() >= self.want {
                        continue;
                    }
                    if bloom && self.peers.iter().filter(|p| p.bloom).count() >= self.want {
                        // Keep it only if it can also serve full blocks and we
                        // do not have a fallback yet.
                        if !full || self.peers.iter().any(|p| p.full) {
                            continue;
                        }
                    }
                    if session.set_read_timeout(Some(PEER_TIMEOUT)).is_err() {
                        continue;
                    }
                    info!(
                        "coinbase peer {addr}: {} (bloom={bloom}, full={full})",
                        handshake.user_agent
                    );
                    self.peers.push(Peer {
                        addr,
                        session,
                        bloom,
                        full,
                    });
                }
                Err(e) => {
                    warn!("coinbase peer {addr} failed: {e:#}");
                }
            }
        }
    }

    fn bloom_peers(&self) -> usize {
        self.peers.iter().filter(|p| p.bloom).count()
    }

    /// Fetch one coinbase, trying bloom peers first and full blocks second.
    fn fetch(&mut self, core: &Core, height: u64, hash: BlockHash) -> Result<()> {
        if self.peers.is_empty() {
            self.fill();
        }
        if self.peers.is_empty() {
            bail!("no coinbase-capable peer");
        }

        // Try every bloom peer once, starting where the last success left off.
        // Heights <= 16 have no BIP34 data push to filter on; use a full block.
        let n = self.peers.len();
        for step in 0..n {
            let index = (self.cursor + step) % n;
            if !self.peers[index].bloom || height <= 16 {
                continue;
            }
            match fetch_bloom(core, &mut self.peers[index].session, height, hash) {
                Ok(()) => {
                    self.cursor = (index + 1) % n;
                    return Ok(());
                }
                Err(e) => {
                    warn!(
                        "coinbase #{height}: bloom peer {} failed: {e:#}",
                        self.peers[index].addr
                    );
                    self.peers[index].bloom = false;
                }
            }
        }

        // No bloom peer could do it: fall back to a full block.
        let n = self.peers.len();
        for step in 0..n {
            let index = (self.cursor + step) % n;
            if !self.peers[index].full {
                continue;
            }
            match fetch_full(core, &mut self.peers[index].session, height, hash) {
                Ok(()) => {
                    self.cursor = (index + 1) % n;
                    return Ok(());
                }
                Err(e) => {
                    warn!(
                        "coinbase #{height}: full peer {} failed: {e:#}",
                        self.peers[index].addr
                    );
                    self.peers[index].full = false;
                }
            }
        }

        // Everything we had is gone. Try a fresh batch once; if that also
        // yields nothing, the caller records the block as failed.
        self.peers.retain(|p| p.bloom || p.full);
        if self.peers.is_empty() {
            self.fill();
            if self.peers.is_empty() {
                bail!("all coinbase peers failed and no replacements are available");
            }
            return self.fetch(core, height, hash);
        }
        bail!(
            "no peer could fetch the coinbase (bloom peers left: {})",
            self.bloom_peers()
        )
    }
}

fn fetch_bloom(
    core: &Core,
    session: &mut Session<std::net::TcpStream>,
    height: u64,
    hash: BlockHash,
) -> Result<()> {
    let element = bip34_height(height);
    let filter = BloomFilter::with_elements([element], FILTER_BYTES, FILTER_HASH_FUNCS, 0)?
        .to_filter_load(BloomFlags::None);
    let filtered = session.get_filtered_block(hash, filter)?;
    let coinbase = filtered
        .transactions
        .iter()
        .find(|tx| tx.is_coinbase())
        .context("filtered block carried no coinbase")?;
    core.store_coinbase(height, hash, coinbase, &filtered.merkle_block)?;
    Ok(())
}

fn fetch_full(
    core: &Core,
    session: &mut Session<std::net::TcpStream>,
    height: u64,
    hash: BlockHash,
) -> Result<()> {
    let block = session.get_block(hash)?;
    if block.header.block_hash() != hash {
        bail!(
            "peer returned block {} for {hash}",
            block.header.block_hash()
        );
    }
    let merkle_root = block
        .compute_merkle_root()
        .context("block has no transactions")?;
    if merkle_root != block.header.merkle_root {
        bail!("block merkle root does not match its header");
    }
    let coinbase = block.txdata.first().context("block has no coinbase")?;
    let proof =
        MerkleBlock::from_block_with_predicate(&block, |txid| *txid == coinbase.compute_txid());
    core.store_coinbase(height, hash, coinbase, &proof)?;
    Ok(())
}
