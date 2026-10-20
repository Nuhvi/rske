//! The Bitcoin header sync: download, validate and store the tail above the
//! anchor, overlapping the fetch with validation.
//!
//! Headers come from a small pool of peers, round-robining across all of them
//! with the next batch prefetched while the current one validates. The store is
//! durable, so a re-run resumes from the tip. Every header is validated by
//! [`rsk_core::Core::connect_headers`]; this module only moves bytes.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use bitcoin::block::BlockHash;
use bitcoin::blockdata::constants::genesis_block;
use bitcoin::hashes::Hash as _;
use bitcoin::pow::Work;
use rsk_core::{Core, DIFFICULTY_PERIOD, FINALITY_MARGIN_BLOCKS, FinalityStatus};
use tracing::{info, warn};

use crate::config::Config;
use crate::peers::{Peer, Pool};

/// The caller's clock, in unix seconds, for the finality rule. The core keeps
/// no clock of its own; this is where it gets one.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// An age in seconds, in the largest readable unit.
fn fmt_age(seconds: u64) -> String {
    const HOUR: u64 = 3_600;
    const DAY: u64 = 86_400;
    if seconds >= DAY {
        format!("{:.1} d", seconds as f64 / DAY as f64)
    } else if seconds >= HOUR {
        format!("{:.1} h", seconds as f64 / HOUR as f64)
    } else {
        format!("{seconds} s")
    }
}

/// A bar evaluation in blocks at the anchor's work per block, with `Φ` — the
/// bar relative to the work carried. Below one the honest chain clears it; a
/// fork with network share `s` is kept out while `Φ > s/(1-s)`.
fn fmt_bar(status: &FinalityStatus) -> String {
    let phi = status
        .phi()
        .map(|phi| format!(", Φ {phi:.3}"))
        .unwrap_or_default();
    format!(
        "{}/{} blocks ({}%{phi})",
        status.have_blocks(),
        status.need_blocks(),
        status.progress_percent()
    )
}

/// The finality state a run starts from. The mark is the anchor of the next
/// bar: a fresh store starts at the seeded checkpoint, a resumed one at the
/// newest mark it already finalized.
fn log_finality_start(core: &Core, now: u64) -> Result<()> {
    let Some(mark) = core.finalized_height()? else {
        return Ok(());
    };
    let Some(header) = core.header_at(mark)? else {
        return Ok(());
    };
    let tip = core.tip_height()?.unwrap_or(mark);
    let age = fmt_age(now.saturating_sub(header.time as u64));
    if tip == mark {
        info!(
            "finality: mark #{mark} (age {age}) is the seed checkpoint; \
             the first bar is measured from it"
        );
    } else {
        info!(
            "finality: mark #{mark} (age {age}), tip #{tip}; \
             the next bar is measured from the mark, not the checkpoint"
        );
    }
    if let Some(status) = core.finality_status(now)? {
        info!(
            "finality: bar {} at anchor #{} (estimated work since the mark)",
            fmt_bar(&status),
            status.anchor,
        );
    }
    Ok(())
}

/// One mark advance, with the bar that cleared it and when it happened.
/// Whether this run looks partitioned, from what the node can see: its tip
/// against the peers' best advertised height, and how old the tip is.
///
/// This is evidence, not proof — a partition that leaves the node on a fresh
/// fork at the peers' height would pass it. It is here so a timing run can say
/// whether it was talking to the network at all.
fn partition_note(
    core: &Core,
    now: u64,
    peer_height: u64,
    peers: usize,
    finished: bool,
) -> Result<String> {
    let Some(tip) = core.tip_height()? else {
        return Ok("no tip yet, so a partition cannot be ruled out".to_string());
    };
    let Some(header) = core.header_at(tip)? else {
        return Ok("no header at the tip, so a partition cannot be ruled out".to_string());
    };
    let age = now.saturating_sub(header.time as u64);
    let behind = peer_height.saturating_sub(tip);
    // Fresh: within two hours (a block is expected every ten minutes).
    let fresh = age <= 2 * 3_600;
    let at_peers = peer_height > 0 && behind <= 2;

    Ok(if peers < 2 {
        format!("only {peers} peer connected, so a partition cannot be ruled out")
    } else if peer_height == 0 {
        "peers advertised no height, so a partition cannot be ruled out".to_string()
    } else if at_peers && fresh {
        format!(
            "no sign of a partition: tip #{tip} is at the peers' best height ({peer_height}) \
             and {} old, across {peers} peers",
            fmt_age(age)
        )
    } else if !at_peers && !finished {
        // Mid-sync this is the expected state, not evidence of a partition.
        format!("still syncing: {behind} block(s) below the peers' best height ({peer_height})")
    } else if !at_peers {
        format!(
            "the sync ended {behind} block(s) below the peers' best height ({peer_height}) \
             — possibly partitioned"
        )
    } else {
        format!("tip #{tip} is {} old — could be a partition", fmt_age(age))
    })
}

