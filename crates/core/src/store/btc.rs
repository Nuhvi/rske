//! Bitcoin chain state: the fork tree, the checkpoint spine, and the
//! cumulative-work ladder.
//!
//! The tables, their record types and the transaction methods that read and
//! write them all live here, so every Bitcoin persistence detail is in one
//! place.

use bitcoin::Network;
use bitcoin::block::BlockHash;
use bitcoin::block::Header as BitcoinHeader;
use bitcoin::consensus::{Decodable, Encodable};
use bitcoin::hashes::Hash;
use bitcoin::pow::Work;
use redb::{ReadableTable, TableDefinition, TypeName, Value, WriteTransaction};

use super::{DIFFICULTY_PERIOD, Meta, ReadTxn, WriteTxn};
use crate::error::Error;

/// Headers + cumulative work keyed by `(fork_id, height)`.
const BTC_HEADERS: TableDefinition<(u64, u64), BtcRow> = TableDefinition::new("btc_headers");
/// `hash → (fork_id, height)`.
const BTC_HASH_LOC: TableDefinition<&[u8], (u64, u64)> = TableDefinition::new("btc_hash_loc");
/// Fork metadata: `fork_id → ForkMeta`.
const BTC_FORKS: TableDefinition<u64, ForkMeta> = TableDefinition::new("btc_forks");
/// The checkpoint spine: one trusted header per difficulty period, never
/// pruned — the cheap whole-chain work ladder.
const BTC_CHECKPOINTS: TableDefinition<u64, BtcHeader> = TableDefinition::new("btc_checkpoints");
const BTC_META: TableDefinition<&str, Meta> = TableDefinition::new("btc_meta");

/// The finality **mark**: the newest header settled as un-reorgable. Any height,
/// advanced only forward; it is also the **anchor** the next bar is measured
/// from, and the period starts at or below it are the **checkpoints**. See
/// [`crate::btc::finality`] for the rule.
const META_FINALIZED: &str = "finalized_height";
/// The greatest clock ([`crate::Core::connect_headers`]'s `now`) ever seen, so
/// a later one cannot shrink the bar by winding the clock back.
const META_NOW: &str = "finality_now";
const META_BEST_FORK: &str = "best_fork";
const META_NEXT_FORK: &str = "next_fork";
const META_NETWORK: &str = "network";

/// Open every Bitcoin table (once, when the store is created).
pub(super) fn open_tables(txn: &WriteTransaction) -> Result<(), Error> {
    txn.open_table(BTC_HEADERS)?;
    txn.open_table(BTC_HASH_LOC)?;
    txn.open_table(BTC_FORKS)?;
    txn.open_table(BTC_CHECKPOINTS)?;
    txn.open_table(BTC_META)?;
    Ok(())
}

/// The Bitcoin reads the finality rule needs. Implemented for both transaction
/// types so the rule runs identically in a read snapshot (status output) and in
/// the writer (a connect).
pub(crate) trait BtcReads {
    fn best_fork(&self) -> Result<Option<u64>, Error>;
    fn fork_tip(&self, fork_id: u64) -> Result<Option<(u64, BlockHash)>, Error>;
    fn finalized_height(&self) -> Result<Option<u64>, Error>;
    fn header_at(&self, fork_id: u64, height: u64) -> Result<Option<BitcoinHeader>, Error>;
    fn work_at(&self, fork_id: u64, height: u64) -> Result<Option<Work>, Error>;
}

macro_rules! impl_btc_reads {
    ($txn:ty) => {
        impl BtcReads for $txn {
            fn best_fork(&self) -> Result<Option<u64>, Error> {
                <$txn>::best_fork(self)
            }

            fn fork_tip(&self, fork_id: u64) -> Result<Option<(u64, BlockHash)>, Error> {
                <$txn>::fork_tip(self, fork_id)
            }

            fn finalized_height(&self) -> Result<Option<u64>, Error> {
                <$txn>::finalized_height(self)
            }

            fn header_at(&self, fork_id: u64, height: u64) -> Result<Option<BitcoinHeader>, Error> {
                <$txn>::header_at(self, fork_id, height)
            }

            fn work_at(&self, fork_id: u64, height: u64) -> Result<Option<Work>, Error> {
                <$txn>::work_at(self, fork_id, height)
            }
        }
    };
}

impl_btc_reads!(ReadTxn);
impl_btc_reads!(WriteTxn);

/// A Bitcoin header row: the header and its cumulative work on its fork.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BtcRow {
    pub header: BitcoinHeader,
    pub work: Work,
}

