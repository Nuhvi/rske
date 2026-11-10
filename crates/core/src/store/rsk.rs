//! Rootstock chain state: the header forest, its uncle lists, and the GHOST
//! subtree walk.
//!
//! The tables, their record types and the transaction methods that read and
//! write them all live here, so every Rootstock persistence detail is in one
//! place.

use std::collections::HashSet;

use alloy_primitives::U256;
use redb::{ReadableTable, TableDefinition, TypeName, Value, WriteTransaction};

use super::{Meta, ReadTxn, WriteTxn};
use crate::error::Error;
use crate::rsk::RskEntry;

/// RSK headers, keyed by block hash: raw RLP bytes.
const RSK_HEADERS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("rsk_headers");
/// `hash → RskBlock`.
const RSK_BLOCKS: TableDefinition<&[u8], RskBlock> = TableDefinition::new("rsk_blocks");
/// `hash → child hashes` (the GHOST subtree edges).
const RSK_CHILDREN: TableDefinition<&[u8], HashList> = TableDefinition::new("rsk_children");
/// `hash → raw uncle headers` (for the uncle rule's ancestry).
const RSK_UNCLES: TableDefinition<&[u8], BlobList> = TableDefinition::new("rsk_uncles");
/// Proposed checkpoint blocks: the roots of the forest, a set of hashes.
const RSK_ROOTS: TableDefinition<&[u8], ()> = TableDefinition::new("rsk_roots");
const RSK_META: TableDefinition<&str, Meta> = TableDefinition::new("rsk_meta");
/// The hash of the newest RSK header this store last committed from IBD.
const META_SYNC_TIP: &str = "sync_tip";

/// Open every Rootstock table (once, when the store is created).
pub(super) fn open_tables(txn: &WriteTransaction) -> Result<(), Error> {
    txn.open_table(RSK_HEADERS)?;
    txn.open_table(RSK_BLOCKS)?;
    txn.open_table(RSK_CHILDREN)?;
    txn.open_table(RSK_UNCLES)?;
    txn.open_table(RSK_ROOTS)?;
    txn.open_table(RSK_META)?;
    Ok(())
}

/// A stored RSK block index: its number, its parent, and its own work
/// (`difficulty + Σ included uncle difficulty`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RskBlock {
    pub number: u64,
    pub parent: [u8; 32],
    pub cumulative_difficulty: U256,
}

impl Value for RskBlock {
    type SelfType<'a> = RskBlock;
    type AsBytes<'a> = [u8; 72];

    fn fixed_width() -> Option<usize> {
        Some(72)
    }

    fn from_bytes<'a>(data: &'a [u8]) -> RskBlock
    where
        Self: 'a,
    {
        let mut parent = [0u8; 32];
        parent.copy_from_slice(&data[8..40]);
        RskBlock {
            number: u64::from_be_bytes(data[..8].try_into().expect("8 bytes")),
            parent,
            cumulative_difficulty: U256::from_be_slice(&data[40..72]),
        }
    }

    fn as_bytes<'a, 'b: 'a>(value: &'a RskBlock) -> [u8; 72]
    where
        Self: 'b,
    {
        let mut out = [0u8; 72];
        out[..8].copy_from_slice(&value.number.to_be_bytes());
        out[8..40].copy_from_slice(&value.parent);
        out[40..72].copy_from_slice(&value.cumulative_difficulty.to_be_bytes::<32>());
        out
    }

    fn type_name() -> TypeName {
        TypeName::new("rsk_core::RskBlock")
    }
}

/// A list of 32-byte hashes (the GHOST subtree edges).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct HashList(pub Vec<[u8; 32]>);

impl Value for HashList {
    type SelfType<'a> = HashList;
    type AsBytes<'a> = Vec<u8>;

    fn fixed_width() -> Option<usize> {
        None
    }

    fn from_bytes<'a>(data: &'a [u8]) -> HashList
    where
        Self: 'a,
    {
        HashList(
            data.chunks(32)
                .filter(|chunk| chunk.len() == 32)
                .map(|chunk| {
                    let mut hash = [0u8; 32];
                    hash.copy_from_slice(chunk);
                    hash
                })
                .collect(),
        )
    }

    fn as_bytes<'a, 'b: 'a>(value: &'a HashList) -> Vec<u8>
    where
        Self: 'b,
    {
        let mut out = Vec::with_capacity(value.0.len() * 32);
        for hash in &value.0 {
            out.extend_from_slice(hash);
        }
        out
    }

    fn type_name() -> TypeName {
        TypeName::new("rsk_core::HashList")
    }
}

