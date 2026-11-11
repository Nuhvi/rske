//! # rsk-core — the side-effect-free core
//!
//! The deterministic heart of Rske's fast-IBD plan: keep the Bitcoin
//! canonical chain, finalize it at difficulty-period boundaries, and carry
//! Rootstock header trees rooted at trusted anchors in the same store.
//!
//! No concurrency, no network, no internal clock, no hidden I/O. Time is an
//! input, never a hidden dependency. The only state is a
//! redb-backed store, reached only through a transaction: [`Core`] opens a read
//! snapshot or the writer at the start of each method.
//!

// Tests read/write a deterministic in-memory store and conventionally use
// `unwrap`; keep the strict lint for production code only.
#![cfg_attr(test, allow(clippy::unwrap_used))]
// The error enum deliberately mirrors redb's error types (`thiserror`), which
// makes `Result<_, Error>` larger than clippy's default threshold. That shape
// is intentional (see CONTRIBUTING.md); every fallible method returns it.
#![allow(clippy::result_large_err)]

pub mod btc;
mod core;
pub mod error;
pub mod rsk;
mod store;

#[cfg(test)]
#[path = "../specs/mod.rs"]
mod specs;

pub use btc::finality::{COINBASE_MATURITY, FinalityStatus, is_checkpoint};
pub use btc::validation::MEDIAN_TIME_PAST;
pub use btc::{
    Coinbase, CoinbaseTag, CoinbaseTags, ConnectOutcome, FinalizedCheckpoints, bip34_height,
    header_work,
};
pub use core::{
    Action, CanonicalHeaders, Core, CoreState, ForkState, Outcome, RskState, StoredHeader,
    StoredHeaders,
};
pub use error::Error;
pub use rsk::{
    EphemeralChainCheckpoint, FIVE_SIGMA_ALPHA, HeaderRef, RootScore, RskConnectOutcome, RskEntry,
    RskHeadersBack, Scoreboard, Share, WORK_EQUIVALENT_BLOCKS, min_tags_for_fraud_proof,
    oldest_checkpoint, required_endorsements, score,
};
pub use rsk_consensus::validation::merged_mining::Tag;
pub use store::DIFFICULTY_PERIOD;
