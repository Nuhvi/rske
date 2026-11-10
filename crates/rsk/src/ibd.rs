//! Initial block download: find the newest RSK header a Bitcoin tag window
//! agrees on.
//!
//! The RSK peer connects before the coinbase scan starts. As the scan walks
//! back, each tag's `BN` (and its oldest CPV checkpoint, if any) is fetched
//! from that peer, appended to the in-memory forest, and the roots are re-scored
//! immediately. The scan stops at the first root whose component carries at
//! least `(1 - honest_share) * needed` endorsements; tags never read are counted
//! as worst-case dissent, so stopping before `needed` tags is safe as long as
//! the endorsement bar is met.
//!
//! Coinbase fetching is blocking and RSK fetching is async, so this is the one
//! place the two are interleaved. The pure scoring path (`score`/`assess`) stays
//! testable without a peer.

use std::time::{Duration, Instant};

use alloy_primitives::B256;
use anyhow::{Context, Result, bail};
use futures::stream::{FuturesUnordered, StreamExt};
#[cfg(test)]
use rsk_core::score;
use rsk_core::{
    Core, FIVE_SIGMA_ALPHA, HeaderRef, RootScore, RskEntry, Scoreboard, Share, Tag,
    min_tags_for_fraud_proof, oldest_checkpoint, required_endorsements,
};
use tracing::{info, warn};

use crate::coinbase::CoinbaseReport;
use crate::config::Config;
use crate::peers::PeerCache;
use crate::rootstock::{Chain, Session};

/// Hard cap on headers buffered for one IBD pass. A window spans a few thousand
/// RSK blocks; 20,000 is a safety valve, not a working limit.
const MAX_HEADERS: usize = 20_000;

/// What one IBD pass found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IbdReport {
    /// Bitcoin tip the window was taken from.
    pub tip: Option<u64>,
    /// Heights visited by the coinbase scan, including failed fetches.
    pub blocks: u64,
    /// Tags found in that scan.
    pub tags: usize,
    /// Tags the configured honest share asks for at 5σ.
    pub needed: u64,
    /// Endorsements a root must carry to be FINAL: the larger of
    /// `(1 - honest_share) * needed` and the 100-block work floor.
    pub required: u64,
    /// RSK headers fetched and scored.
    pub headers: usize,
    /// Distinct roots in the staged forest.
    pub roots: usize,
    /// Best root's endorsement count.
    pub endorse: usize,
    /// Best root's dissent count.
    pub dissent: usize,
    /// The root that cleared `(1 - honest_share) * needed`, if exactly one did.
    pub final_root: Option<RootScore>,
    /// More than one root cleared the bar; no unique winner.
    pub contested: bool,
    /// Coinbase scan counters for the same pass.
    pub coinbase: CoinbaseReport,
    /// RSK header wire bytes downloaded for this pass.
    pub header_bytes: u64,
}

impl IbdReport {
    /// Total wire payload bytes downloaded for this IBD pass.
    pub fn downloaded_bytes(&self) -> u64 {
        self.coinbase.bytes + self.header_bytes
    }

    /// The same total in decimal megabytes, for progress output.
    pub fn downloaded_megabytes(&self) -> f64 {
        self.downloaded_bytes() as f64 / 1_000_000.0
    }
}

/// The tag count the configured honest share asks for at 5σ.
pub fn needed(config: &Config) -> Result<u64> {
    let share = Share::new(config.honest_share).context("honest_share must be in (0, 1)")?;
    Ok(min_tags_for_fraud_proof(share, FIVE_SIGMA_ALPHA))
}

/// The best root when it carries at least `required` endorsements.
///
/// `required` already includes the 100-block work floor; see
/// [`required_endorsements`]. Tags never read are worst-case dissent, so a scan
/// that stops before collecting `needed` tags can still satisfy this bar.
/// The unique root that clears `required`, or `None` plus a contest flag when
/// more than one does.
fn select_final(roots: &[RootScore], required: u64) -> (Option<RootScore>, bool) {
    let mut winners = roots
        .iter()
        .filter(|root| root.endorse as u64 >= required)
        .copied();
    let first = winners.next();
    let contested = first.is_some() && winners.next().is_some();
    (if contested { None } else { first }, contested)
}

#[cfg(test)]
pub fn assess(headers: &[HeaderRef], tags: &[Tag], required: u64) -> Option<RootScore> {
    if tags.is_empty() || required == 0 {
        return None;
    }
    select_final(&score(headers, tags), required).0
}