/// One mark advance, with the bar that cleared it, when it happened, and (on
/// the first one) whether the run looks partitioned.
fn log_advance(
    new: u64,
    status: Option<FinalityStatus>,
    nth: usize,
    elapsed: f64,
    now: u64,
    partition: &str,
) {
    let which = if nth == 1 {
        "first mark advance".to_string()
    } else {
        format!("mark advance #{nth}")
    };
    let note = if partition.is_empty() {
        String::new()
    } else {
        format!("; {partition}")
    };
    match status {
        Some(status) => {
            let blocks = new.saturating_sub(status.anchor);
            let checkpoints = new / DIFFICULTY_PERIOD - status.anchor / DIFFICULTY_PERIOD;
            info!(
                "finality: {which}: #{new} (from #{}, +{blocks} block(s), \
                 +{checkpoints} checkpoint(s)) at {elapsed:.2}s — anchor age {}, bar {}{note}",
                status.anchor,
                fmt_age(now.saturating_sub(status.anchor_time)),
                fmt_bar(&status),
            );
        }
        None => info!("finality: {which}: #{new} at {elapsed:.2}s{note}"),
    }
}

/// What a completed sync did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// Headers newly stored.
    pub headers: usize,
    /// Batches downloaded and validated.
    pub batches: usize,
    /// Canonical tip height after the sync.
    pub tip: u64,
    /// Finality mark after the sync.
    pub finalized: u64,
    /// Cumulative chain work at the tip.
    pub chainwork: Work,
    /// The greatest height advertised by the peers that drove the sync.
    pub peer_height: u64,
}

/// A locator for our canonical tip: the last few headers (newest first), then
/// genesis as the universal fallback. A peer walks it to find the fork point
/// and serves headers after the first hash it knows.
pub(crate) fn locator(core: &Core) -> Result<Vec<BlockHash>> {
    let Some(tip) = core.tip_height()? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for height in (tip.saturating_sub(9)..=tip).rev() {
        if let Some(header) = core.header_at(height)? {
            out.push(header.block_hash());
        }
    }
    if let Some(genesis) = core.header_at(0)? {
        let hash = genesis.block_hash();
        if out.last() != Some(&hash) {
            out.push(hash);
        }
    }
    Ok(out)
}

fn start_height(core: &Core) -> Result<i32> {
    Ok(core.tip_height()?.unwrap_or(0).min(i32::MAX as u64) as i32)
}

/// Running totals for the sync report.
struct Stats {
    started: Instant,
    total: usize,
    batches: usize,
    works: Vec<Work>,
    /// The greatest advertised height across the peers driving the sync.
    peer_height: u64,
    /// Peers the run connected to (used for the partition note).
    peers: usize,
    /// Mark advances this run.
    advances: usize,
    /// Seconds from sync start to the first advance.
    first_advance: Option<f64>,
    /// The most recent bar evaluation.
    last_status: Option<FinalityStatus>,
}

