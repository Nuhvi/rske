//! The Rootstock header tree.
//!
//! Each proposed checkpoint block (found by the p2p layer) roots a **forest**:
//! headers are stored keyed by hash and each block links to its parent. Work
//! follows rskj's rule — a block's own contribution is `difficulty + Σ(included
//! uncle difficulty)` — and the tree is weighted by **GHOST**: a block's weight
//! is the sum over its whole subtree, so side branches count, not just the one
//! chain. A tree is trusted once its work exceeds `2/3` of the Bitcoin tail's
//! work (see [`crate::rsk::checkpoint`]).

pub mod checkpoint;

use alloy_primitives::{B256, U256};
use bitcoin::Network;
use rsk_consensus::Header;
use rsk_consensus::types::block::Block;
use rsk_consensus::validation::HeaderVerifier;
use rsk_consensus::validation::uncles::{
    AncestorSource, UNCLE_GENERATION_LIMIT, UNCLE_LIST_LIMIT, UnclesValidationRule,
};

use crate::Core;
use crate::error::Error;
use crate::store::WriteTxn;

/// The Rootstock consensus config matching a Bitcoin [`Network`]. Used when
/// [`Core`] is opened.
pub(crate) fn chain_config(network: Network) -> std::sync::Arc<rsk_consensus::config::ChainConfig> {
    use rsk_consensus::config::ChainConfig;
    std::sync::Arc::new(match network {
        Network::Bitcoin => ChainConfig::mainnet(),
        Network::Testnet | Network::Testnet4 | Network::Signet => ChainConfig::testnet(),
        Network::Regtest => ChainConfig::regtest(),
    })
}

/// A Rootstock header and the uncle headers it references, each with the wire
/// bytes its hash is taken over.
#[derive(Debug, Clone)]
pub struct RskEntry {
    pub header: Header,
    pub raw: Vec<u8>,
    pub uncles: Vec<RskUncle>,
}

/// One uncle header and its wire bytes.
#[derive(Debug, Clone)]
pub struct RskUncle {
    pub header: Header,
    pub raw: Vec<u8>,
}

/// Outcome of a [`Core::connect_rsk_headers`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RskConnectOutcome {
    /// Headers newly stored.
    pub connected: usize,
    /// The checkpoint this batch established, if any: a proposed checkpoint
    /// whose tree first cleared the trust threshold.
    pub checkpoint: Option<EphemeralChainCheckpoint>,
}

/// A trusted place to start syncing from: a proposed checkpoint block whose
/// tree carries more than `2/3` of the Bitcoin tail's work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EphemeralChainCheckpoint {
    /// The Rootstock block hash of the checkpoint.
    pub hash: B256,
    /// Its Rootstock block number.
    pub number: u64,
    /// The GHOST work of its tree.
    pub work: U256,
    /// The Bitcoin tail work it was measured against.
    pub btc_tail_work: U256,
    /// The Bitcoin tail start the measurement used.
    pub tail_start: u64,
}

// ── the tree on `Core` ──────────────────────────────────────────────────────

impl Core {
    /// Propose a checkpoint block and mark it a root of the forest.
    ///
    /// The p2p layer only *finds* the candidate; the core checks it here: the
    /// header's merge-mining anchor must be a canonical Bitcoin block inside the
    /// trust tail (see [`checkpoint::in_tail`]). A candidate that fails is
    /// refused, so a tree is only ever rooted at a block tied to Bitcoin.
    pub fn propose_checkpoint(&self, entry: &RskEntry) -> Result<B256, Error> {
        let Some(btc_tip) = self.tip_height()? else {
            return Err(Error::Unseeded);
        };
        let btc_tail_start = self.finalized_checkpoint()?.unwrap_or(0);
        let anchor = checkpoint::anchor_height(self, &entry.header)?.ok_or_else(|| {
            Error::Rootstock(format!(
                "checkpoint #{}: anchor is not a canonical bitcoin block",
                entry.header.number
            ))
        })?;
        if !checkpoint::in_tail(anchor, btc_tail_start, btc_tip) {
            return Err(Error::Rootstock(format!(
                "checkpoint #{}: anchor {anchor} is not in the bitcoin tail \
                 [{btc_tail_start}, {btc_tip} - {}]",
                entry.header.number,
                checkpoint::ANCHOR_DEPTH
            )));
        }
        let txn = self.store.write()?;
        let hash = txn.rsk_put_entry(entry)?;
        txn.rsk_put_root(hash.as_slice())?;
        txn.commit()?;
        Ok(B256::from(hash))
    }