/// A list of opaque byte blobs (raw uncle headers).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct BlobList(pub Vec<Vec<u8>>);

impl Value for BlobList {
    type SelfType<'a> = BlobList;
    type AsBytes<'a> = Vec<u8>;

    fn fixed_width() -> Option<usize> {
        None
    }

    fn from_bytes<'a>(data: &'a [u8]) -> BlobList
    where
        Self: 'a,
    {
        if data.len() < 4 {
            return BlobList::default();
        }
        let count = u32::from_be_bytes(data[..4].try_into().expect("4 bytes")) as usize;
        let mut out = Vec::with_capacity(count);
        let mut at = 4;
        for _ in 0..count {
            if at + 4 > data.len() {
                break;
            }
            let len = u32::from_be_bytes(data[at..at + 4].try_into().expect("4 bytes")) as usize;
            at += 4;
            if at + len > data.len() {
                break;
            }
            out.push(data[at..at + len].to_vec());
            at += len;
        }
        BlobList(out)
    }

    fn as_bytes<'a, 'b: 'a>(value: &'a BlobList) -> Vec<u8>
    where
        Self: 'b,
    {
        let mut out = Vec::new();
        out.extend_from_slice(&(value.0.len() as u32).to_be_bytes());
        for blob in &value.0 {
            out.extend_from_slice(&(blob.len() as u32).to_be_bytes());
            out.extend_from_slice(blob);
        }
        out
    }

    fn type_name() -> TypeName {
        TypeName::new("rsk_core::BlobList")
    }
}

/// Rootstock reads shared by [`ReadTxn`] and [`WriteTxn`], so a connect can
/// validate against what it has already staged.
macro_rules! rsk_read_methods {
    () => {
        /// Raw wire header bytes for `hash`.
        pub(crate) fn rsk_raw_header(&self, hash: &[u8]) -> Result<Option<Vec<u8>>, Error> {
            let table = self.txn.open_table(RSK_HEADERS)?;
            Ok(table.get(hash)?.map(|v| v.value().to_vec()))
        }

        /// The stored block index for `hash`.
        pub(crate) fn rsk_block(&self, hash: &[u8]) -> Result<Option<RskBlock>, Error> {
            let table = self.txn.open_table(RSK_BLOCKS)?;
            Ok(table.get(hash)?.map(|v| v.value()))
        }

        pub(crate) fn rsk_has_block(&self, hash: &[u8]) -> Result<bool, Error> {
            Ok(self.rsk_block(hash)?.is_some())
        }

        /// Child hashes of `hash` (the GHOST subtree edges).
        pub(crate) fn rsk_children(&self, hash: &[u8]) -> Result<Vec<[u8; 32]>, Error> {
            let table = self.txn.open_table(RSK_CHILDREN)?;
            Ok(table.get(hash)?.map(|v| v.value().0).unwrap_or_default())
        }

        /// Raw uncle headers of `hash`.
        pub(crate) fn rsk_uncle_raw(&self, hash: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
            let table = self.txn.open_table(RSK_UNCLES)?;
            Ok(table.get(hash)?.map(|v| v.value().0).unwrap_or_default())
        }

        /// The proposed checkpoint (forest root) hashes.
        pub(crate) fn rsk_roots(&self) -> Result<Vec<[u8; 32]>, Error> {
            let table = self.txn.open_table(RSK_ROOTS)?;
            let mut out = Vec::new();
            for entry in table.iter()? {
                let (key, _) = entry.map_err(Error::Storage)?;
                let mut hash = [0u8; 32];
                hash.copy_from_slice(key.value());
                out.push(hash);
            }
            Ok(out)
        }

        /// The newest RSK header an IBD pass committed, if recorded.
        pub(crate) fn rsk_sync_tip(&self) -> Result<Option<([u8; 32], u64)>, Error> {
            let table = self.txn.open_table(RSK_META)?;
            let Some(raw) = table.get(META_SYNC_TIP)?.map(|v| v.value()) else {
                return Ok(None);
            };
            let Meta::Text(text) = raw else {
                return Ok(None);
            };
            let Ok(bytes) = hex::decode(text) else {
                return Ok(None);
            };
            let Ok(hash) = <[u8; 32]>::try_from(bytes.as_slice()) else {
                return Ok(None);
            };
            let Some(block) = self.rsk_block(&hash)? else {
                return Ok(None);
            };
            Ok(Some((hash, block.number)))
        }

        /// GHOST weight of the subtree rooted at `root`: `Σ cumulative_difficulty`
        /// over `root` and every descendant.
        pub(crate) fn rsk_subtree_work(&self, root: &[u8]) -> Result<U256, Error> {
            let mut total = U256::ZERO;
            let mut stack: Vec<Vec<u8>> = vec![root.to_vec()];
            let mut seen: HashSet<Vec<u8>> = HashSet::new();
            while let Some(hash) = stack.pop() {
                if !seen.insert(hash.clone()) {
                    continue;
                }
                if let Some(block) = self.rsk_block(&hash)? {
                    total = total.saturating_add(block.cumulative_difficulty);
                }
                for child in self.rsk_children(&hash)? {
                    stack.push(child.to_vec());
                }
            }
            Ok(total)
        }
    };
}

