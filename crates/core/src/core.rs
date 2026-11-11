//! The [`Core`] handle: one redb-backed store, opened once and shared by the
//! Bitcoin and Rootstock trees.
//!
//! This module owns the handle, how it is built, and the two things that make
//! the core a state machine rather than a bag of methods:
//!
//! * [`Action`] — every state-changing operation the node can ask for. The I/O
//!   driver turns a network event into one of these and calls [`Core::apply`];
//!   the public mutators are thin wrappers around the same call.
//! * [`CoreState`] — the whole stored state, read in one transaction. Tests use
//!   it to deduplicate explored states; a reader uses it to see the tree.
//!
//! The behaviour lives with the domain: [`crate::btc`] and [`crate::rsk`].

use std::path::Path;
use std::sync::Arc;

use alloy_primitives::B256;
use bitcoin::Network;
use bitcoin::block::{BlockHash, Header as BitcoinHeader};
use bitcoin::consensus::Params;
use bitcoin::hashes::Hash as _;
use bitcoin::merkle_tree::MerkleBlock;
use bitcoin::transaction::Transaction;
use rsk_consensus::config::ChainConfig;

use crate::error::Error;
use crate::rsk;
use crate::store::{ForkMeta, ReadTxn, Store};
use crate::{ConnectOutcome, RskConnectOutcome, RskEntry, Tag};

/// The side-effect-free core: a thin, deterministic layer over a redb-backed
/// store.
///
/// No concurrency, no network, no internal clock, no hidden I/O — the caller
/// supplies data, time and reads back outcomes.
pub struct Core {
    pub(crate) store: Store,
    /// Bitcoin consensus parameters used for header validation. The network
    /// they name also selects the finality policy, so there is no second copy
    /// of the network on the handle.
    pub(crate) params: Params,
    /// Rootstock consensus parameters the RSK tree validates against.
    pub(crate) rsk_chain: Arc<ChainConfig>,
}

impl Core {
    /// Open a file-backed store for the given network.
    ///
    /// The finality rule is selected by the network recorded in the store:
    /// mainnet prices elapsed time, every other network falls back to the tip's
    /// difficulty.
    pub fn open(path: &Path, network: Network) -> Result<Self, Error> {
        Ok(Self {
            store: Store::open(path)?,
            params: Params::new(network),
            rsk_chain: rsk::chain_config(network),
        })
    }

    /// Open an in-memory store (no disk I/O) for the given network.
    pub fn in_memory(network: Network) -> Result<Self, Error> {
        Ok(Self {
            store: Store::in_memory()?,
            params: Params::new(network),
            rsk_chain: rsk::chain_config(network),
        })
    }

    /// Open an in-memory store with explicit consensus parameters. The network
    /// they name selects the finality policy and the Rootstock configuration.
    pub fn in_memory_with_params(params: Params) -> Result<Self, Error> {
        Ok(Self {
            store: Store::in_memory()?,
            rsk_chain: rsk::chain_config(params.network),
            params,
        })
    }

    /// Drop every Bitcoin table (destructive: replay from a clean seed).
    pub fn reset(&self) -> Result<(), Error> {
        self.store.reset_btc()
    }

    /// Drop every Rootstock table, keeping the Bitcoin side intact.
    pub fn reset_rsk(&self) -> Result<(), Error> {
        self.store.reset_rsk()
    }

    /// The single entry point for every state change.
    ///
    /// The node's I/O driver maps a network event to one [`Action`]; the public
    /// mutators on [`Core`] do the same and unwrap the expected [`Outcome`].
    /// Every action runs in its own transaction, so it is all-or-nothing.
    pub fn apply(&self, action: Action<'_>) -> Result<Outcome, Error> {
        Ok(match action {
            Action::SeedCheckpoints { entries } => {
                self.seed_checkpoints_impl(entries)?;
                Outcome::Seeded
            }
            Action::ReanchorShipped { trusted } => {
                Outcome::Reanchored(self.reanchor_shipped_impl(trusted)?)
            }
            Action::ConnectHeaders { headers, now } => {
                Outcome::Connected(self.connect_headers_impl(headers, now)?)
            }
            Action::StoreCoinbase {
                height,
                block_hash,
                coinbase,
                merkle_block,
            } => {
                self.store_coinbase_impl(height, block_hash, coinbase, merkle_block)?;
                Outcome::CoinbaseStored
            }
            Action::SetCoinbaseTag { height, tag } => {
                Outcome::CoinbaseTagUpdated(self.set_coinbase_tag_impl(height, tag)?)
            }
            Action::PruneCoinbases => Outcome::CoinbasesPruned(self.prune_coinbases_impl()?),
            Action::ProposeCheckpoint { entry } => {
                Outcome::CheckpointProposed(self.propose_checkpoint_impl(entry)?)
            }
            Action::ConnectRskHeaders { entries } => {
                Outcome::RskConnected(self.connect_rsk_headers_impl(entries)?)
            }
            Action::ConnectRskHeadersWithoutUncles { entries } => {
                Outcome::RskConnected(self.connect_rsk_headers_without_uncles_impl(entries)?)
            }
            Action::SetRskSyncTip { hash } => {
                self.set_rsk_sync_tip_impl(hash)?;
                Outcome::SyncTipSet
            }
        })
    }