impl Value for BtcRow {
    type SelfType<'a> = BtcRow;
    type AsBytes<'a> = [u8; 112];

    fn fixed_width() -> Option<usize> {
        Some(112)
    }

    fn from_bytes<'a>(data: &'a [u8]) -> BtcRow
    where
        Self: 'a,
    {
        let mut reader = &data[..80];
        let header =
            BitcoinHeader::consensus_decode(&mut reader).expect("stored header is always valid");
        let work = Work::from_be_bytes(data[80..112].try_into().expect("32-byte work"));
        BtcRow { header, work }
    }

    fn as_bytes<'a, 'b: 'a>(value: &'a BtcRow) -> [u8; 112]
    where
        Self: 'b,
    {
        let mut out = [0u8; 112];
        let mut buf = Vec::with_capacity(80);
        value
            .header
            .consensus_encode(&mut buf)
            .expect("encoding to vec is infallible");
        out[..80].copy_from_slice(&buf);
        out[80..].copy_from_slice(&value.work.to_be_bytes());
        out
    }

    fn type_name() -> TypeName {
        TypeName::new("rsk_core::BtcRow")
    }
}

/// A bare Bitcoin header (the checkpoint spine).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BtcHeader(pub BitcoinHeader);

impl Value for BtcHeader {
    type SelfType<'a> = BtcHeader;
    type AsBytes<'a> = [u8; 80];

    fn fixed_width() -> Option<usize> {
        Some(80)
    }

    fn from_bytes<'a>(data: &'a [u8]) -> BtcHeader
    where
        Self: 'a,
    {
        let mut reader = &data[..80];
        BtcHeader(
            BitcoinHeader::consensus_decode(&mut reader).expect("stored header is always valid"),
        )
    }

    fn as_bytes<'a, 'b: 'a>(value: &'a BtcHeader) -> [u8; 80]
    where
        Self: 'b,
    {
        let mut buf = Vec::with_capacity(80);
        value
            .0
            .consensus_encode(&mut buf)
            .expect("encoding to vec is infallible");
        buf.try_into().expect("80-byte header")
    }

    fn type_name() -> TypeName {
        TypeName::new("rsk_core::BtcHeader")
    }
}

/// Metadata for one Bitcoin chain (fork) in the tree. The root chain (id 0)
/// has `forkpoint == 0`, `parent == None`, and is the initial best chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ForkMeta {
    /// Height of this fork's first header.
    pub forkpoint: u64,
    /// The chain this one diverged from; `None` for a root fork.
    pub parent: Option<u64>,
    /// Hash of the block at `forkpoint` (this fork's identity).
    pub forkpoint_hash: BlockHash,
    /// Highest connected height on this fork.
    pub tip_height: u64,
    /// Hash of the block at `tip_height`.
    pub tip_hash: BlockHash,
    /// Cumulative work at `tip_height`.
    pub tip_work: Work,
}

impl Value for ForkMeta {
    type SelfType<'a> = ForkMeta;
    type AsBytes<'a> = [u8; 120];

    fn fixed_width() -> Option<usize> {
        Some(120)
    }

    fn from_bytes<'a>(data: &'a [u8]) -> ForkMeta
    where
        Self: 'a,
    {
        let parent = u64::from_be_bytes(data[8..16].try_into().expect("8 bytes"));
        let mut fp_hash = [0u8; 32];
        fp_hash.copy_from_slice(&data[16..48]);
        ForkMeta {
            forkpoint: u64::from_be_bytes(data[0..8].try_into().expect("8 bytes")),
            parent: (parent != NO_PARENT).then_some(parent),
            forkpoint_hash: BlockHash::from_byte_array(fp_hash),
            tip_height: u64::from_be_bytes(data[48..56].try_into().expect("8 bytes")),
            tip_hash: BlockHash::from_byte_array(data[56..88].try_into().expect("32 bytes")),
            tip_work: Work::from_be_bytes(data[80..112].try_into().expect("32 bytes")),
        }
    }

    fn as_bytes<'a, 'b: 'a>(value: &'a ForkMeta) -> [u8; 120]
    where
        Self: 'b,
    {
        let mut out = [0u8; 120];
        out[0..8].copy_from_slice(&value.forkpoint.to_be_bytes());
        out[8..16].copy_from_slice(&value.parent.unwrap_or(NO_PARENT).to_be_bytes());
        out[16..48].copy_from_slice(value.forkpoint_hash.as_byte_array());
        out[48..56].copy_from_slice(&value.tip_height.to_be_bytes());
        out[56..88].copy_from_slice(value.tip_hash.as_byte_array());
        out[88..120].copy_from_slice(&value.tip_work.to_be_bytes());
        out
    }

    fn type_name() -> TypeName {
        TypeName::new("rsk_core::ForkMeta")
    }
}

