use bitcoin::Network;
use bitcoin::block::BlockHash;
use thiserror::Error;

/// Errors from the side-effect-free core.
///
/// Everything here is deterministic given the store contents and the inputs:
/// there is no network, no internal clock, no concurrency and no hidden I/O behind any
/// of these.
#[derive(Debug, Error)]
pub enum Error {
    #[error("database error: {0}")]
    Database(#[from] redb::DatabaseError),

    #[error("transaction error: {0}")]
    Transaction(#[from] redb::TransactionError),

    #[error("storage error: {0}")]
    Storage(#[from] redb::StorageError),

    #[error("table error: {0}")]
    Table(#[from] redb::TableError),

    #[error("commit error: {0}")]
    Commit(#[from] redb::CommitError),

    #[error("no chain seeded: the store has no anchor and the configuration supplied none")]
    Unseeded,

    #[error(
        "stored checkpoint at height {height} is {stored}; the configuration supplies {configured}"
    )]
    AnchorMismatch {
        height: u64,
        stored: BlockHash,
        configured: BlockHash,
    },

    #[error("malformed checkpoint line {line}: {detail}")]
    CheckpointFormat { line: usize, detail: String },

    #[error("unknown ancestor {0}: the batch does not link to any stored header or checkpoint")]
    UnknownAncestor(BlockHash),

    #[error("bad proof of work at height {height}: {detail}")]
    BadPow { height: u64, detail: String },

    #[error("wrong difficulty at height {height}: expected bits {expected:#010x}, got {got:#010x}")]
    BadDifficulty {
        height: u64,
        expected: u32,
        got: u32,
    },

    #[error("timestamp {got} at height {height} is not above the median-time-past {median}")]
    BadTimestamp { height: u64, median: u32, got: u32 },

    #[error("timestamp {got} at height {height} is more than {max_ahead} s after the clock {now}")]
    TimestampTooNew {
        height: u64,
        now: u64,
        got: u32,
        max_ahead: u64,
    },

    #[error("header at height {height} does not link to {expected}")]
    LinkBreak { height: u64, expected: BlockHash },

    #[error("chain was seeded for {seeded}, not {opened}")]
    NetworkMismatch { seeded: Network, opened: Network },

    #[error("fork at height {fork} is at or below the finality mark {finalized}")]
    ForkBelowFinalized { fork: u64, finalized: u64 },

    #[error("rootstock: {0}")]
    Rootstock(String),

    #[error("coinbase at height {height} is not for the canonical block {block_hash}")]
    CoinbaseNotCanonical { height: u64, block_hash: BlockHash },

    #[error("invalid coinbase/proof at height {height}: {detail}")]
    CoinbaseInvalid { height: u64, detail: String },

    #[error("stored coinbase at height {height} does not decode: {detail}")]
    CoinbaseDecode { height: u64, detail: String },

    #[error("internal: {0}")]
    Internal(String),
}
