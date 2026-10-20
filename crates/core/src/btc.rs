//! Finding the Bitcoin canonical chain — the Electrum SPV model.
//!
//! This is the side-effect-free core's chain logic. It mirrors Electrum's
//! `BlockchainManager`: a **tree of chains** (forks), each with a `forkpoint`
//! and a `parent`, and the **best chain** is the one with the most cumulative
//! work. Incoming headers are attached to whichever chain they extend; a header
//! whose parent is a known block but not a chain tip starts a **new fork**,
//! which is kept (not rejected) and can later win if it becomes heavier.
//!
//! The trust anchor is a **checkpoint spine**: one header per difficulty period
//! (2016 blocks), seeded from this crate's embedded `checkpoints.txt`. Because Bitcoin
//! difficulty is constant within a period, each spine header pins the work of
//! its whole period, so the cumulative work of the *entire* chain is known
//! without holding every block.
//!
//! The spine also sets the **finality mark**: a pointer to the newest header we
//! treat as un-reorgable. The mark may be any height — the rule keeps it a
//! [`FINALITY_MARGIN_BLOCKS`] below the tip — and a **checkpoint** is the newest
//! difficulty-period start at or below it ([`finality::is_checkpoint`]). Like
//! Electrum (which refuses forks below its hardcoded max checkpoint), we refuse
//! forks whose `forkpoint` falls at or below the mark, but we **do not prune**
//! the chain when it advances — headers are retained until Rootstock establishes
//! a checkpoint, at which point pruning can occur. The spine's own table is the
//! broader *boundary ladder*, which is what makes the whole chain's work
//! computable. [`crate::btc::finality`] holds the rule that gates the mark.
//!
//! Simplifications for this slice, stated explicitly so they are easy to lift:
//!
//! - The spine is trusted wholesale. Nothing before the first checkpoint is
//!   ever validated.
//! - Future-time, version and transaction/merkle checks are not made: the core
//!   has no clock (the caller hands the finality rule one), and the first two
//!   add no SPV value. See [`crate::btc::validation`] for the exact rule set.

use bitcoin::block::BlockHash;
use bitcoin::block::Header as BitcoinHeader;
use bitcoin::consensus::Params;
use bitcoin::hashes::Hash;
use bitcoin::pow::Work;

use crate::Core;
use crate::error::Error;
use crate::store::{BtcReads, DIFFICULTY_PERIOD, ReadTxn, WriteTxn};

use self::finality::FinalityStatus;

pub mod checkpoints;
pub mod finality;
pub mod validation;

/// How far below the tip the finality mark must stay: the reorg-confidence
/// depth. 100 blocks is the coinbase-maturity window. The mark itself may be any
/// height; a **checkpoint** is the newest difficulty-period start at or below it
/// ([`Core::finalized_checkpoint`]).
pub const FINALITY_MARGIN_BLOCKS: u64 = 100;

/// Outcome of a [`Core::connect_headers`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectOutcome {
    /// Headers newly accepted and stored (across whatever fork they landed on).
    pub connected: usize,
    /// The fork that was current before this batch. `None` on the very first
    /// connection or when the batch stayed on the same chain.
    pub prev_best_fork: Option<u64>,
    /// The chain that is best after this batch.
    pub best_fork: Option<u64>,
    /// Canonical tip height after this batch.
    pub tip_height: u64,
    /// The new finality mark, if this batch advanced it: the mark is advanced
    /// automatically, inside the same transaction as the connect.
    pub finalized: Option<u64>,
    /// The finality bar as this batch left it: what the best chain carries
    /// above the mark and what the rule demands. `None` when the store is
    /// unseeded or a fork (not the chain the mark is measured on) is best.
    pub finality: Option<FinalityStatus>,
}

impl Core {
    /// Plant the trust anchor: the checkpoint spine, one header per difficulty
    /// period, accepted without validating its ancestors. This makes the whole
    /// chain's work computable and creates the root chain. The network is
    /// recorded in the same transaction so a later connect with different
    /// parameters is refused.
    pub fn seed_checkpoints(&self, entries: &[(u64, BitcoinHeader)]) -> Result<(), Error> {
        let txn = self.store.write()?;
        txn.seed_checkpoints(entries)?;
        txn.set_network(self.network)?;
        txn.commit()
    }

    /// The best (heaviest) chain id, if seeded.
    pub fn best_fork(&self) -> Result<Option<u64>, Error> {
        self.store.read()?.best_fork()
    }

    /// The canonical tip height, if the chain has been seeded.
    pub fn tip_height(&self) -> Result<Option<u64>, Error> {
        let txn = self.store.read()?;
        match txn.best_fork()? {
            None => Ok(None),
            Some(f) => Ok(txn.fork_tip(f)?.map(|(h, _)| h)),
        }
    }

    /// Cumulative proof-of-work of the best chain through its tip — the number
    /// chain selection compares across forks, observable from outside the
    /// crate (drivers print it; nothing in the logic depends on it).
    pub fn chainwork(&self) -> Result<Option<Work>, Error> {
        let txn = self.store.read()?;
        match txn.best_fork()? {
            None => Ok(None),
            Some(best) => txn.fork_chainwork(best),
        }
    }

    /// Cumulative proof-of-work of the best chain at `height`.
    pub fn chainwork_at(&self, height: u64) -> Result<Option<Work>, Error> {
        let txn = self.store.read()?;
        let Some(best) = txn.best_fork()? else {
            return Ok(None);
        };
        txn.work_at(best, height)
    }

    /// The finality **mark**: the newest header that has cleared the bar and is
    /// treated as un-reorgable. It may be any height (the rule keeps it
    /// [`FINALITY_MARGIN_BLOCKS`] below the tip), monotonic, and `None` before
    /// the store is seeded.
    pub fn finalized_height(&self) -> Result<Option<u64>, Error> {
        self.store.read()?.finalized_height()
    }

    /// The newest **checkpoint**: the last difficulty-period start at or below
    /// the mark. This is what a Rootstock tail starts from, because a checkpoint
    /// is a boundary while the mark itself is only a reorg depth.
    pub fn finalized_checkpoint(&self) -> Result<Option<u64>, Error> {
        Ok(self
            .finalized_height()?
            .map(|mark| (mark / DIFFICULTY_PERIOD) * DIFFICULTY_PERIOD))
    }

    /// The finality bar as the rule sees it right now: the mark it is measured
    /// from, the estimated work it demands, and what the best chain carries
    /// above the mark. A read-only view of the decision the connect path makes,
    /// for status output.
    pub fn finality_status(&self, now: u64) -> Result<Option<FinalityStatus>, Error> {
        let txn = self.store.read()?;
        Ok(measure_finality(&txn, self.bar, now, self.genesis_time())?.map(|m| m.status))
    }

    /// Move the finality mark forward to the newest **shipped** checkpoint the
    /// store already holds, if that is newer than the current mark. Returns
    /// `(from, to)` when the mark moved.
    ///
    /// The compiled-in spine is the binary's subjective trust anchor. A store
    /// seeded from an older or shorter spine would otherwise never benefit from
    /// a newer one: the mark is the anchor, and seeding happens only once. This
    /// is the "upgrade — trust a newer checkpoint shipped in the new binary"
    /// path, applied to a store that already exists.
    ///
    /// Forward-only, and only to a header the store can prove it holds: the
    /// exact hash at that height on the spine's own chain. That is the same
    /// trust the seed takes, no more.
    pub fn reanchor_shipped(
        &self,
        trusted: &[(u64, BitcoinHeader)],
    ) -> Result<Option<(u64, u64)>, Error> {
        let txn = self.store.write()?;
        let Some(current) = txn.finalized_height()? else {
            // Unseeded: seeding decides the first mark.
            return Ok(None);
        };

        let mut adopted = None;
        for (height, header) in trusted.iter().rev() {
            if *height <= current {
                break;
            }
            if txn
                .header_at(0, *height)?
                .is_some_and(|stored| stored.block_hash() == header.block_hash())
            {
                adopted = Some(*height);
                break;
            }
        }

        match adopted {
            Some(height) => {
                txn.set_finalized(height)?;
                txn.commit()?;
                Ok(Some((current, height)))
            }
            // No writes were staged; dropping the transaction aborts it.
            None => Ok(None),
        }
    }