    /// Validate and store a contiguous ascending run of RSK headers (with their
    /// uncles). Each header must link to a stored block; the batch is stored as
    /// it validates, so later entries see earlier ones.
    ///
    /// Validation is `rsk-consensus`': static + parent rules on each header, the
    /// `ommers_hash` commitment on the uncle list, and the full uncle rule
    /// (generation window, no reuse, and the uncle's parent must be one of the
    /// including block's ancestors) against a store-backed [`AncestorSource`].
    ///
    /// Batches are incremental — feed them as they arrive, ascending, each
    /// parent already stored. After storing, the call attempts to establish a
    /// checkpoint and reports it in [`RskConnectOutcome::checkpoint`].
    pub fn connect_rsk_headers(&self, entries: &[RskEntry]) -> Result<RskConnectOutcome, Error> {
        let tail = self.checkpoint_tail()?;
        let txn = self.store.write()?;
        let verifier = HeaderVerifier::default_rsk(self.rsk_chain.clone());
        let mut connected = 0usize;
        for entry in entries {
            validate_entry(&txn, &verifier, entry)?;
            txn.rsk_put_entry(entry)?;
            connected += 1;
        }
        let checkpoint = match tail {
            Some((tail_start, btc_tail_work)) => {
                attempt_checkpoint(&txn, tail_start, btc_tail_work)?
            }
            None => None,
        };
        txn.commit()?;
        Ok(RskConnectOutcome {
            connected,
            checkpoint,
        })
    }

    /// The Bitcoin tail a checkpoint is measured against: `(tail_start, its
    /// work)`, or `None` while no tail work is stored yet.
    fn checkpoint_tail(&self) -> Result<Option<(u64, U256)>, Error> {
        let Some(tail_start) = self.finalized_checkpoint()? else {
            return Ok(None);
        };
        let Some(work) = self.btc_tail_work(tail_start)? else {
            return Ok(None);
        };
        Ok(Some((tail_start, U256::from_be_slice(&work.to_be_bytes()))))
    }

    /// GHOST weight of the subtree rooted at `root`: `Σ cumulative_difficulty`
    /// over `root` and every descendant.
    #[cfg(test)]
    pub fn rsk_subtree_work(&self, root: &B256) -> Result<U256, Error> {
        self.store.read()?.rsk_subtree_work(root.as_slice())
    }

    /// The root with the heaviest subtree, if any.
    #[cfg(test)]
    pub fn rsk_best_checkpoint(&self) -> Result<Option<(B256, U256)>, Error> {
        let txn = self.store.read()?;
        let mut best: Option<(B256, U256)> = None;
        for root in txn.rsk_roots()? {
            let work = txn.rsk_subtree_work(&root)?;
            if best.as_ref().is_none_or(|(_, w)| work > *w) {
                best = Some((B256::from(root), work));
            }
        }
        Ok(best)
    }
}

/// The checkpoint attempt: among the proposed roots, the heaviest whose tree
/// clears `2/3` of the Bitcoin tail's work. `None` if none does.
fn attempt_checkpoint(
    txn: &WriteTxn,
    tail_start: u64,
    btc_tail_work: U256,
) -> Result<Option<EphemeralChainCheckpoint>, Error> {
    let mut best: Option<EphemeralChainCheckpoint> = None;
    for root in txn.rsk_roots()? {
        let work = txn.rsk_subtree_work(&root)?;
        if !checkpoint::clears_threshold(work, btc_tail_work) {
            continue;
        }
        let Some(block) = txn.rsk_block(&root)? else {
            continue;
        };
        let candidate = EphemeralChainCheckpoint {
            hash: B256::from(root),
            number: block.number,
            work,
            btc_tail_work,
            tail_start,
        };
        if best.is_none_or(|b| candidate.work > b.work) {
            best = Some(candidate);
        }
    }
    Ok(best)
}