/// The lowest RSK height a tag needs: its own `BN`, or the oldest CPV
/// checkpoint when the tag can only endorse softly.
fn tag_target(tag: &Tag) -> u64 {
    let bn = u64::from(tag.bn);
    oldest_checkpoint(tag.bn).map_or(bn, |checkpoint| checkpoint.min(bn))
}

/// An open RSK session plus the contiguous header range fetched so far.
///
/// Headers are always fetched backwards from the peer tip, so appending a
/// newer batch below the current range keeps the in-memory forest
/// parent-first and contiguous.
struct RskSource {
    session: Session,
    /// The peer's best hash at connect time; the top of the forest.
    peer_tip: B256,
    /// The peer's best height at connect time.
    peer_number: u64,
    /// How many headers to ask for in the next `headers` request.
    batch: u32,
    /// The hash and number the last IBD pass committed, if any.
    stored_tip: Option<(B256, u64)>,
    /// Hash to request next: the parent of the oldest header fetched.
    cursor: B256,
    /// Oldest-first, so `entries[0]` is the forest root.
    entries: Vec<RskEntry>,
    lowest: Option<u64>,
    header_bytes: u64,
    capped: bool,
    /// Whether this pass added headers the store does not already have.
    dirty: bool,
    last_progress: Instant,
}

impl RskSource {
    fn new(core: &Core, session: Session) -> Result<Self> {
        let peer_tip = session.status().best_block_hash;
        Ok(Self {
            cursor: peer_tip,
            peer_tip,
            peer_number: session.status().best_block_number,
            batch: 192,
            stored_tip: core.rsk_sync_tip()?,
            session,
            entries: Vec::new(),
            lowest: None,
            header_bytes: 0,
            capped: false,
            dirty: false,
            last_progress: Instant::now(),
        })
    }

    /// Ensure every height down to `target_bn` is staged.
    async fn ensure(&mut self, core: &Core, target_bn: u64) -> Result<()> {
        if self.capped {
            return Ok(());
        }
        if self.entries.is_empty() {
            self.load_from_store(core, target_bn).await?;
        }
        if self.entries.is_empty() && !self.capped {
            self.fetch_batch().await?;
        }
        while !self.capped && self.lowest.is_some_and(|lowest| target_bn < lowest) {
            if self.load_stored_gap(core, target_bn)? {
                continue;
            }
            self.fetch_batch().await?;
        }
        Ok(())
    }

    /// Try to satisfy the range from the store before touching the network.
    ///
    /// A matching peer tip is reused directly. If the peer has advanced, the
    /// stored tip is used as the seam: fetch only the new headers above it and
    /// splice them onto the stored range below.
    async fn load_from_store(&mut self, core: &Core, target_bn: u64) -> Result<()> {
        let direct = core.rsk_headers_back(self.peer_tip, target_bn)?;
        if !direct.is_empty() {
            self.adopt(direct);
            info!(
                "ibd: reusing {} stored header(s) back to #{}",
                self.entries.len(),
                self.lowest.unwrap_or(0)
            );
            return Ok(());
        }

        let Some((stored_hash, stored_number)) = self.stored_tip else {
            return Ok(());
        };
        let stored = core.rsk_headers_back(stored_hash, target_bn)?;
        if stored.is_empty() {
            return Ok(());
        }

        self.dirty = true;
        // The delta above the stored tip is usually a handful of headers; asking
        // for a full 192-header batch would download the rest for nothing.
        let delta = self
            .peer_number
            .saturating_sub(stored_number)
            .saturating_add(1);
        self.batch = (delta.min(192) as u32).max(1);
        while !self.capped && self.lowest.is_none_or(|lowest| stored_number < lowest) {
            self.fetch_batch().await?;
        }
        self.batch = 192;
        let Some(seam) = self
            .entries
            .iter()
            .position(|entry| entry.header.hash() == stored_hash)
        else {
            // The peer's chain does not include our stored tip (or the batch
            // never reached it). The stored range may still cover every tag
            // we are about to read, so use it rather than refetching the whole
            // range; tags above it simply stay unresolved for this pass.
            info!("ibd: stored RSK tip is not on this peer's chain; using the stored range");
            self.dirty = false;
            self.adopt(stored);
            return Ok(());
        };

        // `entries` is oldest-first, so everything after the seam is newer
        // than the stored tip. The stored range is oldest-first too, so it must
        // come first and the new headers above it are appended.
        let newer = self.entries[seam + 1..].to_vec();
        let fetched = newer.len();
        let mut combined = stored;
        combined.extend(newer);
        if fetched == 0 {
            self.dirty = false;
        }
        self.adopt(combined);
        info!(
            "ibd: fetched {fetched} new header(s); reused {} stored header(s) back to #{}",
            self.entries.len() - fetched,
            self.lowest.unwrap_or(0)
        );
        Ok(())
    }