    /// The network's genesis block time: the age origin for the estimated-work
    /// power law. A Bitcoin constant, not a property of the store.
    fn genesis_time(&self) -> u64 {
        bitcoin::blockdata::constants::genesis_block(self.network)
            .header
            .time as u64
    }

    /// The work of the first difficulty period after `tail_start`:
    /// `work[tail_start .. tail_start + DIFFICULTY_PERIOD)`, or `None` if that
    /// period is not stored. Rootstock's tail work is measured against 2/3 of
    /// this. `tail_start` is a checkpoint ([`Core::finalized_checkpoint`]), then
    /// one period at a time back from it.
    pub fn btc_tail_work(&self, tail_start: u64) -> Result<Option<Work>, Error> {
        let txn = self.store.read()?;
        let Some(best) = txn.best_fork()? else {
            return Ok(None);
        };
        let end_height = tail_start.saturating_add(DIFFICULTY_PERIOD);
        let (Some(begin), Some(end)) = (
            txn.work_at(best, tail_start)?,
            txn.work_at(best, end_height)?,
        ) else {
            return Ok(None);
        };
        Ok(Some(end - begin))
    }

    /// The **checkpoints** — the period-boundary headers at or below the
    /// finality mark, in height order (height `0` first).
    ///
    /// A boundary is a checkpoint exactly when it starts a difficulty period
    /// ([`finality::is_checkpoint`]) *and* the mark has reached it. These are
    /// the headers safe to treat as irreversible, e.g. to embed as a trust
    /// anchor. The tip's nearest boundary is deliberately **excluded**: the
    /// mark never reaches into the last [`FINALITY_MARGIN_BLOCKS`].
    pub fn finalized_checkpoints(&self) -> Result<Vec<(u64, BitcoinHeader)>, Error> {
        let txn = self.store.read()?;
        let Some(finalized) = txn.finalized_height()? else {
            return Ok(Vec::new());
        };
        let Some(best) = txn.best_fork()? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let mut height = 0u64;
        while height <= finalized {
            if let Some(header) = txn.header_at(best, height)? {
                out.push((height, header));
            }
            height += DIFFICULTY_PERIOD;
        }
        Ok(out)
    }

    /// Height of a known block hash on the canonical chain, if it is canonical.
    pub fn height_of(&self, hash: &BlockHash) -> Result<Option<u64>, Error> {
        let txn = self.store.read()?;
        let Some((fork, height)) = txn.locate(hash)? else {
            return Ok(None);
        };
        Ok(is_canonical_at(&txn, fork, height)?.then_some(height))
    }

    /// The header at `height` on the canonical chain, if stored.
    pub fn header_at(&self, height: u64) -> Result<Option<BitcoinHeader>, Error> {
        let txn = self.store.read()?;
        let Some(best) = txn.best_fork()? else {
            return Ok(None);
        };
        txn.header_at(best, height)
    }

    /// True if `hash` is the canonical block at `height`.
    ///
    /// Test-only: nothing outside the core reads it.
    #[cfg(test)]
    pub fn is_canonical(&self, height: u64, hash: &BlockHash) -> Result<bool, Error> {
        let txn = self.store.read()?;
        let Some(best) = txn.best_fork()? else {
            return Ok(false);
        };
        Ok(txn
            .header_at(best, height)?
            .is_some_and(|h| h.block_hash() == *hash))
    }

    /// Validate and store a contiguous ascending run of headers, attaching them
    /// to whichever chain they extend — Electrum's `can_connect` model.
    ///
    /// For each run:
    ///
    /// - The first header's parent must be a known block. If that block is a
    ///   chain tip, the run extends that chain; otherwise it starts a **new
    ///   fork** (kept, not rejected).
    /// - Every header is PoW-, continuity-, difficulty- and
    ///   median-time-past-checked (see [`crate::btc::validation`]).
    /// - The best chain is re-selected by cumulative work; a fork only wins
    ///   when it is strictly heavier.
    /// - A fork whose `forkpoint` is at or below the finality mark is refused
    ///   ([`Error::ForkBelowFinalized`]) — the mark's floor.
    ///
    /// `now` is the caller's clock (unix seconds), used only by the finality
    /// rule: the mark advances when the work above the mark clears
    /// [`finality::estimated_work_bar`], the estimated work the network did
    /// since the mark. The core keeps no clock of its own, and the store
    /// remembers the greatest `now` seen, so a clock wound back cannot shrink
    /// the bar.
    ///
    /// Returns [`Error::NetworkMismatch`] if the store was seeded for a
    /// different network than this [`Core`] validates against.
    ///
    /// The whole operation — reads, validation, writes and the automatic mark
    /// advance below — runs inside one write transaction, so nothing can change
    /// the store between the reads a decision is based on and the writes that
    /// act on it.
    pub fn connect_headers(
        &self,
        headers: &[BitcoinHeader],
        now: u64,
    ) -> Result<ConnectOutcome, Error> {
        // One transaction for the entire read → validate → write → advance
        // sequence. Dropping it without committing (the early returns) aborts.
        let txn = self.store.write()?;

        // The store records the network it was seeded for; refuse to validate
        // real headers against the wrong consensus parameters.
        if let Some(seeded) = txn.network()?
            && seeded != self.network
        {
            return Err(Error::NetworkMismatch {
                seeded,
                opened: self.network,
            });
        }

        let prev_best = txn.best_fork()?;

        // Nothing to connect: report the current tip (the transaction aborts).
        if headers.is_empty() {
            let tip = match prev_best {
                Some(f) => txn.fork_tip(f)?.unwrap_or((0, BlockHash::all_zeros())).0,
                None => 0,
            };
            return Ok(ConnectOutcome {
                connected: 0,
                prev_best_fork: prev_best,
                best_fork: prev_best,
                tip_height: tip,
                finalized: None,
                finality: None,
            });
        }

        // Skip a leading prefix that is already stored anywhere (reconnects).
        let mut start = 0usize;
        while start < headers.len() && txn.locate(&headers[start].block_hash())?.is_some() {
            start += 1;
        }
        if start == headers.len() {
            let tip = match prev_best {
                Some(f) => txn.fork_tip(f)?.unwrap_or((0, BlockHash::all_zeros())).0,
                None => 0,
            };
            return Ok(ConnectOutcome {
                connected: 0,
                prev_best_fork: prev_best,
                best_fork: prev_best,
                tip_height: tip,
                finalized: None,
                finality: None,
            });
        }

        // Parent of the first *new* header: must be a known block.
        let parent = if start == 0 {
            headers[0].prev_blockhash
        } else {
            headers[start - 1].block_hash()
        };
        let (parent_fork, parent_height) =
            txn.locate(&parent)?.ok_or(Error::UnknownAncestor(parent))?;

        // Decide the target chain: extend the chain whose tip is the parent, or
        // start a new fork from the parent block.
        let parent_tip = txn
            .fork_tip(parent_fork)?
            .unwrap_or((0, BlockHash::all_zeros()))
            .0;
        let (target_fork, forkpoint) = if parent_height == parent_tip {
            (parent_fork, parent_height + 1)
        } else {
            let forkpoint = parent_height + 1;
            if let Some(finalized) = txn.finalized_height()?
                && forkpoint <= finalized
            {
                return Err(Error::ForkBelowFinalized {
                    fork: forkpoint,
                    finalized,
                });
            }
            let id = txn.create_fork(parent_fork, forkpoint, headers[start].block_hash())?;
            (id, forkpoint)
        };

        // Validate continuity, expected difficulty, median-time-past and PoW
        // for the new run. Earlier headers of this batch are visible to later
        // ones, so a long run validates itself.
        let params = Params::new(self.network);
        let enforce_difficulty = validation::enforces_difficulty(&params);
        let new_headers = &headers[start..];
        // Header at `height` on the chain being extended: a header already
        // accepted in this batch, or a stored ancestor (walking to ancestor
        // forks below the forkpoint).
        let at = |height: u64| -> Result<Option<BitcoinHeader>, Error> {
            if height >= forkpoint
                && let Some(header) = new_headers.get((height - forkpoint) as usize)
            {
                return Ok(Some(*header));
            }
            txn.header_at(target_fork, height)
        };

        let mut prev = parent;
        for (offset, header) in new_headers.iter().enumerate() {
            let height = forkpoint + offset as u64;
            if header.prev_blockhash != prev {
                return Err(Error::LinkBreak {
                    height,
                    expected: prev,
                });
            }

            if enforce_difficulty {
                let parent_header = at(height - 1)?
                    .ok_or_else(|| Error::Internal(format!("missing parent at {height}")))?;
                let period_start = match height.checked_sub(DIFFICULTY_PERIOD) {
                    Some(h) => at(h)?,
                    None => None,
                };
                if let Some(expected) = validation::expected_bits(
                    height,
                    &parent_header,
                    period_start.as_ref(),
                    &params,
                ) && header.bits != expected
                {
                    return Err(Error::BadDifficulty {
                        height,
                        expected: expected.to_consensus(),
                        got: header.bits.to_consensus(),
                    });
                }
            }

            if let Some(median) = median_time_past_at(&at, height)?
                && header.time <= median
            {
                return Err(Error::BadTimestamp {
                    height,
                    median,
                    got: header.time,
                });
            }

            header
                .validate_pow(header.target())
                .map_err(|e| Error::BadPow {
                    height,
                    detail: e.to_string(),
                })?;
            prev = header.block_hash();
        }

        // Cumulative work of the run, starting from the parent's work.
        let base_work = txn.work_at(parent_fork, parent_height)?.ok_or_else(|| {
            Error::Internal(format!("no work at ({parent_fork}, {parent_height})"))
        })?;
        let mut entries: Vec<(u64, &BitcoinHeader, Work)> =
            Vec::with_capacity(headers.len() - start);
        let mut cumulative = base_work;
        for (offset, header) in headers[start..].iter().enumerate() {
            cumulative = cumulative + header.target().to_work();
            entries.push((forkpoint + offset as u64, header, cumulative));
        }
        txn.put_headers(target_fork, &entries)?;

        // Re-select the best chain by cumulative work (strictly heavier wins).
        let mut best = txn.best_fork()?.unwrap_or(0);
        let best_work = txn
            .fork_chainwork(best)?
            .unwrap_or(Work::from_be_bytes([0; 32]));
        let target_work = txn
            .fork_chainwork(target_fork)?
            .unwrap_or(Work::from_be_bytes([0; 32]));
        if target_fork != best && target_work > best_work {
            txn.set_best_fork(target_fork)?;
            best = target_fork;
        }

        let tip_height = match prev_best {
            Some(_) => txn.fork_tip(best)?.unwrap_or((0, BlockHash::all_zeros())).0,
            None => forkpoint + (headers.len() - start) as u64 - 1,
        };

        // Advancing the finality mark is part of connecting headers: move it if
        // this batch carried enough work above the anchor to clear the bar.
        let finality = finalize_in_txn(&txn, self.bar, now, self.genesis_time())?;
        txn.commit()?;

        Ok(ConnectOutcome {
            connected: headers.len() - start,
            prev_best_fork: prev_best,
            best_fork: Some(best),
            tip_height,
            finalized: finality.advanced,
            finality: finality.status,
        })
    }