/// Validate one entry: commitment, header rules against its parent, and the
/// full uncle rule.
fn validate_entry(
    txn: &WriteTxn,
    verifier: &HeaderVerifier,
    entry: &RskEntry,
) -> Result<(), Error> {
    let header = &entry.header;

    if entry.uncles.len() as u64 != header.uncle_count {
        return Err(Error::Rootstock(format!(
            "block #{} says {} uncle(s) but {} came with it",
            header.number,
            header.uncle_count,
            entry.uncles.len()
        )));
    }
    let committed = alloy_primitives::keccak256(encoded_uncles(&entry.uncles));
    if committed != header.ommers_hash {
        return Err(Error::Rootstock(format!(
            "block #{}: uncle list does not match the header's ommers hash",
            header.number
        )));
    }

    let parent = txn
        .rsk_raw_header(header.parent_hash.as_slice())?
        .and_then(|raw| decode_header(&raw));
    verifier
        .verify(header, parent.as_ref())
        .map_err(|e| Error::Rootstock(format!("header #{}: {e}", header.number)))?;

    if !entry.uncles.is_empty() {
        let source = TxnAncestors { txn };
        let rule = UnclesValidationRule {
            store: &source,
            uncle_list_limit: UNCLE_LIST_LIMIT,
            uncle_generation_limit: UNCLE_GENERATION_LIMIT,
            header_rules: verifier.static_rules(),
            parent_rules: verifier.parent_rules(),
        };
        let block = Block {
            header: header.clone(),
            transactions: Vec::new(),
            ommers: entry.uncles.iter().map(|u| u.header.clone()).collect(),
        };
        rule.validate(&block)
            .map_err(|e| Error::Rootstock(format!("uncles of #{}: {e}", header.number)))?;
    }

    Ok(())
}

/// The RLP of the uncle list as `ommers_hash` commits to it: the wire encodings
/// of the uncles wrapped in a list.
fn encoded_uncles(uncles: &[RskUncle]) -> Vec<u8> {
    let mut payload = Vec::new();
    for uncle in uncles {
        payload.extend_from_slice(&uncle.raw);
    }
    let mut out = Vec::with_capacity(payload.len() + 9);
    alloy_rlp::Header {
        list: true,
        payload_length: payload.len(),
    }
    .encode(&mut out);
    out.extend_from_slice(&payload);
    out
}

/// Decode a header from its wire bytes, hashing from those bytes.
fn decode_header(raw: &[u8]) -> Option<Header> {
    let mut slice = raw;
    Header::decode_with_hash(&mut slice).ok()
}

/// The store-backed ancestry the uncle rule walks.
struct TxnAncestors<'a> {
    txn: &'a WriteTxn,
}