/// Sentinel `parent` for a root fork (`ForkMeta::parent == None`).
const NO_PARENT: u64 = u64::MAX;

/// Bitcoin reads shared by [`ReadTxn`] and [`WriteTxn`], so a connect can read
/// what it has already staged.
macro_rules! btc_read_methods {
    () => {
        pub(crate) fn fork_meta(&self, fork_id: u64) -> Result<Option<ForkMeta>, Error> {
            let table = self.txn.open_table(BTC_FORKS)?;
            Ok(table.get(fork_id)?.map(|v| v.value()))
        }

        /// The current best (heaviest) chain id.
        pub(crate) fn best_fork(&self) -> Result<Option<u64>, Error> {
            self.meta_u64(META_BEST_FORK)
        }

        /// The tip of a fork: `(height, hash)`.
        pub(crate) fn fork_tip(&self, fork_id: u64) -> Result<Option<(u64, BlockHash)>, Error> {
            Ok(self.fork_meta(fork_id)?.map(|m| (m.tip_height, m.tip_hash)))
        }

        /// Where `hash` lives, if stored: `(fork_id, height)`.
        pub(crate) fn locate(&self, hash: &BlockHash) -> Result<Option<(u64, u64)>, Error> {
            let table = self.txn.open_table(BTC_HASH_LOC)?;
            Ok(table
                .get(hash.as_byte_array().as_slice())?
                .map(|v| v.value()))
        }

        /// The header at `height` on `fork_id`'s chain: the fork's own row, or an
        /// ancestor's below its forkpoint.
        pub(crate) fn header_at(
            &self,
            fork_id: u64,
            height: u64,
        ) -> Result<Option<BitcoinHeader>, Error> {
            let Some(owner) = self.owner_fork(fork_id, height)? else {
                return Ok(None);
            };
            let table = self.txn.open_table(BTC_HEADERS)?;
            Ok(table.get((owner, height))?.map(|v| v.value().header))
        }

        /// The cumulative work at `height` on `fork_id`'s chain. Resolves the
        /// owning fork exactly as [`header_at`](Self::header_at) does.
        pub(crate) fn work_at(&self, fork_id: u64, height: u64) -> Result<Option<Work>, Error> {
            let Some(owner) = self.owner_fork(fork_id, height)? else {
                return Ok(None);
            };
            let table = self.txn.open_table(BTC_HEADERS)?;
            Ok(table.get((owner, height))?.map(|v| v.value().work))
        }

        /// The fork on `fork_id`'s ancestor chain that owns `height`: the first
        /// fork whose forkpoint is at or below it. `None` when even the root
        /// chain starts above `height`.
        fn owner_fork(&self, fork_id: u64, height: u64) -> Result<Option<u64>, Error> {
            let mut f = fork_id;
            loop {
                let Some(meta) = self.fork_meta(f)? else {
                    return Ok(None);
                };
                if height >= meta.forkpoint {
                    return Ok(Some(f));
                }
                let Some(parent) = meta.parent else {
                    return Ok(None);
                };
                f = parent;
            }
        }

        /// Cumulative work of a fork at its tip — the number chain selection
        /// compares across forks (Electrum's `get_chainwork`).
        pub(crate) fn fork_chainwork(&self, fork_id: u64) -> Result<Option<Work>, Error> {
            match self.fork_meta(fork_id)? {
                None => Ok(None),
                Some(m) => self.work_at(fork_id, m.tip_height),
            }
        }

        /// The finality mark, if seeded. Monotonic: only the connect path
        /// advances it, and only forward.
        pub(crate) fn finalized_height(&self) -> Result<Option<u64>, Error> {
            self.meta_u64(META_FINALIZED)
        }

        /// The greatest clock the finality rule has seen, if any.
        pub(crate) fn now_hwm(&self) -> Result<Option<u64>, Error> {
            self.meta_u64(META_NOW)
        }

        fn meta_u64(&self, key: &str) -> Result<Option<u64>, Error> {
            let table = self.txn.open_table(BTC_META)?;
            Ok(table.get(key)?.and_then(|v| v.value().as_u64()))
        }
    };
}

impl ReadTxn {
    btc_read_methods!();

    /// Number of live forks (test-only).
    #[cfg(test)]
    pub(crate) fn fork_count(&self) -> Result<u64, Error> {
        let table = self.txn.open_table(BTC_FORKS)?;
        Ok(table.iter()?.count() as u64)
    }