    /// Connect a single header (Electrum's near-tip `save_header` path) at the
    /// caller's clock.
    ///
    /// Test-only: the sync callers use [`Core::connect_headers`] directly.
    #[cfg(test)]
    pub fn connect_header(
        &self,
        header: &BitcoinHeader,
        now: u64,
    ) -> Result<ConnectOutcome, Error> {
        self.connect_headers(std::slice::from_ref(header), now)
    }

    /// Advance the finality mark to the newest difficulty-period boundary that
    /// is at least a full period below the tip, if the bar allows it. Returns
    /// the new mark.
    ///
    /// This runs automatically at the end of every [`Core::connect_headers`]
    /// batch (reported in [`ConnectOutcome::finalized`]); you do not normally
    /// need to call it.
    ///
    /// Only advances when the work above the mark clears
    /// [`finality::estimated_work_bar`] — the coinbase floor and the estimated
    /// work the network did since the mark. The mark is also the anchor: each
    /// advance re-anchors, so the next bar is measured from a fresher point.
    /// Cumulative work is known at every period boundary (the spine), so both
    /// sides are plain differences of stored cumulative work.
    ///
    /// Nothing advances while a fork is the best chain, since the spine
    /// (fork 0) is assumed canonical.
    ///
    /// Test-only: production advancement rides inside
    /// [`Core::connect_headers`].
    #[cfg(test)]
    pub fn finalize_period(&self, now: u64) -> Result<Option<u64>, Error> {
        let txn = self.store.write()?;
        match finalize_in_txn(&txn, self.bar, now, self.genesis_time())?.advanced {
            Some(height) => {
                txn.commit()?;
                Ok(Some(height))
            }
            // No writes were staged; dropping the transaction aborts it.
            None => Ok(None),
        }
    }
}

/// What the finality rule concluded for one connect.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Finality {
    /// The new mark, if this connect advanced it.
    advanced: Option<u64>,
    /// The bar as judged, if it could be measured.
    status: Option<FinalityStatus>,
}

impl Finality {
    /// The bar was measured but no boundary was settled this connect.
    fn not_yet(status: FinalityStatus) -> Self {
        Self {
            advanced: None,
            status: Some(status),
        }
    }
}

/// The finality rule, applied through an open transaction. Factored out so
/// `connect_headers` can run it inside the same transaction as the connect.
fn finalize_in_txn(
    txn: &WriteTxn,
    bar: finality::Bar,
    now: u64,
    genesis_time: u64,
) -> Result<Finality, Error> {
    // The clock may not go backwards: the bar grows with the anchor's age, so a
    // wound-back clock must not shrink it.
    let hwm = txn.now_hwm()?.unwrap_or(0);
    let now = now.max(hwm);
    if now > hwm {
        txn.set_now_hwm(now)?;
    }

    // Only the spine's chain settles a boundary: a fork that is best has no
    // trusted history behind it, and moving the mark onto it is exactly the
    // capture the rule exists to prevent.
    let Some(measurement) = measure_finality(txn, bar, now, genesis_time)? else {
        return Ok(Finality::default());
    };
    let status = measurement.status;
    let tip = measurement.tip;

    // The mark advances freely up to the reorg-confidence depth; it is not
    // quantized to a difficulty boundary. Checkpoints are derived from it.
    let Some(mark) = tip.checked_sub(FINALITY_MARGIN_BLOCKS) else {
        return Ok(Finality::not_yet(status));
    };
    if mark <= status.anchor {
        return Ok(Finality::not_yet(status));
    }
    if !status.clears() {
        return Ok(Finality::not_yet(status));
    }

    // RETENTION POLICY (RSK IBD design):
    // Do NOT prune the Bitcoin chain here. We keep all headers so Rootstock can
    // cross-link against the full canonical chain; the caller prunes below the
    // Bitcoin tail start only once Rootstock establishes a checkpoint.
    txn.set_finalized(mark)?;
    Ok(Finality {
        advanced: Some(mark),
        status: Some(status),
    })
}

/// A bar evaluation and the tip it was taken at.
struct Measurement {
    status: FinalityStatus,
    tip: u64,
}