    /// Extend the range from the stored suffix before using the network.
    ///
    /// `cursor` is the parent of the current oldest header, so this reads only
    /// the gap the next tag needs rather than replaying the whole stored range.
    fn load_stored_gap(&mut self, core: &Core, target_bn: u64) -> Result<bool> {
        let gap = core.rsk_headers_back(self.cursor, target_bn)?;
        if gap.is_empty() {
            return Ok(false);
        }
        let mut combined = gap;
        combined.append(&mut self.entries);
        self.adopt(combined);
        Ok(true)
    }

    fn adopt(&mut self, entries: Vec<RskEntry>) {
        self.lowest = entries.first().map(|entry| entry.header.number);
        self.cursor = entries
            .first()
            .map(|entry| entry.header.parent_hash)
            .unwrap_or(self.peer_tip);
        self.entries = entries;
    }

    async fn fetch_batch(&mut self) -> Result<()> {
        let batch = self.session.headers(self.cursor, self.batch).await?;
        if batch.is_empty() {
            bail!("peer returned no headers");
        }
        let bytes: usize = batch.iter().map(|header| header.raw.len()).sum();
        self.header_bytes += bytes as u64;
        let oldest = batch.last().context("non-empty batch")?;
        self.cursor = oldest.header.parent_hash;

        let mut ascending = batch;
        ascending.reverse();
        let mut fetched: Vec<RskEntry> = ascending
            .into_iter()
            .map(|header| RskEntry {
                header: header.header,
                raw: header.raw.to_vec(),
                uncles: Vec::new(),
            })
            .collect();
        fetched.append(&mut self.entries);
        self.entries = fetched;
        self.lowest = self.entries.first().map(|entry| entry.header.number);
        self.dirty = true;

        if self.entries.len() > MAX_HEADERS {
            self.capped = true;
            warn!("ibd: header buffer hit {MAX_HEADERS}; the window may be incomplete");
        }
        if self.last_progress.elapsed() >= Duration::from_secs(5) {
            info!(
                "ibd: fetched {} header(s) back to #{}",
                self.entries.len(),
                self.lowest.unwrap_or(0)
            );
            self.last_progress = Instant::now();
        }
        Ok(())
    }

    fn refs(&self) -> Vec<HeaderRef> {
        refs(&self.entries)
    }
}

/// Connect to the first RSK peer that answers.
///
/// Bootnodes and configured peers are independent, and a dead discovery
/// endpoint costs its full retry budget. Racing them means the scan starts
/// from the first responsive node instead of waiting out every earlier failure.
async fn connect_any(peers: &[String], chain: Chain) -> Result<(String, Session)> {
    let mut pending = FuturesUnordered::new();
    for peer in peers {
        let peer = peer.clone();
        pending.push(async move {
            let session = Session::connect(&peer, chain)
                .await
                .with_context(|| format!("connecting {peer}"))?;
            Ok::<_, anyhow::Error>((peer, session))
        });
    }
    while let Some(result) = pending.next().await {
        match result {
            Ok(connected) => return Ok(connected),
            Err(e) => warn!("ibd: RSK peer failed: {e:#}"),
        }
    }
    bail!("no RSK peer connected")
}