    /// The whole stored state, read in one transaction.
    ///
    /// This is what a reader wants and what a test uses to deduplicate explored
    /// states: the canonical chain alone is not enough, because a store holding
    /// a blocked fork looks identical to one without it.
    ///
    /// This is a point-in-time snapshot, not a streaming read. For the row
    /// collections that can grow without bound, use the iterators
    /// ([`Core::stored_headers`], [`Core::canonical_headers`],
    /// [`Core::finalized_checkpoints`], [`Core::coinbase_tags`],
    /// [`Core::rsk_headers_back`]).
    pub fn state(&self) -> Result<CoreState, Error> {
        let txn = self.store.read()?;
        let mut forks: Vec<ForkState> = txn
            .all_forks()?
            .into_iter()
            .map(|(id, meta)| ForkState {
                id,
                forkpoint: meta.forkpoint,
                parent: meta.parent,
                tip_height: meta.tip_height,
                tip_hash: meta.tip_hash.to_byte_array(),
                tip_work: meta.tip_work.to_be_bytes(),
            })
            .collect();
        forks.sort_by_key(|fork| fork.id);

        let mut roots = txn.rsk_roots()?;
        roots.sort_unstable();

        Ok(CoreState {
            network: txn.network()?,
            best_fork: txn.best_fork()?,
            finalized: txn.finalized_height()?,
            now_hwm: txn.now_hwm()?,
            forks,
            rootstock: RskState {
                forest: txn.rsk_all_hashes()?,
                roots,
                sync_tip: txn.rsk_sync_meta()?,
                sync_tip_header: txn.rsk_sync_tip()?,
            },
        })
    }

    /// Every stored Bitcoin header and the fork/height it occupies.
    ///
    /// [`CoreState`] reports fork metadata, not the rows themselves: a header
    /// on a blocked fork is invisible through the canonical readers. This is
    /// the observability view a model-based test needs to compare its predicted
    /// stored set with the real one.
    ///
    /// The rows are streamed from the read snapshot: a caller that only needs
    /// the first few does not pay for the whole tree.
    pub fn stored_headers(&self) -> Result<StoredHeaders, Error> {
        let txn = self.store.read()?;
        let forks = txn.all_forks()?;
        Ok(StoredHeaders {
            txn,
            forks: forks.into_iter(),
            current: None,
        })
    }

    /// The canonical chain, oldest first, streamed from one read snapshot.
    ///
    /// Yields an [`Error::Internal`] once if a canonical height has no stored
    /// row. A spine-only seed (for example checkpoints at 0 and 2016 with no
    /// blocks between them) has such gaps; a full chain does not.
    pub fn canonical_headers(&self) -> Result<CanonicalHeaders, Error> {
        let txn = self.store.read()?;
        let Some(best) = txn.best_fork()? else {
            return Ok(CanonicalHeaders::empty());
        };
        let Some((tip, _)) = txn.fork_tip(best)? else {
            return Ok(CanonicalHeaders::empty());
        };
        Ok(CanonicalHeaders {
            txn: Some(txn),
            best,
            next: 0,
            tip,
        })
    }

    /// Where a stored block lives, canonical or not.
    pub fn locate(&self, hash: &BlockHash) -> Result<Option<(u64, u64)>, Error> {
        self.store.read()?.locate(hash)
    }
}