/// The bar through an open transaction, writing nothing of its own. Shared by
/// the connect path and [`Core::finality_status`], so a status read and a
/// decision can never drift.
///
/// The anchor is the mark itself: any history the node has already finalized is
/// trusted, so a returning node measures from its own chain and a fresh store
/// measures from the seeded spine tip. Each advance re-anchors, resetting `T`.
fn measure_finality<R: BtcReads>(
    reads: &R,
    bar: finality::Bar,
    now: u64,
    genesis_time: u64,
) -> Result<Option<Measurement>, Error> {
    if reads.best_fork()? != Some(0) {
        return Ok(None);
    }
    let Some((tip, _)) = reads.fork_tip(0)? else {
        return Ok(None);
    };
    let Some(anchor) = reads.finalized_height()? else {
        return Ok(None);
    };
    let anchor_header = reads
        .header_at(0, anchor)?
        .ok_or_else(|| Error::Internal(format!("no header at the finality mark {anchor}")))?;
    let anchor_time = anchor_header.time as u64;
    let anchor_work = anchor_header.target().to_work();
    let now = now.max(anchor_time);
    let need = bar(anchor_work, anchor_time, genesis_time, now);

    let work = |height: u64| -> Result<Work, Error> {
        reads
            .work_at(0, height)?
            .ok_or_else(|| Error::Internal(format!("no cumulative work at height {height}")))
    };
    let (tip_work, anchor_cum) = (work(tip)?, work(anchor)?);
    if tip_work < anchor_cum {
        // Only reachable from an inconsistent store (a mark above the tip);
        // report nothing rather than underflow.
        return Ok(None);
    }
    Ok(Some(Measurement {
        status: FinalityStatus {
            anchor,
            anchor_time,
            anchor_work,
            have: tip_work - anchor_cum,
            need,
        },
        tip,
    }))
}

/// Whether a stored `(fork, height)` block lies on the canonical chain.
fn is_canonical_at(txn: &ReadTxn, fork: u64, height: u64) -> Result<bool, Error> {
    let Some(best) = txn.best_fork()? else {
        return Ok(false);
    };
    if fork == best {
        return Ok(true);
    }
    // Walk the fork's ancestry: a block is canonical if it is at or below the
    // best chain's forkpoint and on the best chain's ancestors.
    let mut f = best;
    loop {
        let Some(meta) = txn.fork_meta(f)? else {
            return Ok(false);
        };
        if fork == f {
            return Ok(height >= meta.forkpoint);
        }
        let Some(parent) = meta.parent else {
            return Ok(false);
        };
        f = parent;
    }
}

// ── validation helpers ──────────────────────────────────────────────────────

/// The median-time-past the header at `height` must exceed: the median of the
/// previous [`validation::MEDIAN_TIME_PAST`] timestamps. Returns `None` when
/// any of them is unavailable (e.g. just above a checkpoint that stores only
/// period boundaries), in which case the check is skipped for that header.
fn median_time_past_at(
    at: &impl Fn(u64) -> Result<Option<BitcoinHeader>, Error>,
    height: u64,
) -> Result<Option<u32>, Error> {
    let mut times = [0u32; validation::MEDIAN_TIME_PAST];
    for (k, slot) in times.iter_mut().enumerate() {
        let Some(h) = height.checked_sub(1 + k as u64) else {
            return Ok(None);
        };
        match at(h)? {
            Some(header) => *slot = header.time,
            None => return Ok(None),
        }
    }
    Ok(Some(validation::median_time_past(times)))
}

// ── proof-of-work helper ────────────────────────────────────────────────────