/// Download and validate the tail above the current tip, driving the pool of
/// `candidates`. Returns the [`SyncReport`] once every peer is caught up.
pub(crate) fn run(
    core: &Core,
    config: &Config,
    candidates: &mut VecDeque<SocketAddr>,
) -> Result<SyncReport> {
    let want = config.peers.max(1);
    let mut pool = Pool::new(config.user_agent.clone());
    pool.fill(candidates, want, config.limit, start_height(core)?);
    if pool.is_empty() {
        bail!("could not connect to any peer");
    }
    let summary = pool.summary();
    let mut peers = pool.into_peers();
    let n = peers.len();
    info!("{n} peer(s) connected: {summary}");

    let mut stats = Stats {
        started: Instant::now(),
        total: 0,
        batches: 0,
        works: Vec::new(),
        peer_height: peers
            .iter()
            .map(|p| p.handshake.start_height.max(0) as u64)
            .max()
            .unwrap_or(0),
        peers: n,
        advances: 0,
        first_advance: None,
        last_status: None,
    };

    // What the first bar will be measured from: the store's mark. A fresh store
    // starts at the checkpoint; a resumed one at its own last finalization.
    log_finality_start(core, unix_now())?;

    // Every peer is a worker; `alive[i]` marks those still usable. The sync
    // round-robins across them and prefetches the next batch while the current
    // one validates, so download overlaps the (CPU-bound) validation.
    let mut alive = vec![true; n];
    let mut cur = 0usize;
    peers[cur]
        .session
        .request_headers(&locator(core)?, BlockHash::all_zeros())?;

    loop {
        let server = peers[cur].addr;
        let headers = match peers[cur].session.read_headers() {
            Ok(headers) => headers,
            Err(e) => {
                warn!("  peer {server} dropped: {e:#}");
                alive[cur] = false;
                let Some(next) = next_alive(&alive, cur) else {
                    bail!("all peers dropped");
                };
                cur = next;
                peers[cur]
                    .session
                    .request_headers(&locator(core)?, BlockHash::all_zeros())?;
                continue;
            }
        };
        if headers.is_empty() {
            return report(core, config, &stats);
        }

        // Prefetch the next batch on another peer *before* validating this one.
        let last = headers.last().context("non-empty headers")?.block_hash();
        cur = prefetch(&mut peers, &mut alive, cur, last)?;

        let first = core.tip_height()?.unwrap_or(0) + 1;
        let now = unix_now();
        let out = core.connect_headers(&headers, now)?;
        stats.batches += 1;
        stats.total += out.connected;
        stats.last_status = out.finality;
        stats
            .works
            .push(core.chainwork()?.context("chainwork after connect")?);
        let tip = core.tip_height()?.context("tip after connect")?;
        let seen = first + out.connected as u64 - 1;

        // Every mark advance is reported, whenever it happens. The first one
        // also says whether the run looks partitioned.
        if let Some(finalized) = out.finalized {
            stats.advances += 1;
            let elapsed = stats.started.elapsed().as_secs_f64();
            if stats.first_advance.is_none() {
                stats.first_advance = Some(elapsed);
            }
            let partition = if stats.advances == 1 {
                let live = alive.iter().filter(|a| **a).count();
                partition_note(core, now, stats.peer_height, live, false)?
            } else {
                String::new()
            };
            log_advance(
                finalized,
                out.finality,
                stats.advances,
                elapsed,
                now,
                &partition,
            );
        }

        // Print the first handful, then every 20th batch, so a long run stays
        // readable. Until the first advance, show how far along the bar is.
        if stats.batches <= 10 || stats.batches.is_multiple_of(20) {
            let percent = if stats.peer_height > 0 {
                format!(" {:>5.1}%", 100.0 * seen as f64 / stats.peer_height as f64)
            } else {
                String::new()
            };
            let rate = stats.total as f64 / stats.started.elapsed().as_secs_f64().max(0.001);
            let live = alive.iter().filter(|a| **a).count();
            let bar = match (stats.advances, out.finalized, out.finality) {
                (0, None, Some(status)) => format!("  bar {}", fmt_bar(&status)),
                _ => String::new(),
            };
            info!(
                "[{:>4}] headers {first}..={seen}  tip {tip}{percent}  {rate:.0} h/s  peer {server} ({live}/{n} up){bar}",
                stats.batches
            );
        }
    }
}

/// The next alive peer after `from`, round-robin.
fn next_alive(alive: &[bool], from: usize) -> Option<usize> {
    let n = alive.len();
    (1..=n).map(|step| (from + step) % n).find(|&p| alive[p])
}

/// Send the next `getheaders` (for the batch after `last`) to an alive peer,
/// round-robin, returning whichever peer now holds the outstanding request. A
/// peer whose send fails is marked dead; if every other peer is gone, the peer
/// we just read from is reused (single-peer fallback).
fn prefetch(peers: &mut [Peer], alive: &mut [bool], from: usize, last: BlockHash) -> Result<usize> {
    let n = peers.len();
    for step in 1..=n {
        let p = (from + step) % n;
        if !alive[p] {
            continue;
        }
        match peers[p]
            .session
            .request_headers(&[last], BlockHash::all_zeros())
        {
            Ok(()) => return Ok(p),
            Err(e) => {
                warn!("  peer {} dropped (request): {e:#}", peers[p].addr);
                alive[p] = false;
            }
        }
    }
    peers[from]
        .session
        .request_headers(&[last], BlockHash::all_zeros())?;
    Ok(from)
}