impl AncestorSource for TxnAncestors<'_> {
    fn header(&self, hash: B256) -> Option<Header> {
        self.txn
            .rsk_raw_header(hash.as_slice())
            .ok()
            .flatten()
            .and_then(|raw| decode_header(&raw))
    }

    fn uncles_of(&self, hash: B256) -> Vec<Header> {
        self.txn
            .rsk_uncle_raw(hash.as_slice())
            .ok()
            .unwrap_or_default()
            .iter()
            .filter_map(|raw| decode_header(raw))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_rlp::Encodable;
    use bitcoin::Network;
    use bitcoin::block::BlockHash;
    use bitcoin::block::Header as BitcoinHeader;
    use bitcoin::hashes::Hash;
    use rsk_consensus::config::ChainConfig;
    use rsk_consensus::types::header::Header as RskHeader;
    use std::sync::Arc;

    /// A synthetic RSK header. No PoW — these tests exercise the tree/work
    /// layer, not consensus validation. `tag` makes otherwise-identical
    /// siblings distinct (a different timestamp).
    fn header(number: u64, parent: B256, difficulty: u64, tag: u64) -> RskHeader {
        RskHeader {
            parent_hash: parent,
            ommers_hash: B256::ZERO,
            beneficiary: Default::default(),
            state_root: B256::ZERO,
            transactions_root: B256::ZERO,
            receipts_root: B256::ZERO,
            logs_bloom: Default::default(),
            extension_data: None,
            difficulty: U256::from(difficulty),
            number,
            gas_limit: U256::from(10_000_000u64),
            gas_used: 0,
            timestamp: 1_600_000_000 + number + tag,
            extra_data: Default::default(),
            paid_fees: U256::ZERO,
            minimum_gas_price: U256::from(1),
            uncle_count: 0,
            umm_root: None,
            bitcoin_merged_mining_header: None,
            bitcoin_merged_mining_merkle_proof: None,
            bitcoin_merged_mining_coinbase_transaction: None,
            cached_hash: None,
            cached_hash_for_merged_mining: None,
        }
    }

    /// A raw-encoded entry (canonical RLP) for the unvalidated tree tests.
    fn entry(number: u64, parent: B256, difficulty: u64, tag: u64) -> RskEntry {
        let header = header(number, parent, difficulty, tag);
        let mut raw = Vec::new();
        header.encode(&mut raw);
        RskEntry {
            header,
            raw,
            uncles: Vec::new(),
        }
    }

    fn core() -> Core {
        Core::in_memory(Network::Regtest).unwrap()
    }

    /// Store an unvalidated entry straight into the tree (bypassing consensus).
    fn put(core: &Core, entry: &RskEntry) -> B256 {
        let txn = core.store.write().unwrap();
        let hash = txn.rsk_put_entry(entry).unwrap();
        txn.commit().unwrap();
        B256::from(hash)
    }

    /// The easiest valid Bitcoin target (regtest's proof-of-work limit).
    const EASY_BITS: u32 = 0x207f_ffff;

    /// Mine a Bitcoin header on top of `prev` at `time`.
    fn mine(prev: BlockHash, time: u32) -> BitcoinHeader {
        use bitcoin::block::Version;
        use bitcoin::pow::CompactTarget;
        let target = CompactTarget::from_consensus(EASY_BITS);
        let mut nonce = 0u32;
        loop {
            let header = BitcoinHeader {
                version: Version::ONE,
                prev_blockhash: prev,
                merkle_root: Hash::all_zeros(),
                time,
                bits: target,
                nonce,
            };
            if header.validate_pow(header.target()).is_ok() {
                return header;
            }
            nonce += 1;
        }
    }

    /// A regtest chain with a usable Bitcoin tail: the spine at 0 and 2016, then
    /// easy headers up to 4032, so `btc_tail_work(2016) = work[2016..4032)`.
    fn btc_with_tail() -> Core {
        let core = Core::in_memory(Network::Regtest).unwrap();
        let g0 = mine(BlockHash::all_zeros(), 1_600_000_000);
        let g2016 = mine(g0.block_hash(), 1_600_000_001);
        core.seed_checkpoints(&[(0, g0), (2016, g2016)]).unwrap();

        let mut cur = g2016;
        let mut run = Vec::new();
        for i in 0..2016u32 {
            cur = mine(cur.block_hash(), 1_600_000_002 + i);
            run.push(cur);
        }
        // A clock 1,200,000 s (~2000 blocks) past the anchor: the bar asks for
        // 2000 blocks of work, and the tail above the anchor carries 2016.
        core.connect_headers(&run, 1_600_000_001 + 1_200_000)
            .unwrap();
        core
    }

    /// An entry whose merge-mining header points at the Bitcoin block `anchor`.
    fn anchored(number: u64, anchor: BlockHash, difficulty: u64) -> RskEntry {
        use bitcoin::block::Version;
        use bitcoin::consensus::Encodable as _;
        use bitcoin::pow::CompactTarget;
        let solution = BitcoinHeader {
            version: Version::ONE,
            prev_blockhash: anchor,
            merkle_root: Hash::all_zeros(),
            time: 1_600_000_000,
            bits: CompactTarget::from_consensus(EASY_BITS),
            nonce: 0,
        };
        let mut mm = Vec::new();
        solution.consensus_encode(&mut mm).unwrap();
        let mut header = header(number, B256::ZERO, difficulty, 0);
        header.bitcoin_merged_mining_header = Some(mm.into());
        let mut raw = Vec::new();
        header.encode(&mut raw);
        RskEntry {
            header,
            raw,
            uncles: Vec::new(),
        }
    }

    #[test]
    fn ghost_weight_counts_side_branches() {
        let core = core();
        let f = put(&core, &entry(0, B256::ZERO, 100, 0));

        // A chain of five blocks above F.
        let mut parent = f;
        let mut a_tip = f;
        for n in 1..=5u64 {
            a_tip = put(&core, &entry(n, parent, 10, 0));
            parent = a_tip;
        }
        // A fork off block #2, four blocks long (tagged so its blocks differ
        // from the main chain's).
        let fork_base = {
            // block #2's hash
            let txn = core.store.read().unwrap();
            // walk: F's child #1, #2
            let c1 = txn.rsk_children(f.as_slice()).unwrap()[0];
            let c2 = txn.rsk_children(&c1).unwrap()[0];
            B256::from(c2)
        };
        let mut parent = fork_base;
        for n in 3..=6u64 {
            parent = put(&core, &entry(n, parent, 10, 1));
        }

        // F(100) + chain A(5*10=50) + fork B(4*10=40) = 190.
        assert_eq!(core.rsk_subtree_work(&f).unwrap(), U256::from(190));

        // The linear chain alone (F + A) would be 150; GHOST sees 190.
        let linear = U256::from(100 + 5 * 10);
        assert!(core.rsk_subtree_work(&f).unwrap() > linear);

        let _ = (a_tip, fork_base);
    }

    #[test]
    fn checkpoints_are_ranked_by_subtree_work() {
        let core = core();
        let small = put(&core, &entry(0, B256::ZERO, 10, 0));
        let big = put(&core, &entry(100, B256::ZERO, 1000, 0));
        {
            let txn = core.store.write().unwrap();
            txn.rsk_put_root(small.as_slice()).unwrap();
            txn.rsk_put_root(big.as_slice()).unwrap();
            txn.commit().unwrap();
        }

        let (best, work) = core.rsk_best_checkpoint().unwrap().unwrap();
        assert_eq!(best, big);
        assert_eq!(work, U256::from(1000));
    }

    #[test]
    fn connect_rejects_a_mismatched_uncle_list() {
        let core = core();
        let f = put(&core, &entry(0, B256::ZERO, 100, 0));

        // A header claiming one uncle but carrying none.
        let mut bad = entry(1, f, 10, 0);
        bad.header.uncle_count = 1;
        let err = core.connect_rsk_headers(&[bad]).unwrap_err();
        assert!(matches!(err, Error::Rootstock(_)), "got {err:?}");
    }

    #[test]
    fn the_attempt_trusts_the_heaviest_qualifying_tree() {
        let core = core();
        let light = put(&core, &entry(0, B256::ZERO, 10, 0));
        let heavy = put(&core, &entry(0, B256::ZERO, 100, 1));
        put(&core, &entry(1, heavy, 50, 0));
        {
            let txn = core.store.write().unwrap();
            txn.rsk_put_root(light.as_slice()).unwrap();
            txn.rsk_put_root(heavy.as_slice()).unwrap();
            txn.commit().unwrap();
        }

        // light = 10, heavy = 150; 2/3 of 100 is ~67, so only `heavy` qualifies.
        let txn = core.store.write().unwrap();
        let cp = attempt_checkpoint(&txn, 2016, U256::from(100))
            .unwrap()
            .expect("a checkpoint");
        assert_eq!(cp.hash, heavy);
        assert_eq!(cp.number, 0);
        assert_eq!(cp.work, U256::from(150));
        assert_eq!(cp.tail_start, 2016);

        // 2/3 of 300 is 200: nothing qualifies.
        assert!(
            attempt_checkpoint(&txn, 2016, U256::from(300))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn connect_rsk_headers_reports_an_established_checkpoint() {
        let core = btc_with_tail();
        assert_eq!(core.finalized_checkpoint().unwrap(), Some(2016));

        // A single, very heavy root clears 2/3 of the (easy-target) tail work.
        let f = put(&core, &entry(0, B256::ZERO, u64::MAX, 0));
        {
            let txn = core.store.write().unwrap();
            txn.rsk_put_root(f.as_slice()).unwrap();
            txn.commit().unwrap();
        }

        let out = core.connect_rsk_headers(&[]).unwrap();
        assert_eq!(out.connected, 0);
        let cp = out.checkpoint.expect("a checkpoint");
        assert_eq!(cp.hash, f);
        assert_eq!(cp.tail_start, 2016);
        assert_eq!(cp.work, U256::from(u64::MAX));
    }

    #[test]
    fn propose_checkpoint_requires_an_in_tail_anchor() {
        let core = btc_with_tail();
        assert_eq!(core.tip_height().unwrap(), Some(4032));

        // Height 3000 is canonical and buried well over ANCHOR_DEPTH below the tip.
        let good = core.header_at(3000).unwrap().unwrap();
        assert!(
            core.propose_checkpoint(&anchored(0, good.block_hash(), 10))
                .is_ok()
        );

        // Height 4030 is too near the tip to trust.
        let shallow = core.header_at(4030).unwrap().unwrap();
        let err = core
            .propose_checkpoint(&anchored(1, shallow.block_hash(), 10))
            .unwrap_err();
        assert!(matches!(err, Error::Rootstock(_)), "got {err:?}");
    }

    #[test]
    fn rsk_config_matches_the_network() {
        // Smoke: the Core carries an RSK chain config.
        let core = core();
        assert_eq!(core.rsk_chain.chain_id, ChainConfig::regtest().chain_id);
        let _ = Arc::new(core.rsk_chain.clone());
    }
}