/// Compute the `Work` carried by a header (needed by callers that assemble
/// branches before handing them to [`Core::connect_headers`]).
pub fn header_work(header: &BitcoinHeader) -> Work {
    header.target().to_work()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::Network;
    use bitcoin::block::Version;
    use bitcoin::consensus::Decodable;
    use bitcoin::pow::CompactTarget;

    /// A fixed base timestamp for synthetic headers.
    const BASE_TIME: u32 = 1_600_000_000;

    /// The clock the synthetic tests connect at: 2,400,000 s (40 days) past
    /// [`BASE_TIME`]. The bar is then 4,000 blocks of work above the anchor, so
    /// a chain of 4,032 synthetic blocks settles its first new boundary — the
    /// neutral setting for tests that are not about the bar itself.
    const NOW: u64 = BASE_TIME as u64 + 2_400_000;

    /// The easiest valid Bitcoin target (`0x207fffff`): nearly any hash passes,
    /// so we can synthesize long chains quickly without a real miner. It is
    /// also exactly the regtest proof-of-work limit, which is the network the
    /// synthetic tests use.
    const EASY_BITS: u32 = 0x207f_ffff;

    /// Targets with per-block work exactly 20 and 22 (a 1.1x difficulty step).
    ///
    /// `Target::to_work` is an integer division (`2^256 / (target + 1)`), so
    /// near the top of the target range only whole multiples of `EASY_BITS`'s
    /// work of 2 are reachable (`0x207effff` also floors to 2). Starting from a
    /// larger base gives finer steps while still passing about one hash in 20,
    /// so mining stays instant.
    const WORK20_BITS: u32 = 0x200c_cccc;
    const WORK22_BITS: u32 = 0x200b_a2e8;

    /// Mine a header on top of `prev` with an explicit timestamp and target.
    fn mine_at(prev: BlockHash, time: u32, bits: u32) -> BitcoinHeader {
        let target = CompactTarget::from_consensus(bits);
        let mut nonce = 0u32;
        loop {
            let h = BitcoinHeader {
                version: Version::ONE,
                prev_blockhash: prev,
                merkle_root: bitcoin::hashes::Hash::all_zeros(),
                time,
                bits: target,
                nonce,
            };
            if h.validate_pow(h.target()).is_ok() {
                return h;
            }
            nonce = nonce.wrapping_add(1);
            assert!(nonce != 0, "could not mine a header");
        }
    }

    /// Mine a child of `parent`. `tag` shifts the timestamp forward from the
    /// parent's, which keeps timestamps monotonic across separate runs (so the
    /// median-time-past rule holds) and makes two children of the same parent
    /// distinct.
    fn mine_child(parent: &BitcoinHeader, bits: u32, tag: u32) -> BitcoinHeader {
        mine_at(parent.block_hash(), parent.time + 1 + tag, bits)
    }

    /// Mine `n` contiguous headers above `parent` with the given target bits.
    fn mine_chain_from(
        parent: &BitcoinHeader,
        n: usize,
        bits: u32,
        tag_offset: u32,
    ) -> Vec<BitcoinHeader> {
        let mut out = Vec::with_capacity(n);
        let mut cur = *parent;
        for i in 0..n {
            let h = mine_child(&cur, bits, tag_offset + i as u32);
            cur = h;
            out.push(h);
        }
        out
    }

    /// Mine the synthetic genesis (parent is the virtual all-zero block).
    fn mine_genesis(tag: u32) -> BitcoinHeader {
        mine_at(BlockHash::all_zeros(), BASE_TIME + tag, EASY_BITS)
    }

    /// Easy-target chain above `parent`; `tag_offset` separates competing forks.
    fn easy_chain(parent: &BitcoinHeader, n: usize, tag_offset: u32) -> Vec<BitcoinHeader> {
        mine_chain_from(parent, n, EASY_BITS, tag_offset)
    }

    /// A fresh in-memory regtest chain seeded with a synthetic checkpoint at 0.
    ///
    /// Regtest permits minimum-difficulty blocks, so (like Electrum) the
    /// difficulty rule is skipped; the tests here exercise the fork/finality
    /// machinery and the median-time-past rule. The difficulty rule itself is
    /// tested in [`crate::btc::validation`] and against real mainnet data.
    fn seeded() -> Core {
        let chain = Core::in_memory(Network::Regtest).expect("in-memory store");
        let genesis = mine_genesis(0);
        chain.seed_checkpoints(&[(0, genesis)]).expect("seed");
        chain
    }

    /// The header at `height` on the canonical chain.
    fn canonical(chain: &Core, height: u64) -> BitcoinHeader {
        chain
            .header_at(height)
            .unwrap()
            .unwrap_or_else(|| panic!("no canonical header at {height}"))
    }

    /// The hash of the block at `height` on the canonical chain.
    fn canonical_hash(chain: &Core, height: u64) -> BlockHash {
        canonical(chain, height).block_hash()
    }

    fn real_genesis() -> BitcoinHeader {
        let hex = "0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a29ab5f49ffff001d1dac2b7c";
        let bytes = hex::decode(hex).unwrap();
        let mut reader = bytes.as_slice();
        BitcoinHeader::consensus_decode(&mut reader).unwrap()
    }

    #[test]
    fn seeds_and_extends() -> eyre::Result<()> {
        let chain = seeded();
        assert_eq!(chain.tip_height()?, Some(0));
        assert_eq!(chain.finalized_height()?, Some(0));
        assert_eq!(chain.best_fork()?, Some(0));

        let run = easy_chain(&canonical(&chain, 0), 5, 1);
        let out = chain.connect_headers(&run, NOW)?;
        assert_eq!(out.connected, 5);
        assert_eq!(out.best_fork, Some(0));
        assert_eq!(out.tip_height, 5);

        let h3 = canonical_hash(&chain, 3);
        assert!(chain.is_canonical(3, &h3)?);
        assert_eq!(chain.height_of(&h3)?, Some(3));
        Ok(())
    }

    #[test]
    fn seeding_twice_is_refused_and_leaves_the_chain_intact() -> eyre::Result<()> {
        let chain = seeded();
        let run = easy_chain(&canonical(&chain, 0), 5, 1);
        chain.connect_headers(&run, NOW)?;
        let tip = chain.tip_height()?;
        let work = chain.chainwork()?;

        // Re-seeding an anchored store is refused rather than clobbering the
        // taller tip and the finality floor.
        let other = mine_genesis(42);
        let err = chain.seed_checkpoints(&[(0, other)]).unwrap_err();
        assert!(matches!(err, Error::AlreadySeeded), "got {err:?}");

        assert_eq!(chain.tip_height()?, tip);
        assert_eq!(chain.chainwork()?, work);
        Ok(())
    }

    #[test]
    fn rejects_bad_pow() -> eyre::Result<()> {
        let chain = seeded();
        let mut bad = easy_chain(&canonical(&chain, 0), 1, 1);
        bad[0].bits = CompactTarget::from_consensus(0x1d00_ffff);
        let err = chain.connect_headers(&bad, NOW).unwrap_err();
        assert!(matches!(err, Error::BadPow { .. }), "got {err:?}");
        Ok(())
    }

    #[test]
    fn rejects_unknown_ancestor() -> eyre::Result<()> {
        let chain = seeded();
        let run = [mine_at(BlockHash::all_zeros(), BASE_TIME + 1, EASY_BITS)]; // links to nothing stored
        let err = chain.connect_headers(&run, NOW).unwrap_err();
        assert!(matches!(err, Error::UnknownAncestor(_)), "got {err:?}");
        Ok(())
    }

    #[test]
    fn single_header_connects() -> eyre::Result<()> {
        let chain = seeded();
        let h = mine_child(&canonical(&chain, 0), EASY_BITS, 1);
        let out = chain.connect_header(&h, NOW)?;
        assert_eq!(out.connected, 1);
        assert_eq!(out.tip_height, 1);
        assert!(chain.is_canonical(1, &h.block_hash())?);
        Ok(())
    }

    #[test]
    fn lighter_fork_is_kept_not_rejected() -> eyre::Result<()> {
        let chain = seeded();
        let main = easy_chain(&canonical(&chain, 0), 5, 1);
        chain.connect_headers(&main, NOW)?;
        assert_eq!(chain.tip_height()?, Some(5));

        // A lighter fork from height 3 (parent = main[2]) with 2 blocks. Its
        // tip (height 5) ties, but with strictly less work it must not become
        // best — and crucially it must be *stored*, not rejected.
        let fork = easy_chain(&main[2], 2, 1000);
        let out = chain.connect_headers(&fork, NOW)?;
        assert_eq!(out.connected, 2);
        assert_eq!(out.best_fork, Some(0), "best chain must stay the root");

        // The fork survives in the tree and its headers are locatable.
        assert_eq!(chain.store.read()?.fork_count()?, 2);
        let (f, h) = chain.store.read()?.locate(&fork[0].block_hash())?.unwrap();
        assert_eq!(h, 4);
        assert_ne!(f, 0);
        // The fork's blocks are not canonical.
        assert!(!chain.is_canonical(4, &fork[0].block_hash())?);
        Ok(())
    }

    /// A fork's cumulative work below its forkpoint lives on its ancestors, so
    /// `chainwork_at` must resolve through them rather than read the fork's own
    /// (absent) rows.
    #[test]
    fn chainwork_at_resolves_the_ancestor_fork() -> eyre::Result<()> {
        let chain = seeded();
        let main = easy_chain(&canonical(&chain, 0), 5, 1);
        chain.connect_headers(&main, NOW)?;

        // A heavier fork from height 3 (parent = main[2]) wins; its forkpoint is
        // 4, so heights below it live on the root chain.
        let fork = easy_chain(&main[2], 5, 1000);
        chain.connect_headers(&fork, NOW)?;
        let best = chain.best_fork()?.expect("a best fork");
        assert_ne!(best, 0);

        let txn = chain.store.read()?;
        // Below the forkpoint both reads resolve to the root chain's row.
        assert_eq!(chain.chainwork_at(2)?, txn.work_at(0, 2)?);
        assert_eq!(txn.work_at(best, 2)?, txn.work_at(0, 2)?);
        // At/above the forkpoint the fork's own row is used.
        assert_eq!(chain.chainwork_at(4)?, txn.work_at(best, 4)?);
        Ok(())
    }

    #[test]
    fn failed_batch_leaves_no_partial_state() -> eyre::Result<()> {
        // A batch whose parent is not a chain tip starts a new fork; if a later
        // header in the same batch fails validation, the fork must be rolled
        // back too — the whole read/validate/write/advance runs in one txn.
        let chain = seeded();
        let main = easy_chain(&canonical(&chain, 0), 5, 1);
        chain.connect_headers(&main, NOW)?;
        assert_eq!(chain.store.read()?.fork_count()?, 1);

        // Fork from height 3 (parent = main[2], not the tip), second header
        // with bad PoW (a much harder target than it was mined for).
        let mut fork = easy_chain(&main[2], 2, 1000);
        fork[1].bits = CompactTarget::from_consensus(0x1d00_ffff);
        let err = chain.connect_headers(&fork, NOW).unwrap_err();
        assert!(matches!(err, Error::BadPow { .. }), "got {err:?}");

        // No orphan fork, and no header of the batch was committed.
        assert_eq!(chain.store.read()?.fork_count()?, 1, "no orphan fork");
        assert!(chain.store.read()?.locate(&fork[0].block_hash())?.is_none());
        Ok(())
    }

    #[test]
    fn heavier_fork_wins() -> eyre::Result<()> {
        let chain = seeded();
        let main = easy_chain(&canonical(&chain, 0), 5, 1);
        chain.connect_headers(&main, NOW)?;

        // Fork from height 3 with 5 blocks → tip 8, strictly more work.
        let fork = easy_chain(&main[2], 5, 1000);
        let out = chain.connect_headers(&fork, NOW)?;
        assert_eq!(out.connected, 5);
        assert_eq!(out.tip_height, 8);
        let best = chain.best_fork()?.unwrap();
        assert_ne!(best, 0, "fork must become best");
        assert_eq!(out.best_fork, Some(best));

        // Fork tip is canonical; old branch tip is not.
        assert!(chain.is_canonical(8, &fork[4].block_hash())?);
        assert!(!chain.is_canonical(5, &main[4].block_hash())?);
        // Canonical reads below the forkpoint fall back to the shared parent.
        assert!(chain.is_canonical(3, &main[2].block_hash())?);
        Ok(())
    }

    #[test]
    fn fork_below_finalized_is_refused() -> eyre::Result<()> {
        let chain = seeded();
        let main = easy_chain(&canonical(&chain, 0), 6, 1);
        chain.connect_headers(&main, NOW)?;

        // Advance the finality mark to height 1, as an advance would.
        let txn = chain.store.write()?;
        txn.set_finalized(1)?;
        txn.commit()?;
        assert_eq!(chain.finalized_height()?, Some(1));

        // A fork starting at height 1 (forkpoint 1 <= finalized 1) is refused.
        let fork = easy_chain(&canonical(&chain, 0), 3, 1000);
        let err = chain.connect_headers(&fork, NOW).unwrap_err();
        assert!(
            matches!(err, Error::ForkBelowFinalized { .. }),
            "got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn lighter_fork_then_extends_to_win() -> eyre::Result<()> {
        // Electrum's grace: a fork is kept even while lighter, so it can win
        // later when more of its headers arrive.
        let chain = seeded();
        let main = easy_chain(&canonical(&chain, 0), 5, 1);
        chain.connect_headers(&main, NOW)?;
        assert_eq!(chain.best_fork()?, Some(0));

        // First arrives a short, lighter slice of a fork from height 3.
        let fork_part1 = easy_chain(&main[2], 1, 1000);
        let out = chain.connect_headers(&fork_part1, NOW)?;
        assert_eq!(out.connected, 1);
        assert_eq!(chain.best_fork()?, Some(0), "still root while lighter");

        // Later, more headers of the same fork arrive, tipping it heavier.
        let fork_part2 = easy_chain(&fork_part1[0], 4, 1001);
        let out = chain.connect_headers(&fork_part2, NOW)?;
        assert_eq!(out.connected, 4);
        assert_eq!(out.tip_height, 8);
        let best = chain.best_fork()?.unwrap();
        assert_ne!(best, 0, "fork wins once it becomes heavier");
        // The two batches landed on the same fork.
        let txn = chain.store.read()?;
        let (f1, _) = txn.locate(&fork_part1[0].block_hash())?.unwrap();
        let (f2, _) = txn.locate(&fork_part2[3].block_hash())?.unwrap();
        assert_eq!(f1, f2);
        assert!(chain.is_canonical(8, &fork_part2[3].block_hash())?);
        Ok(())
    }

    #[test]
    fn finalizes_period_retains_headers() -> eyre::Result<()> {
        let chain = seeded();
        // Build past two full difficulty periods (4032) plus a bit; the tip is
        // 4,132, so the mark advances to tip - 100 = 4,032.
        let run = easy_chain(
            &canonical(&chain, 0),
            2 * DIFFICULTY_PERIOD as usize + 100,
            1,
        );
        let out = chain.connect_headers(&run, NOW)?;
        assert_eq!(chain.tip_height()?, Some(2 * DIFFICULTY_PERIOD + 100));

        // The mark advanced automatically as part of the connect.
        assert_eq!(out.finalized, Some(2 * DIFFICULTY_PERIOD));
        assert_eq!(chain.finalized_height()?, Some(2 * DIFFICULTY_PERIOD));

        // The tail work is the first period after the checkpoint: work[2016 ..
        // 4032), computed on the fly.
        let expected = chain.chainwork_at(2 * DIFFICULTY_PERIOD)?.unwrap()
            - chain.chainwork_at(DIFFICULTY_PERIOD)?.unwrap();
        assert_eq!(chain.btc_tail_work(DIFFICULTY_PERIOD)?, Some(expected));
        assert!(expected > Work::from_be_bytes([0; 32]));

        // Headers BELOW the checkpoint are RETAINED (no pruning when it
        // finalizes; pruning happens later when RSK establishes a checkpoint).
        assert!(chain.header_at(1)?.is_some());
        assert!(chain.header_at(DIFFICULTY_PERIOD)?.is_some());
        assert!(
            chain
                .header_at(2 * DIFFICULTY_PERIOD + 100)
                .unwrap()
                .is_some()
        );

        // Finalizing again with no new period is a no-op.
        assert_eq!(chain.finalize_period(NOW)?, None);
        Ok(())
    }

    #[test]
    fn finalized_checkpoints_stop_at_the_floor() -> eyre::Result<()> {
        let chain = seeded();
        // Two full periods plus a bit: the mark is 4,032, which happens to be a
        // boundary, and the next boundary (6,048) is above it.
        let parent = canonical(&chain, 0);
        let run = easy_chain(&parent, 2 * DIFFICULTY_PERIOD as usize + 100, 1);
        chain.connect_headers(&run, NOW)?;

        let finalized = chain.finalized_height()?.expect("a finality mark");
        assert_eq!(finalized, 2 * DIFFICULTY_PERIOD);
        let cps = chain.finalized_checkpoints()?;

        // Every exported checkpoint is at or below the mark, and the newest one
        // *is* the mark here — never a boundary above it.
        assert_eq!(cps.first().map(|(h, _)| *h), Some(0));
        assert_eq!(cps.last().map(|(h, _)| *h), Some(finalized));
        assert!(cps.iter().all(|(h, _)| *h <= finalized));
        assert_eq!(cps.len() as u64, finalized / DIFFICULTY_PERIOD + 1);

        // The mark sits FINALITY_MARGIN_BLOCKS below the tip.
        let tip = chain.tip_height()?.expect("a tip");
        assert_eq!(tip, finalized + FINALITY_MARGIN_BLOCKS);
        Ok(())
    }

    #[test]
    fn checkpoint_spine_survives_pruning() -> eyre::Result<()> {
        let chain = seeded();
        let run = easy_chain(
            &canonical(&chain, 0),
            2 * DIFFICULTY_PERIOD as usize + 100,
            1,
        );
        // The connect advances the tail and does NOT prune.
        let out = chain.connect_headers(&run, NOW)?;
        assert_eq!(out.finalized, Some(2 * DIFFICULTY_PERIOD));

        // The checkpoint spine still resolves: height 0 (the seeded genesis)
        // survives, its hash maps to its height, and its work is intact.
        assert!(chain.header_at(0)?.is_some());
        let genesis_hash = canonical_hash(&chain, 0);
        assert_eq!(chain.height_of(&genesis_hash)?, Some(0));
        assert!(chain.store.read()?.work_at(0, 0)?.is_some());

        Ok(())
    }

    #[test]
    fn spine_work_scales_by_full_periods() -> eyre::Result<()> {
        // Two spine checkpoints, each an easy-target header with per-block work
        // `w`. The spine must report cumulative work as if every one of the
        // 2016 blocks in the intervening period were stored:
        //   work_at(0)     = w                    (just the boundary block)
        //   work_at(2016)  = w + 2015*w + w = 2017*w
        let chain = Core::in_memory(Network::Regtest).unwrap();
        let g0 = mine_genesis(0);
        let g2016 = mine_genesis(1);
        let w = header_work(&g0);
        chain
            .seed_checkpoints(&[(0, g0), (DIFFICULTY_PERIOD, g2016)])
            .unwrap();

        assert_eq!(chain.store.read()?.work_at(0, 0).unwrap(), Some(w));
        let expected = {
            let mut acc = w; // block 0
            for _ in 1..DIFFICULTY_PERIOD {
                acc = acc + w; // the rest of period 0
            }
            acc + header_work(&g2016) // this boundary block
        };
        assert_eq!(
            chain.store.read()?.work_at(0, DIFFICULTY_PERIOD).unwrap(),
            Some(expected)
        );
        Ok(())
    }

    #[test]
    fn real_genesis_is_a_valid_pow_checkpoint() -> eyre::Result<()> {
        let genesis = real_genesis();
        // The real mainnet genesis satisfies the difficulty-1 target.
        genesis
            .validate_pow(genesis.target())
            .expect("real genesis PoW");
        let chain = Core::in_memory(Network::Regtest).unwrap();
        chain.seed_checkpoints(&[(0, genesis)]).unwrap();
        assert_eq!(
            chain.header_at(0)?.unwrap().block_hash(),
            genesis.block_hash()
        );
        // And we can extend on top of the real genesis hash with synthetic
        // headers (continuity is what matters here, not difficulty).
        let run = mine_chain_from(&genesis, 2, EASY_BITS, 1);
        let out = chain.connect_headers(&run, NOW)?;
        assert_eq!(out.connected, 2);
        assert_eq!(out.tip_height, 2);
        Ok(())
    }

    #[test]
    fn finalizes_once_the_bar_is_cleared() -> eyre::Result<()> {
        // At constant difficulty the bar is a number of blocks above the anchor;
        // once the chain carries that many, the mark advances to the reorg depth.
        let chain = seeded();
        let checkpoint = DIFFICULTY_PERIOD as usize;

        // Build a chain to the checkpoint height: nothing to settle yet (the tip
        // has not buried a boundary by the margin).
        let run1 = easy_chain(&canonical(&chain, 0), checkpoint, 1);
        chain.connect_headers(&run1, NOW)?;
        assert_eq!(chain.tip_height()?, Some(checkpoint as u64));

        // Now extend past the checkpoint with blocks at the same difficulty.
        let run2 = easy_chain(&canonical(&chain, checkpoint as u64), checkpoint + 100, 1);
        let out = chain.connect_headers(&run2, NOW)?;
        assert_eq!(chain.tip_height()?, Some((2 * checkpoint + 100) as u64));

        // 4,132 blocks of work above the anchor clears the 4,000-block bar.
        assert_eq!(out.finalized, Some(2 * DIFFICULTY_PERIOD));
        assert_eq!(chain.finalized_height()?, Some(2 * DIFFICULTY_PERIOD));
        Ok(())
    }

    /// These store tests use the linear reference bar, `w0 · max(100, T/600)`;
    /// what gates the mark is the work above the anchor, not the number of
    /// blocks carrying it.
    #[test]
    fn the_bar_gates_finalization() -> eyre::Result<()> {
        let chain = seeded();
        let genesis = canonical(&chain, 0);
        let w = crate::btc::header_work(&genesis);

        // A clock 4,032 blocks past the anchor: the bar asks for 4,032 blocks
        // of work above it.
        let now = genesis.time as u64 + 4_032 * finality::TARGET_BLOCK_SECONDS;

        // 4,031 blocks of work: one short, so no boundary settles — and the
        // status says exactly why.
        let run = easy_chain(&genesis, 4_031, 1);
        let short = chain.connect_headers(&run, now)?;
        assert_eq!(chain.tip_height()?, Some(4_031));
        assert_eq!(short.finalized, None, "one block short must not settle");
        let status = short.finality.expect("the bar is measurable");
        assert_eq!(status.anchor, 0);
        assert_eq!(status.anchor_time, genesis.time as u64);
        assert_eq!(
            finality::work_to_u256(status.have),
            finality::work_to_u256(w) * alloy_primitives::U256::from(4_031u64),
            "work above the anchor, one short"
        );
        assert_eq!(
            status.need,
            finality::work_to_u256(w) * alloy_primitives::U256::from(4_032u64)
        );
        assert!(!status.clears());
        assert_eq!(chain.finalized_height()?, Some(0));

        // The last block tips it over: the mark advances to tip - 100.
        let last = mine_child(&canonical(&chain, 4_031), EASY_BITS, 1);
        let out = chain.connect_headers(&[last], now)?;
        let mark = 4_032 - FINALITY_MARGIN_BLOCKS;
        assert_eq!(out.finalized, Some(mark));
        assert!(out.finality.expect("the bar is measurable").clears());
        assert_eq!(chain.finalized_height()?, Some(mark));
        // The mark is not a boundary; the newest checkpoint is still 2,016.
        assert_eq!(chain.finalized_checkpoint()?, Some(DIFFICULTY_PERIOD));
        Ok(())
    }

    /// A fresh anchor asks for the coinbase window only: the estimate is zero
    /// at age zero, so the reference floor is the whole bar.
    #[test]
    fn a_fresh_anchor_asks_for_the_reversal_window() -> eyre::Result<()> {
        let chain = seeded();
        let genesis = canonical(&chain, 0);
        let w = crate::btc::header_work(&genesis);

        // Ten blocks, connected at the anchor's own timestamp (age zero).
        let run = easy_chain(&genesis, 10, 1);
        let out = chain.connect_headers(&run, genesis.time as u64)?;
        let status = out.finality.expect("the bar is measurable");
        assert_eq!(
            finality::work_to_u256(status.have),
            finality::work_to_u256(w) * alloy_primitives::U256::from(10u64)
        );
        assert_eq!(
            status.need,
            finality::work_to_u256(w) * alloy_primitives::U256::from(finality::REVERSAL_BLOCKS)
        );
        assert!(
            !status.clears(),
            "10 blocks is short of the 100-block floor"
        );
        Ok(())
    }

    /// The clock is the one unhashed input, so it may only move the bar up: a
    /// later connect with an earlier `now` sees the same bar.
    #[test]
    fn a_wound_back_clock_cannot_shrink_the_bar() -> eyre::Result<()> {
        let chain = seeded();
        let genesis = canonical(&chain, 0);
        let anchor_time = genesis.time as u64;

        let first = easy_chain(&genesis, 5, 1);
        let out = chain.connect_headers(&first, anchor_time + 86_400)?;
        let high = out.finality.expect("measurable").need;

        // A day earlier than the clock already seen.
        let second = mine_child(&canonical(&chain, 5), EASY_BITS, 1);
        let out = chain.connect_headers(&[second], anchor_time)?;
        assert_eq!(
            out.finality.expect("measurable").need,
            high,
            "the bar must not shrink when the clock winds back"
        );
        Ok(())
    }

    /// Finalization may not happen on a fork: the mark is only ever moved on the
    /// spine's own chain.
    #[test]
    fn a_heavier_fork_cannot_move_the_mark() -> eyre::Result<()> {
        let chain = seeded();
        let main = easy_chain(&canonical(&chain, 0), 5, 1);
        chain.connect_headers(&main, NOW)?;

        // A strictly heavier fork off height 3 (parent = main[2]).
        let fork = easy_chain(&main[2], 5, 1000);
        let out = chain.connect_headers(&fork, NOW)?;
        assert_ne!(out.best_fork, Some(0), "the fork is best");
        assert_eq!(out.finalized, None);
        assert_eq!(
            out.finality, None,
            "the bar is not measured on a fork's chain"
        );
        assert_eq!(chain.finalized_height()?, Some(0));
        Ok(())
    }

    /// The shipped estimated-work bar through the store: seed the real spine
    /// tip, ask for the bar a month later, and check it is a few thousand
    /// blocks — the model's value — rather than the linear age term.
    #[test]
    fn the_estimated_work_bar_is_used_by_the_store() -> eyre::Result<()> {
        let spine = crate::btc::checkpoints::parse()?;
        let (height, header) = *spine.last().expect("non-empty spine");
        let chain = Core::in_memory_with_bar(Network::Bitcoin, finality::estimated_work_bar)?;
        chain.seed_checkpoints(&[(height, header)])?;

        let now = header.time as u64 + 31 * 86_400;
        let status = chain.finality_status(now)?.expect("the bar is measurable");
        assert_eq!(status.anchor, height);
        assert_eq!(status.have_blocks(), alloy_primitives::U256::ZERO);
        let blocks = status.need_blocks();
        assert!(
            blocks > alloy_primitives::U256::from(2_000u64)
                && blocks < alloy_primitives::U256::from(3_500u64),
            "bar {blocks} blocks"
        );
        Ok(())
    }

    /// On resume a store adopts the newest shipped checkpoint it can prove it
    /// holds, so a binary with a newer spine moves an old mark forward instead
    /// of leaving the store stuck on the mark it was seeded with.
    #[test]
    fn a_resumed_store_adopts_the_newest_shipped_checkpoint_it_holds() -> eyre::Result<()> {
        let chain = seeded();
        let run = easy_chain(&canonical(&chain, 0), 4_032, 1);
        chain.connect_headers(&run, NOW)?;
        // The mark is at the reorg depth, not quantized to a boundary.
        let mark = 4_032 - FINALITY_MARGIN_BLOCKS;
        assert_eq!(chain.finalized_height()?, Some(mark));

        // A newer spine whose tip the store holds: adopt it.
        let trusted = vec![
            (0, canonical(&chain, 0)),
            (DIFFICULTY_PERIOD, canonical(&chain, DIFFICULTY_PERIOD)),
            (
                2 * DIFFICULTY_PERIOD,
                canonical(&chain, 2 * DIFFICULTY_PERIOD),
            ),
        ];
        assert_eq!(
            chain.reanchor_shipped(&trusted)?,
            Some((mark, 2 * DIFFICULTY_PERIOD))
        );
        assert_eq!(chain.finalized_height()?, Some(2 * DIFFICULTY_PERIOD));

        // A newer spine than that, but a header the store does not hold: the
        // mark must not move on an unverifiable claim.
        let beyond = vec![(4 * DIFFICULTY_PERIOD, mine_genesis(7))];
        assert_eq!(chain.reanchor_shipped(&beyond)?, None);
        assert_eq!(chain.finalized_height()?, Some(2 * DIFFICULTY_PERIOD));

        // Never backwards.
        assert_eq!(chain.reanchor_shipped(&trusted)?, None);
        Ok(())
    }

    /// The anchor *is* the mark: a returning node measures from its own
    /// finalized chain, and each advance re-anchors, so the bar is never
    /// measured from a stale fixed checkpoint.
    #[test]
    fn the_anchor_follows_the_mark() -> eyre::Result<()> {
        let chain = Core::in_memory(Network::Regtest)?;
        let g0 = mine_genesis(0);
        let g2016 = mine_child(&g0, EASY_BITS, 1);
        chain.seed_checkpoints(&[(0, g0), (DIFFICULTY_PERIOD, g2016)])?;

        // A clock 4,000 blocks past the seeded tip's time, and a run of 4,100
        // blocks: the bar asks for 4,000 blocks above the seeded mark (2016)
        // and the run carries 4,100, so the mark advances to 2016 + 4,100 - 100.
        let now = g2016.time as u64 + 4_000 * finality::TARGET_BLOCK_SECONDS;
        let run = easy_chain(&g2016, 4_100, 1);
        let out = chain.connect_headers(&run, now)?;
        let status = out.finality.expect("the bar is measurable");
        assert_eq!(status.anchor, DIFFICULTY_PERIOD);
        assert_eq!(status.anchor_time, g2016.time as u64);
        assert!(status.clears());
        assert_eq!(out.finalized, Some(6_016));

        // More headers, a later clock: the anchor is now the new mark, not the
        // seeded tip.
        let tip = chain.tip_height()?.expect("a tip");
        let more = easy_chain(&canonical(&chain, tip), 10, 1);
        let out = chain.connect_headers(&more, now + 6_000)?;
        assert_eq!(
            out.finality.expect("measurable").anchor,
            6_016,
            "the anchor must follow the mark"
        );
        Ok(())
    }

    #[test]
    fn rejects_timestamp_at_or_below_median_time_past() -> eyre::Result<()> {
        let chain = seeded();
        // 11 headers so a 12th has a full median-time-past window.
        let run = easy_chain(&canonical(&chain, 0), 11, 1);
        chain.connect_headers(&run, NOW)?;

        // A sibling of height 12 whose timestamp is below the median of the
        // previous 11 (the median is the 6th, well above the first block's time).
        let parent = canonical(&chain, 11);
        let too_early = canonical(&chain, 1).time;
        let bad = mine_at(parent.block_hash(), too_early, EASY_BITS);
        let err = chain.connect_headers(&[bad], NOW).unwrap_err();
        assert!(
            matches!(err, Error::BadTimestamp { height: 12, .. }),
            "got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn connecting_with_the_wrong_network_is_refused() -> eyre::Result<()> {
        let chain = Core::in_memory(Network::Regtest)?;
        chain.seed_checkpoints(&[(0, mine_genesis(0))])?;
        assert_eq!(chain.store.read()?.network()?, Some(Network::Regtest));

        // Re-open the same store believing it is mainnet: the recorded network
        // wins over the caller's claim.
        let Core { store, bar, .. } = chain;
        let chain = Core {
            store,
            network: Network::Bitcoin,
            rsk_chain: crate::rsk::chain_config(Network::Bitcoin),
            bar,
        };
        let header = mine_child(&canonical(&chain, 0), EASY_BITS, 1);
        let err = chain.connect_headers(&[header], NOW).unwrap_err();
        assert!(
            matches!(
                err,
                Error::NetworkMismatch {
                    seeded: Network::Regtest,
                    opened: Network::Bitcoin,
                }
            ),
            "got {err:?}"
        );
        Ok(())
    }

    // ── btc tail work tests ─────────────────────────────────────────────────

    /// The work is computed on the fly for the given tail start; not seeded,
    /// there is nothing to compute.
    #[test]
    fn btc_tail_work_none_without_seed() -> eyre::Result<()> {
        let chain = Core::in_memory(Network::Regtest)?;
        assert_eq!(chain.finalized_height()?, None);
        assert_eq!(chain.btc_tail_work(0)?, None);
        Ok(())
    }

    /// With less than a full period above the start there is no first period to
    /// measure.
    #[test]
    fn btc_tail_work_none_before_a_full_period() -> eyre::Result<()> {
        let chain = seeded();
        assert_eq!(chain.finalized_height()?.unwrap(), 0);

        // 100 blocks is less than a period: work[0 .. 2016) is not stored.
        let run = easy_chain(&canonical(&chain, 0), 100, 1);
        chain.connect_headers(&run, NOW)?;
        assert_eq!(chain.finalized_height()?.unwrap(), 0, "no finalize yet");
        assert_eq!(chain.btc_tail_work(0)?, None);
        Ok(())
    }

    /// After two full periods + 100 blocks the tail work at 2016 is the work of
    /// the first period after it, [2016 .. 4032).
    #[test]
    fn btc_tail_work_is_the_first_tail_period() -> eyre::Result<()> {
        let chain = seeded();
        let parent = canonical(&chain, 0);
        let n = 2 * DIFFICULTY_PERIOD as usize + 100;
        let run = mine_chain_from(&parent, n, EASY_BITS, 1);
        chain.connect_headers(&run, NOW)?;

        assert_eq!(chain.finalized_height()?.unwrap(), 2 * DIFFICULTY_PERIOD);
        let window = chain.chainwork_at(2 * DIFFICULTY_PERIOD)?.unwrap()
            - chain.chainwork_at(DIFFICULTY_PERIOD)?.unwrap();
        assert_eq!(chain.btc_tail_work(DIFFICULTY_PERIOD)?, Some(window));
        assert!(window > chain.chainwork_at(0)?.unwrap());
        Ok(())
    }

    /// The work is the first period *after* the tail start, at that period's own
    /// difficulty — not the (heavier) period before it. With a drop to work 20
    /// after the boundary, it is 2016 * 20, below the preceding ~2015 * 22.
    #[test]
    fn btc_tail_work_uses_the_period_after_the_start() -> eyre::Result<()> {
        let chain = seeded();
        let period = DIFFICULTY_PERIOD as usize;

        // Genesis has easy-target work. Period 0 blocks use work 22.
        let run1 = mine_chain_from(&canonical(&chain, 0), period, WORK22_BITS, 1);
        chain.connect_headers(&run1, NOW)?;

        // Tail after boundary 2016 at the lower work 20, enough to outweigh the
        // period before it and finalize.
        let tail_needed = (period * 22).div_ceil(20);
        let tail = mine_chain_from(
            &canonical(&chain, period as u64),
            tail_needed,
            WORK20_BITS,
            1,
        );
        chain.connect_headers(&tail, NOW)?;

        assert_eq!(
            chain.finalized_height()?.unwrap(),
            (period + tail_needed) as u64 - FINALITY_MARGIN_BLOCKS
        );
        let after = chain.chainwork_at(2 * DIFFICULTY_PERIOD)?.unwrap()
            - chain.chainwork_at(DIFFICULTY_PERIOD)?.unwrap();
        let before =
            chain.chainwork_at(DIFFICULTY_PERIOD)?.unwrap() - chain.chainwork_at(0)?.unwrap();
        assert_eq!(chain.btc_tail_work(DIFFICULTY_PERIOD)?, Some(after));
        assert!(after < before, "the drop makes the after-period lighter");
        Ok(())
    }
}