/// The end-of-run summary and the invariants that say the sync was real.
fn report(core: &Core, config: &Config, stats: &Stats) -> Result<SyncReport> {
    let tip = core.tip_height()?.context("no tip after sync")?;
    let elapsed = stats.started.elapsed().as_secs_f64();
    info!(
        "\nsynced {} headers in {} batches over {elapsed:.1}s ({:.0} headers/s); tip {tip}",
        stats.total,
        stats.batches,
        stats.total as f64 / elapsed.max(0.001)
    );

    // 1. We reached the peer's advertised height.
    if stats.peer_height > 0 {
        anyhow::ensure!(
            tip >= stats.peer_height,
            "tip {tip} is below the peer's advertised height {}",
            stats.peer_height
        );
        info!(
            "peer advertised height {}; our tip is {tip}",
            stats.peer_height
        );
    }

    // 2. Cumulative work grew monotonically with the tip.
    for window in stats.works.windows(2) {
        anyhow::ensure!(
            window[1] > window[0],
            "cumulative work did not increase across a batch"
        );
    }
    let chainwork = core.chainwork()?.context("chainwork after sync")?;
    info!("chainwork: {chainwork} (log2 {:.1})", chainwork.log2());

    // 3. Genesis survived and the canonical (root) chain won.
    let genesis = core.header_at(0)?.context("genesis pruned")?;
    anyhow::ensure!(
        genesis.block_hash() == genesis_block(config.network).header.block_hash(),
        "genesis hash changed"
    );
    anyhow::ensure!(
        core.best_fork()? == Some(0),
        "a fork won the canonical chain"
    );

    // 4. Finality. A lag is legitimate — the bar is allowed to deny a chain —
    // so this reports the state rather than asserting progress.
    let finalized = core.finalized_height()?.unwrap_or(0);
    let expected = tip.saturating_sub(FINALITY_MARGIN_BLOCKS);
    match stats.first_advance {
        Some(first) => info!(
            "finality: {} mark advance(s) this run; first at {first:.2}s, mark now #{finalized}",
            stats.advances
        ),
        None => info!("finality: no mark advance this run; mark is still #{finalized}"),
    }
    let now = unix_now();
    // Show the bar whenever the mark is not where it should be, including after
    // advances: a lagging mark then says *why* (e.g. 92% of the bar, Φ above 1).
    if (stats.advances == 0 || finalized < expected)
        && let Some(status) = stats.last_status
    {
        info!(
            "finality: bar {} at anchor #{} (age {})",
            fmt_bar(&status),
            status.anchor,
            fmt_age(now.saturating_sub(status.anchor_time)),
        );
    }
    if finalized < expected {
        warn!(
            "finality: mark #{finalized} is {} block(s) behind the expected mark \
             #{expected} — the bar has not cleared",
            expected - finalized
        );
    } else {
        info!("finality: mark #{finalized} (expected mark ~#{expected})");
    }

    // 5. Whether this run looked partitioned. Evidence, not proof: a fork served
    // at the peers' height would pass it, but a stale or lagging tip will not.
    match partition_note(core, now, stats.peer_height, stats.peers, true) {
        Ok(note) => info!("finality: {note}"),
        Err(e) => warn!("finality: could not judge partitioning: {e}"),
    }

    Ok(SyncReport {
        headers: stats.total,
        batches: stats.batches,
        tip,
        finalized,
        chainwork,
        peer_height: stats.peer_height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::Network;

    /// A temp store anchored on the first `periods` spine checkpoints
    /// (heights 0, 2016, 4032, …). The temp dir must outlive the store.
    fn anchored(periods: usize) -> (tempfile::TempDir, Core) {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(&dir.path().join("chain.redb"), Network::Bitcoin).unwrap();
        let spine = rsk_core::btc::checkpoints::parse().unwrap();
        core.seed_checkpoints(&spine[..periods]).unwrap();
        (dir, core)
    }

    #[test]
    fn locator_is_the_tip_then_genesis() {
        let (_dir, core) = anchored(3); // heights 0, 2016, 4032
        let spine = rsk_core::btc::checkpoints::parse().unwrap();

        // Only the tip boundary (4032) is within the last ten heights; genesis
        // is always appended as the fallback.
        let loc = locator(&core).unwrap();
        assert_eq!(loc.first(), Some(&spine[2].1.block_hash()));
        assert_eq!(loc.last(), Some(&spine[0].1.block_hash()));
        assert_eq!(loc.len(), 2);
    }

    #[test]
    fn locator_is_empty_without_a_tip() {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(&dir.path().join("chain.redb"), Network::Bitcoin).unwrap();
        assert!(locator(&core).unwrap().is_empty());
    }
}
