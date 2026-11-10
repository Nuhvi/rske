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
use bitcoin::hashes::Hash as _;
use bitcoin::merkle_tree::MerkleBlock;
use bitcoin::transaction::Transaction;
use rsk_consensus::config::ChainConfig;

use crate::btc::finality;
use crate::error::Error;
use crate::rsk;
use crate::store::Store;
use crate::{ConnectOutcome, RskConnectOutcome, RskEntry, Tag};

/// The side-effect-free core: a thin, deterministic layer over a redb-backed
/// store.
///
/// No concurrency, no network, no clock, no hidden I/O — the caller supplies
/// data and reads back outcomes.
pub struct Core {
    pub(crate) store: Store,
    pub(crate) network: Network,
    /// Rootstock consensus parameters the RSK tree validates against.
    pub(crate) rsk_chain: Arc<ChainConfig>,
    /// The finality policy: how much work the chain since the mark must carry.
    pub(crate) bar: finality::Bar,
    /// How far below the tip the finality mark is kept. Production always uses
    /// [`crate::btc::FINALITY_MARGIN_BLOCKS`]; tests may shorten it so a small
    /// chain reaches the mark.
    pub(crate) margin: u64,
}

impl Core {
    /// Open a file-backed store for the given network.
    ///
    /// Mainnet uses the price-law estimated-work bar; other networks use the
    /// linear reference rule, because the price law is a mainnet calibration.
    pub fn open(path: &Path, network: Network) -> Result<Self, Error> {
        Ok(Self {
            store: Store::open(path)?,
            network,
            rsk_chain: rsk::chain_config(network),
            bar: bar_for(network),
            margin: crate::btc::FINALITY_MARGIN_BLOCKS,
        })
    }

    /// Open an in-memory store (tests, no disk I/O) for the given network.
    #[cfg(any(test, feature = "test-support"))]
    pub fn in_memory(network: Network) -> Result<Self, Error> {
        Self::in_memory_with_bar(network, bar_for(network))
    }

    /// Open an in-memory store with an explicit finality policy (tests).
    #[cfg(any(test, feature = "test-support"))]
    pub fn in_memory_with_bar(network: Network, bar: finality::Bar) -> Result<Self, Error> {
        Self::in_memory_with(network, bar, crate::btc::FINALITY_MARGIN_BLOCKS)
    }

    /// Open an in-memory store with an explicit finality policy and mark depth
    /// (tests only: a short margin lets a small chain exercise finality).
    #[cfg(any(test, feature = "test-support"))]
    pub fn in_memory_with(
        network: Network,
        bar: finality::Bar,
        margin: u64,
    ) -> Result<Self, Error> {
        Ok(Self {
            store: Store::in_memory()?,
            network,
            rsk_chain: rsk::chain_config(network),
            bar,
            margin,
        })
    }

    /// Empty the Bitcoin tables (tests: replay from a clean seed).
    #[cfg(feature = "test-support")]
    pub fn reset(&self) -> Result<(), Error> {
        self.store.reset_btc()
    }

    /// Empty the Rootstock tables, keeping the Bitcoin side intact.
    #[cfg(feature = "test-support")]
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
        now: u64,
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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

/// The finality policy for a network. The price law is calibrated on Bitcoin
/// mainnet; other networks fall back to the linear reference rule.
fn bar_for(network: Network) -> finality::Bar {
    match network {
        Network::Bitcoin => finality::estimated_work_bar,
        _ => finality::linear_reference_bar,
    }
}