    /// The network the chain was seeded for, if any (test-only).
    #[cfg(test)]
    pub(crate) fn network(&self) -> Result<Option<Network>, Error> {
        let table = self.txn.open_table(BTC_META)?;
        Ok(table
            .get(META_NETWORK)?
            .and_then(|v| v.value().as_text().and_then(|t| t.parse().ok())))
    }
}

impl WriteTxn {
    btc_read_methods!();

    /// The network the chain was seeded for, if any. Recorded at seed time so
    /// a resume with the wrong network is refused rather than silently
    /// validated against the wrong consensus parameters.
    pub(crate) fn network(&self) -> Result<Option<Network>, Error> {
        let table = self.txn.open_table(BTC_META)?;
        Ok(table
            .get(META_NETWORK)?
            .and_then(|v| v.value().as_text().and_then(|t| t.parse().ok())))
    }

    /// Plant the trust anchor: one header per difficulty period (the
    /// checkpoint spine from this crate's embedded `checkpoints.txt`), taken on faith
    /// without validating ancestors, and create the root chain on top of it.
    ///
    /// This is a one-time operation: it is refused ([`Error::AlreadySeeded`])
    /// if the store already carries a chain, so it can never clobber a taller
    /// tip or a more advanced finality floor. Resume by connecting the headers
    /// above the existing tip.
    ///
    /// Because difficulty is constant within a period, each checkpoint header
    /// pins the work of its whole 2016-block period, so cumulative work is
    /// known at every boundary without holding every block. The entries must be
    /// one period apart (contiguous, ascending), starting at a period boundary.
    pub(crate) fn seed_checkpoints(&self, entries: &[(u64, BitcoinHeader)]) -> Result<(), Error> {
        if self.fork_meta(0)?.is_some() {
            return Err(Error::AlreadySeeded);
        }
        if entries.is_empty() {
            return Err(Error::Internal("no checkpoints to seed".into()));
        }
        let mut sorted = entries.to_vec();
        sorted.sort_by_key(|(h, _)| *h);
        for w in sorted.windows(2) {
            if w[1].0 != w[0].0 + DIFFICULTY_PERIOD {
                return Err(Error::Internal(format!(
                    "checkpoints must be contiguous difficulty periods: {} then {}",
                    w[0].0, w[1].0
                )));
            }
        }

        let mut headers = self.txn.open_table(BTC_HEADERS)?;
        let mut hash_loc = self.txn.open_table(BTC_HASH_LOC)?;
        let mut forks = self.txn.open_table(BTC_FORKS)?;
        let mut checkpoints = self.txn.open_table(BTC_CHECKPOINTS)?;
        let mut meta = self.txn.open_table(BTC_META)?;
        let mut cum = Work::from_be_bytes([0; 32]);
        let mut prev_w = Work::from_be_bytes([0; 32]);
        for (i, (height, header)) in sorted.iter().enumerate() {
            let w = header.target().to_work();
            cum = if i == 0 {
                w // just the boundary block itself
            } else {
                // Period (i-1) had `DIFFICULTY_PERIOD` blocks at `prev_w`;
                // the boundary block `prev_w` was already counted, so add
                // the remaining `DIFFICULTY_PERIOD - 1` plus this block.
                cum + work_scaled(prev_w, DIFFICULTY_PERIOD - 1) + w
            };
            prev_w = w;
            checkpoints.insert(*height, BtcHeader(*header))?;
            hash_loc.insert(header.block_hash().as_byte_array().as_slice(), (0, *height))?;
            // The spine doubles as the root chain's headers at these
            // heights, with cumulative work.
            headers.insert(
                (0, *height),
                BtcRow {
                    header: *header,
                    work: cum,
                },
            )?;
        }
        let last = sorted.last().expect("checked non-empty above").0;
        let last_hash = sorted
            .last()
            .expect("checked non-empty above")
            .1
            .block_hash();
        let root = ForkMeta {
            forkpoint: 0,
            parent: None,
            forkpoint_hash: sorted[0].1.block_hash(),
            tip_height: last,
            tip_hash: last_hash,
            tip_work: cum,
        };
        forks.insert(0, root)?;
        meta.insert(META_FINALIZED, Meta::U64(last))?;
        meta.insert(META_BEST_FORK, Meta::U64(0))?;
        meta.insert(META_NEXT_FORK, Meta::U64(1))?;
        Ok(())
    }