#[allow(dead_code)] // some reads are only used on one transaction kind
impl ReadTxn {
    rsk_read_methods!();
}

#[allow(dead_code)] // some reads are only used on one transaction kind
impl WriteTxn {
    rsk_read_methods!();

    /// Store a header, its uncle list, and the child edge to its parent.
    pub(crate) fn rsk_put_entry(&self, entry: &RskEntry) -> Result<[u8; 32], Error> {
        let header = &entry.header;
        let hash = header.hash();
        let block = RskBlock {
            number: header.number,
            parent: header.parent_hash.0,
            cumulative_difficulty: cumulative_difficulty(entry),
        };
        self.rsk_put_block(hash.as_slice(), &entry.raw, block)?;
        let uncle_raws: Vec<Vec<u8>> = entry.uncles.iter().map(|u| u.raw.clone()).collect();
        self.rsk_put_uncles(hash.as_slice(), &uncle_raws)?;
        if self.rsk_has_block(header.parent_hash.as_slice())? {
            self.rsk_add_child(header.parent_hash.as_slice(), hash.as_slice())?;
        }
        Ok(hash.0)
    }

    pub(crate) fn rsk_put_block(
        &self,
        hash: &[u8],
        raw: &[u8],
        block: RskBlock,
    ) -> Result<(), Error> {
        {
            let mut table = self.txn.open_table(RSK_HEADERS)?;
            table.insert(hash, raw)?;
        }
        {
            let mut table = self.txn.open_table(RSK_BLOCKS)?;
            table.insert(hash, block)?;
        }
        Ok(())
    }

    pub(crate) fn rsk_put_uncles(&self, hash: &[u8], raws: &[Vec<u8>]) -> Result<(), Error> {
        let mut table = self.txn.open_table(RSK_UNCLES)?;
        table.insert(hash, BlobList(raws.to_vec()))?;
        Ok(())
    }

    pub(crate) fn rsk_add_child(&self, parent: &[u8], child: &[u8]) -> Result<(), Error> {
        let mut table = self.txn.open_table(RSK_CHILDREN)?;
        let mut children = table.get(parent)?.map(|v| v.value()).unwrap_or_default();
        let mut hash = [0u8; 32];
        hash.copy_from_slice(child);
        children.0.push(hash);
        table.insert(parent, children)?;
        Ok(())
    }

    pub(crate) fn rsk_put_root(&self, hash: &[u8]) -> Result<(), Error> {
        let mut table = self.txn.open_table(RSK_ROOTS)?;
        table.insert(hash, ())?;
        Ok(())
    }

    /// Remember the newest RSK header the IBD pass has committed.
    pub(crate) fn set_rsk_sync_tip(&self, hash: &[u8; 32]) -> Result<(), Error> {
        let mut table = self.txn.open_table(RSK_META)?;
        table.insert(META_SYNC_TIP, Meta::Text(hex::encode(hash)))?;
        Ok(())
    }
}

/// rskj's per-block contribution: `difficulty + Σ(included uncle difficulty)`.
fn cumulative_difficulty(entry: &RskEntry) -> U256 {
    let mut work = entry.header.difficulty;
    for uncle in &entry.uncles {
        work = work.saturating_add(uncle.header.difficulty);
    }
    work
}
