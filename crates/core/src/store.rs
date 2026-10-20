//! Persistent Bitcoin + Rootstock chain state, backed by redb.
//!
//! This is the *state* of the side-effect-free core. The Bitcoin side is a
//! **tree of chains** (forks), each with a `forkpoint` and a `parent`, plus a
//! trusted **checkpoint spine** (one header per difficulty period) that makes
//! the whole chain's work computable without holding every block. The Rootstock
//! side is a **forest** of trees, each rooted at a proposed checkpoint block.
//! Header validation lives in [`crate::rsk`] and [`crate::btc`]. Tests construct
//! the store with the redb **in-memory** backend, so the exact same code path
//! runs with zero disk I/O.
//!
//! The store is only ever touched through a transaction: [`Store::read`] opens
//! a consistent snapshot ([`ReadTxn`]) and [`Store::write`] opens the single
//! writer ([`WriteTxn`]). A read-modify-write (in particular, connecting
//! headers) therefore runs entirely inside one [`WriteTxn`]: redb serialises
//! writers, so nothing can change between the reads a decision is based on and
//! the writes that act on it.
//!
//! This module owns the handle and the transaction types. The domain tables,
//! their records and the methods over them live in [`btc`] and [`rsk`], split
//! by the side of the chain they belong to.

use std::path::Path;

use redb::{Database, ReadTransaction, TypeName, Value, WriteTransaction};

mod btc;
mod rsk;

pub(crate) use btc::BtcReads;

use crate::error::Error;

/// Bitcoin difficulty period, in blocks. Difficulty retargets only at period
/// boundaries, so every block in a period carries the same work.
pub const DIFFICULTY_PERIOD: u64 = 2016;

/// A metadata value: a small tagged union so one meta table serves every key,
/// instead of each call site encoding and decoding raw bytes. Shared by the
/// Bitcoin and Rootstock meta tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Meta {
    U64(u64),
    Text(String),
}

impl Meta {
    pub(crate) fn as_u64(&self) -> Option<u64> {
        match self {
            Meta::U64(value) => Some(*value),
            Meta::Text(_) => None,
        }
    }

    pub(crate) fn as_text(&self) -> Option<&str> {
        match self {
            Meta::Text(text) => Some(text),
            Meta::U64(_) => None,
        }
    }
}

impl Value for Meta {
    type SelfType<'a> = Meta;
    type AsBytes<'a> = Vec<u8>;

    fn fixed_width() -> Option<usize> {
        None
    }

    fn from_bytes<'a>(data: &'a [u8]) -> Meta
    where
        Self: 'a,
    {
        match data.first() {
            Some(0) => Meta::U64(u64::from_be_bytes(
                data[1..9].try_into().expect("8-byte meta"),
            )),
            Some(1) => Meta::Text(String::from_utf8_lossy(&data[1..]).into_owned()),
            _ => panic!("stored meta value has an unknown tag"),
        }
    }

    fn as_bytes<'a, 'b: 'a>(value: &'a Meta) -> Vec<u8>
    where
        Self: 'b,
    {
        let mut out = Vec::new();
        match value {
            Meta::U64(number) => {
                out.push(0);
                out.extend_from_slice(&number.to_be_bytes());
            }
            Meta::Text(text) => {
                out.push(1);
                out.extend_from_slice(text.as_bytes());
            }
        }
        out
    }

    fn type_name() -> TypeName {
        TypeName::new("rsk_core::Meta")
    }
}

/// redb-backed chain state, threaded by [`crate::Core`]. It is only reachable
/// through [`Store::read`] and [`Store::write`]: there are no per-call read or
/// write shortcuts, so every operation declares up front whether it takes a
/// read snapshot or the writer.
pub(crate) struct Store {
    db: Database,
}

impl Store {
    /// Open (creating if needed) a file-backed store at `path`.
    pub(crate) fn open(path: &Path) -> Result<Self, Error> {
        let db = Database::create(path)?;
        Self::new(db)
    }

    /// Create a store over the redb in-memory backend. No disk I/O: this is
    /// what tests use, and what "I control everything going on" means.
    #[cfg(test)]
    pub(crate) fn in_memory() -> Result<Self, Error> {
        let db = Database::builder().create_with_backend(redb::backends::InMemoryBackend::new())?;
        Self::new(db)
    }

    fn new(db: Database) -> Result<Self, Error> {
        let txn = db.begin_write()?;
        btc::open_tables(&txn)?;
        rsk::open_tables(&txn)?;
        txn.commit()?;
        Ok(Self { db })
    }

    /// Begin a consistent read snapshot. Every read through it sees one
    /// committed state, and any number of readers may run concurrently.
    pub(crate) fn read(&self) -> Result<ReadTxn, Error> {
        Ok(ReadTxn {
            txn: self.db.begin_read()?,
        })
    }

    /// Begin a single read-write transaction. While it is open no other writer
    /// can run, and every read through it sees one consistent snapshot — so a
    /// read → decide → write sequence cannot be interleaved. Call
    /// [`WriteTxn::commit`] to persist; dropping without committing aborts.
    pub(crate) fn write(&self) -> Result<WriteTxn, Error> {
        Ok(WriteTxn {
            txn: self.db.begin_write()?,
        })
    }
}

/// A read-only snapshot: a consistent view of committed state for the life of
/// the transaction.
pub(crate) struct ReadTxn {
    pub(crate) txn: ReadTransaction,
}

/// A single read-write transaction: a consistent snapshot plus the exclusive
/// right to write. It carries the same read surface as [`ReadTxn`], so a
/// sequence of reads feeding a decision and the writes acting on it are atomic
/// with respect to any other writer.
pub(crate) struct WriteTxn {
    pub(crate) txn: WriteTransaction,
}

impl WriteTxn {
    /// Persist every change made through this transaction.
    pub(crate) fn commit(self) -> Result<(), Error> {
        self.txn.commit()?;
        Ok(())
    }
}