/// Every state-changing operation the node can ask the core to perform.
#[derive(Debug)]
pub enum Action<'a> {
    /// Plant the trusted Bitcoin spine (once): one header per period boundary.
    SeedCheckpoints { entries: &'a [(u64, BitcoinHeader)] },
    /// Adopt the newest shipped Bitcoin period boundary the store already holds.
    ReanchorShipped { trusted: &'a [(u64, BitcoinHeader)] },
    /// Connect a run of Bitcoin headers at the caller's clock.
    ConnectHeaders {
        headers: &'a [BitcoinHeader],
        /// The caller's clock, when it has one. `None` skips the future-time
        /// check and runs the requirement on the stored high-water mark and
        /// the best tip's header time.
        now: Option<u64>,
    },
    /// Store a canonical coinbase and its inclusion proof.
    StoreCoinbase {
        height: u64,
        block_hash: BlockHash,
        coinbase: &'a Transaction,
        merkle_block: &'a MerkleBlock,
    },
    /// Backfill the cached tag on a coinbase row.
    SetCoinbaseTag { height: u64, tag: Option<Tag> },
    /// Drop coinbases below the retention floor.
    PruneCoinbases,
    /// Propose an RSK checkpoint block and make it a forest root.
    ProposeCheckpoint { entry: &'a RskEntry },
    /// Validate and store a contiguous RSK run with its uncle lists.
    ConnectRskHeaders { entries: &'a [RskEntry] },
    /// Validate and store a contiguous RSK run without uncle bodies.
    ConnectRskHeadersWithoutUncles { entries: &'a [RskEntry] },
    /// Record the newest committed RSK header.
    SetRskSyncTip { hash: B256 },
}

/// What [`Core::apply`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Seeded,
    Reanchored(Option<(u64, u64)>),
    Connected(ConnectOutcome),
    CoinbaseStored,
    CoinbaseTagUpdated(bool),
    CoinbasesPruned(usize),
    CheckpointProposed(B256),
    RskConnected(RskConnectOutcome),
    SyncTipSet,
}

/// The whole stored state, as [`Core::state`] reports it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CoreState {
    pub network: Option<Network>,
    pub best_fork: Option<u64>,
    pub finalized: Option<u64>,
    pub now_hwm: Option<u64>,
    pub forks: Vec<ForkState>,
    pub rootstock: RskState,
}

/// One stored Bitcoin header, with the fork and height it occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StoredHeader {
    pub fork: u64,
    pub height: u64,
    pub hash: BlockHash,
}

/// Streams every stored Bitcoin header, one row at a time.
///
/// Created by [`Core::stored_headers`]. Rows are grouped by fork and ascending
/// height. The underlying read snapshot is held for the life of the iterator.
pub struct StoredHeaders {
    txn: ReadTxn,
    forks: std::vec::IntoIter<(u64, ForkMeta)>,
    current: Option<(u64, u64, u64)>, // (fork, next height, tip height)
}

impl Iterator for StoredHeaders {
    type Item = Result<StoredHeader, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some((fork, height, tip)) = self.current {
                if height <= tip {
                    self.current = Some((fork, height + 1, tip));
                    match self.txn.header_at(fork, height) {
                        Ok(Some(header)) => {
                            return Some(Ok(StoredHeader {
                                fork,
                                height,
                                hash: header.block_hash(),
                            }));
                        }
                        Ok(None) => continue,
                        Err(error) => {
                            self.current = None;
                            self.forks = Vec::new().into_iter();
                            return Some(Err(error));
                        }
                    }
                }
                self.current = None;
            }

            let (fork, meta) = self.forks.next()?;
            if meta.tip_height >= meta.forkpoint {
                self.current = Some((fork, meta.forkpoint, meta.tip_height));
            }
        }
    }
}

/// Streams the canonical Bitcoin chain, oldest first.
///
/// Created by [`Core::canonical_headers`]. Holds one read snapshot while it
/// yields rows.
pub struct CanonicalHeaders {
    txn: Option<ReadTxn>,
    best: u64,
    next: u64,
    tip: u64,
}

impl CanonicalHeaders {
    fn empty() -> Self {
        Self {
            txn: None,
            best: 0,
            next: 0,
            tip: 0,
        }
    }
}

impl Iterator for CanonicalHeaders {
    type Item = Result<BitcoinHeader, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let txn = self.txn.as_ref()?;
        if self.next > self.tip {
            self.txn = None;
            return None;
        }
        let height = self.next;
        self.next += 1;
        match txn.header_at(self.best, height) {
            Ok(Some(header)) => Some(Ok(header)),
            Ok(None) => {
                self.txn = None;
                Some(Err(Error::Internal(format!(
                    "missing canonical header at height {height}"
                ))))
            }
            Err(error) => {
                self.txn = None;
                Some(Err(error))
            }
        }
    }
}

/// One Bitcoin fork: its identity, parent and tip.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ForkState {
    pub id: u64,
    pub forkpoint: u64,
    pub parent: Option<u64>,
    pub tip_height: u64,
    pub tip_hash: [u8; 32],
    pub tip_work: [u8; 32],
}

/// The Rootstock forest: stored headers, proposed roots and the sync tip.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct RskState {
    /// Every stored header hash, sorted.
    pub forest: Vec<[u8; 32]>,
    /// Every proposed RSK checkpoint root, sorted.
    pub roots: Vec<[u8; 32]>,
    /// The recorded sync tip exactly as stored (may name an unstored header).
    pub sync_tip: Option<[u8; 32]>,
    /// What `rsk_sync_tip` resolves to: a stored header, or nothing.
    pub sync_tip_header: Option<([u8; 32], u64)>,
}