/// Open the RSK side of the pipeline before the coinbase scan starts.
async fn connect_source(
    core: &Core,
    config: &Config,
    cache: Option<&PeerCache>,
) -> Option<RskSource> {
    let Some(chain) = Chain::for_network(config.network) else {
        warn!(
            "ibd: {} has no Rootstock chain; tags collected but not scored",
            config.network
        );
        return None;
    };
    if let Some(cache) = cache {
        let cached = cache.rsk();
        let (ips, hosts): (Vec<String>, Vec<String>) = cached
            .into_iter()
            .partition(|peer| peer.parse::<std::net::SocketAddr>().is_ok());
        if !ips.is_empty() {
            info!("ibd: trying {} cached RSK address(es) first", ips.len());
            match tokio::time::timeout(Duration::from_secs(3), connect_any(&ips, chain)).await {
                Ok(Ok((peer, session))) => {
                    let addr = session.addr();
                    cache.record_rsk(&addr.to_string());
                    info!("ibd: RSK peer {peer} connected (cached as {addr})");
                    return RskSource::new(core, session).ok();
                }
                Ok(Err(e)) => warn!("ibd: cached RSK address(es) failed: {e:#}"),
                Err(_) => warn!("ibd: cached RSK address(es) timed out"),
            }
        }
        if !hosts.is_empty() {
            info!("ibd: trying {} cached RSK hostname(s)", hosts.len());
            match tokio::time::timeout(Duration::from_secs(3), connect_any(&hosts, chain)).await {
                Ok(Ok((peer, session))) => {
                    let addr = session.addr();
                    cache.record_rsk(&addr.to_string());
                    info!("ibd: RSK peer {peer} connected (cached as {addr})");
                    return RskSource::new(core, session).ok();
                }
                Ok(Err(e)) => warn!("ibd: cached RSK hostname(s) failed: {e:#}"),
                Err(_) => warn!("ibd: cached RSK hostname(s) timed out; falling back"),
            }
        }
    }
    let (peers, shipped) = if config.rsk_peers.is_empty() {
        (chain.bootnodes(), true)
    } else {
        (config.rsk_peers.clone(), false)
    };
    if peers.is_empty() {
        warn!("ibd: no RSK peer configured or shipped; tags collected but not scored");
        return None;
    }
    if shipped {
        info!(
            "ibd: racing {} shipped bootnode(s); no --rsk-peer configured",
            peers.len()
        );
    } else {
        info!("ibd: connecting to {} configured RSK peer(s)", peers.len());
    }
    match connect_any(&peers, chain).await {
        Ok((peer, session)) => {
            let addr = session.addr();
            info!("ibd: RSK peer {peer} connected ({addr})");
            if let Some(cache) = cache {
                cache.record_rsk(&addr.to_string());
            }
            match RskSource::new(core, session) {
                Ok(source) => Some(source),
                Err(e) => {
                    warn!("ibd: could not read the stored RSK tip: {e:#}");
                    None
                }
            }
        }
        Err(e) => {
            warn!("ibd: no RSK peer connected: {e:#}; tags collected but not scored");
            None
        }
    }
}

/// Store the fetched headers as a forest rooted at the oldest one.
///
/// The root is proposed (its Bitcoin anchor must be in the finalized tail); the
/// rest are connected in ascending order, which validates each header against
/// the parent stored just before it.
fn commit(core: &Core, entries: &[RskEntry]) -> Result<()> {
    let root = entries.first().context("no RSK headers to commit")?;
    core.propose_checkpoint(root)
        .with_context(|| format!("rooting the IBD header at #{}", root.header.number))?;
    if entries.len() > 1 {
        core.connect_rsk_headers_without_uncles(&entries[1..])
            .with_context(|| format!("connecting {} RSK headers", entries.len() - 1))?;
    }
    Ok(())
}

/// Project fetched entries into the pure scoring view.
fn refs(entries: &[RskEntry]) -> Vec<HeaderRef> {
    entries
        .iter()
        .map(|entry| HeaderRef {
            number: entry.header.number,
            hash: entry.header.hash().0,
            merged_mining_hash: entry.header.hash_for_merged_mining().0,
            parent: entry.header.parent_hash.0,
            bitcoin_lsb: merged_mining_lsb(&entry.header),
        })
        .collect()
}

/// The byte a CPV commits to: the LSB of the header's Bitcoin block hash.
fn merged_mining_lsb(header: &rsk_consensus::Header) -> Option<u8> {
    use bitcoin::consensus::Decodable;
    use bitcoin::hashes::Hash;

    let raw = header.bitcoin_merged_mining_header.as_ref()?;
    let mut reader = &raw[..];
    let btc = bitcoin::block::Header::consensus_decode(&mut reader).ok()?;
    Some(btc.block_hash().to_byte_array()[0])
}