    /// Create a new fork diverging from `parent` at `forkpoint`; the block at
    /// `forkpoint` is `forkpoint_hash`. Returns the new fork id.
    ///
    /// The new fork's tip starts empty (`tip_height = forkpoint - 1`, pointing
    /// at its parent block) and is overwritten by the first `put_headers`.
    pub(crate) fn create_fork(
        &self,
        parent: u64,
        forkpoint: u64,
        forkpoint_hash: BlockHash,
    ) -> Result<u64, Error> {
        let id = {
            let mut meta = self.txn.open_table(BTC_META)?;
            let next = meta
                .get(META_NEXT_FORK)?
                .and_then(|v| v.value().as_u64())
                .unwrap_or(1);
            meta.insert(META_NEXT_FORK, Meta::U64(next + 1))?;
            next
        };
        let meta = ForkMeta {
            forkpoint,
            parent: Some(parent),
            forkpoint_hash,
            tip_height: forkpoint.saturating_sub(1),
            tip_hash: BlockHash::all_zeros(),
            tip_work: Work::from_be_bytes([0; 32]),
        };
        {
            let mut forks = self.txn.open_table(BTC_FORKS)?;
            forks.insert(id, meta)?;
        }
        Ok(id)
    }

    /// Append a contiguous run of headers to a fork in this transaction. Each
    /// entry carries its cumulative work (through that header, on this fork).
    /// Period-boundary heights on the root chain also extend the checkpoint
    /// spine.
    pub(crate) fn put_headers(
        &self,
        fork_id: u64,
        entries: &[(u64, &BitcoinHeader, Work)],
    ) -> Result<(), Error> {
        if entries.is_empty() {
            return Ok(());
        }
        {
            let mut headers = self.txn.open_table(BTC_HEADERS)?;
            let mut hash_loc = self.txn.open_table(BTC_HASH_LOC)?;
            let mut checkpoints = self.txn.open_table(BTC_CHECKPOINTS)?;
            let mut forks = self.txn.open_table(BTC_FORKS)?;
            for &(height, header, cum) in entries {
                headers.insert(
                    (fork_id, height),
                    BtcRow {
                        header: *header,
                        work: cum,
                    },
                )?;
                hash_loc.insert(
                    header.block_hash().as_byte_array().as_slice(),
                    (fork_id, height),
                )?;
                // Extend the spine when the root chain crosses a period
                // boundary (the spine stays a full-chain work ladder).
                if fork_id == 0 && height % DIFFICULTY_PERIOD == 0 {
                    checkpoints.insert(height, BtcHeader(*header))?;
                }
            }
            let last = entries.last().expect("checked non-empty above");
            let last_hash = last.1.block_hash();
            let mut meta = forks.get(fork_id)?.map(|v| v.value());
            if let Some(m) = meta.as_mut() {
                m.tip_height = last.0;
                m.tip_hash = last_hash;
                m.tip_work = last.2;
            }
            let meta = meta.expect("fork meta must exist before appending headers");
            forks.insert(fork_id, meta)?;
        }
        Ok(())
    }

    /// Mark `fork_id` as the best (heaviest) chain.
    pub(crate) fn set_best_fork(&self, fork_id: u64) -> Result<(), Error> {
        let mut meta = self.txn.open_table(BTC_META)?;
        meta.insert(META_BEST_FORK, Meta::U64(fork_id))?;
        Ok(())
    }

    /// Record the network this chain is seeded for (read back by the network
    /// check at the top of [`crate::Core::connect_headers`]).
    pub(crate) fn set_network(&self, network: Network) -> Result<(), Error> {
        let mut meta = self.txn.open_table(BTC_META)?;
        meta.insert(META_NETWORK, Meta::Text(network.to_string()))?;
        Ok(())
    }

    /// Record the finality mark.
    pub(crate) fn set_finalized(&self, height: u64) -> Result<(), Error> {
        let mut meta = self.txn.open_table(BTC_META)?;
        meta.insert(META_FINALIZED, Meta::U64(height))?;
        Ok(())
    }

    /// Record the greatest clock the finality rule has seen.
    pub(crate) fn set_now_hwm(&self, now: u64) -> Result<(), Error> {
        let mut meta = self.txn.open_table(BTC_META)?;
        meta.insert(META_NOW, Meta::U64(now))?;
        Ok(())
    }
}

/// `work` added to itself `n` times. (`Work` has no `Mul`, and the per-period
/// scaling only ever happens at seed time, so a loop is fine.)
fn work_scaled(work: Work, n: u64) -> Work {
    let mut acc = Work::from_be_bytes([0; 32]);
    for _ in 0..n {
        acc = acc + work;
    }
    acc
}