/// One IBD pass: scan coinbases backwards, fetch the RSK headers each tag
/// names, and re-score after every update until a root clears `(1 - H)·N`.
///
/// The RSK peer connects before the first coinbase is fetched, and the
/// coinbase/RSK loops are interleaved: a tag is staged and scored as soon as it
/// is read, rather than after the whole tag window has been downloaded.
pub async fn run(core: &Core, config: &Config, cache: Option<&PeerCache>) -> Result<IbdReport> {
    let needed = needed(config)?;
    let share = Share::new(config.honest_share).context("honest_share must be in (0, 1)")?;
    let required = required_endorsements(share, needed);
    let Some(mut scanner) =
        crate::coinbase::Scanner::new_with_cache(core, config, config.ibd_max_blocks, cache)?
    else {
        return Ok(IbdReport::default());
    };
    let mut report = IbdReport {
        tip: core.tip_height()?,
        needed,
        required,
        ..IbdReport::default()
    };
    info!(
        "ibd: scanning coinbases for {needed} tag(s) at {:.0}% honest",
        config.honest_share * 100.0
    );

    let mut source = connect_source(core, config, cache).await;
    let mut tags = Vec::new();
    let mut roots = Vec::new();
    let mut final_root = None;
    let mut board: Option<Scoreboard> = None;
    let mut board_headers = 0usize;
    let mut last_log = Instant::now();

    while let Some((_height, tag)) = scanner.next(core)? {
        let mut changed = false;
        if let Some(tag) = tag {
            if let Some(source) = source.as_mut()
                && let Err(e) = source.ensure(core, tag_target(&tag)).await
            {
                warn!("ibd: fetching headers for tag BN {}: {e:#}", tag.bn);
            }
            tags.push(tag);
            changed = true;
        }

        if changed {
            if let Some(source) = source.as_ref() {
                if board.is_none() || source.entries.len() != board_headers {
                    board = Some(Scoreboard::with_tags(source.refs(), &tags));
                    board_headers = source.entries.len();
                } else if let Some(board) = board.as_mut()
                    && let Some(tag) = tags.last()
                {
                    board.add_tag(tag);
                }
                if let Some(board) = board.as_ref() {
                    roots = board.roots();
                    let (winner, contested) = select_final(&roots, required);
                    report.contested = contested;
                    final_root = winner;
                }
            }
            if final_root.is_some() || report.contested {
                break;
            }
            if source.is_none() && tags.len() as u64 >= needed {
                break;
            }
        }
        if last_log.elapsed() >= Duration::from_secs(5) {
            let best = roots.first();
            info!(
                "ibd: {} tag(s), {} header(s), best {}/{} endorse",
                tags.len(),
                source.as_ref().map_or(0, |source| source.entries.len()),
                best.map_or(0, |root| root.endorse),
                needed
            );
            last_log = Instant::now();
        }
    }

    report.blocks = scanner.blocks();
    report.tags = tags.len();
    report.coinbase = scanner.into_report();
    info!(
        "coinbase: stopped at #{} after {} block(s), {} tag(s), {} stored, {} cached, {} failed",
        report.coinbase.start.unwrap_or(0),
        report.blocks,
        report.tags,
        report.coinbase.stored,
        report.coinbase.cached,
        report.coinbase.failed
    );

    if let Some(source) = source.as_mut() {
        report.headers = source.entries.len();
        report.header_bytes = source.header_bytes;
        if source.dirty && !source.entries.is_empty() {
            match commit(core, &source.entries) {
                Ok(()) => {
                    if let Some(tip) = source.entries.last()
                        && let Err(e) = core.set_rsk_sync_tip(tip.header.hash())
                    {
                        warn!("ibd: recording the RSK sync tip failed: {e:#}");
                    }
                }
                Err(e) => {
                    // The in-memory score is still meaningful: it is exactly
                    // the fraud proof, and the headers were already committed
                    // to by the peer.
                    warn!("ibd: storing the RSK headers failed: {e:#}");
                }
            }
        }
        board = Some(Scoreboard::with_tags(source.refs(), &tags));
        roots = board.as_ref().map(Scoreboard::roots).unwrap_or_default();
        let (winner, contested) = select_final(&roots, required);
        report.contested = contested;
        final_root = winner;
    }

    report.roots = roots.len();
    if let Some(best) = roots.first() {
        report.endorse = best.endorse;
        report.dissent = best.dissent;
    }
    report.final_root = final_root;

    let statistical = ((1.0 - config.honest_share) * needed as f64).ceil() as u64;
    if report.contested {
        warn!(
            "ibd: CONTESTED — more than one root cleared {required} endorsements; no unique FINAL root"
        );
    }
    match report.final_root {
        Some(root) => info!(
            "ibd: FINAL RSK header #{} 0x{} with {}/{} endorse ({required} required: {statistical} statistical, 100-block work floor), {} root(s), {:.1} MB downloaded ({:.2} MB coinbase, {:.2} MB RSK headers)",
            root.number,
            hex::encode(root.root),
            root.endorse,
            needed,
            report.roots,
            report.downloaded_megabytes(),
            report.coinbase.bytes as f64 / 1_000_000.0,
            report.header_bytes as f64 / 1_000_000.0
        ),
        None => warn!(
            "ibd: no FINAL root: best {}/{} endorse, {required} required ({statistical} statistical, 100-block work floor; {needed} tags in the sample), {} root(s)",
            report.endorse, report.tags, report.roots
        ),
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use rsk_core::{HeaderRef, Tag};

    use rsk_core::RootScore;

    use super::{assess, select_final, tag_target};

    /// A tag with the given block number whose prefix matches `hash`.
    fn tag(bn: u32, hash: &[u8; 32]) -> Tag {
        let mut prefix = [0u8; 20];
        prefix.copy_from_slice(&hash[..20]);
        Tag {
            prefix,
            cpv: [0; 7],
            nu: 0,
            bn,
        }
    }

    /// A chain of `n` headers, block `i` linking to block `i - 1`.
    fn chain(n: u64) -> Vec<HeaderRef> {
        let mut headers = Vec::with_capacity(n as usize);
        let mut parent = [0u8; 32];
        for number in 0..n {
            let mut hash = [0u8; 32];
            hash[..8].copy_from_slice(&number.to_be_bytes());
            let mut merged_mining_hash = [0xC0u8; 32];
            merged_mining_hash[..8].copy_from_slice(&number.to_be_bytes());
            headers.push(HeaderRef {
                number,
                hash,
                merged_mining_hash,
                parent,
                bitcoin_lsb: Some(0),
            });
            parent = hash;
        }
        headers
    }

    #[test]
    fn a_tag_targets_its_oldest_checkpoint() {
        let mut tag = tag(448, &[0x11; 32]);
        assert_eq!(tag_target(&tag), 0, "the soft checkpoint is 384 below");

        tag.bn = 100;
        assert_eq!(tag_target(&tag), 100, "too young for a CPV checkpoint");
    }

    #[test]
    fn a_root_clearing_the_bar_is_final() {
        let headers = chain(20);
        let tags: Vec<Tag> = (0..10)
            .map(|bn| tag(bn, &headers[bn as usize].merged_mining_hash))
            .collect();
        let root = assess(&headers, &tags, 10).expect("final");
        assert_eq!(root.number, 0);
        assert_eq!(root.endorse, 10);
    }

    #[test]
    fn a_short_sample_is_not_final() {
        // Ten tags collected but fourteen needed: even unanimous endorsement
        // cannot reach the bar the window asked for.
        let headers = chain(20);
        let tags: Vec<Tag> = (0..10)
            .map(|bn| tag(bn, &headers[bn as usize].merged_mining_hash))
            .collect();
        assert!(assess(&headers, &tags, 14).is_none());
    }

    #[test]
    fn split_endorsement_is_not_final() {
        // Two roots, five tags each: neither reaches 9 of 10.
        let mut headers = chain(10);
        let mut fork = chain(5);
        for (i, header) in fork.iter_mut().enumerate() {
            header.number += 10;
            header.hash[..8].copy_from_slice(&(100 + i as u64).to_be_bytes());
            header.merged_mining_hash[..8].copy_from_slice(&(100 + i as u64).to_be_bytes());
        }
        fork[0].parent = [0u8; 32];
        for i in 1..fork.len() {
            fork[i].parent = fork[i - 1].hash;
        }
        headers.extend(fork);

        let tags: Vec<Tag> = (0..5)
            .map(|bn| tag(bn, &headers[bn as usize].merged_mining_hash))
            .chain((0..5).map(|bn| tag(bn, &headers[10 + bn as usize].merged_mining_hash)))
            .collect();
        assert!(assess(&headers, &tags, 10).is_none());
    }

    #[test]
    fn two_qualifying_roots_are_contested() {
        let root = RootScore {
            root: [1; 32],
            number: 0,
            endorse: 10,
            dissent: 0,
        };
        let other = RootScore {
            root: [2; 32],
            number: 10,
            endorse: 10,
            dissent: 0,
        };
        assert_eq!(select_final(&[root], 10), (Some(root), false));
        assert_eq!(select_final(&[root, other], 10), (None, true));
    }

    #[test]
    fn no_tags_is_not_final() {
        let headers = chain(3);
        assert!(assess(&headers, &[], 10).is_none());
    }
}
