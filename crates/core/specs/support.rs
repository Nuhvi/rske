//! Shared fixtures for the executable specifications.
//!
//! The machines and the rule scenarios both build synthetic Bitcoin chains.
//! They use the same relaxed proof-of-work limit and the same in-memory
//! [`Core`], so the fixtures live here instead of in either test file.
//!
//! [`header_universe`] builds the delivery fixture that the chain machine and
//! the median-time-past scenario share. Each capability file still owns its
//! own model and scenarios.
//!
//! The synthetic chain starts at [`CHECKPOINT_HEIGHT`] with [`EASY_BITS`], so
//! headers
//! mine in a few hashes while the mainnet difficulty rule stays active.

use alloy_primitives::U256;
use bitcoin::Network;
use bitcoin::TxMerkleNode;
use bitcoin::block::{BlockHash, Header as BitcoinHeader, Version};
use bitcoin::consensus::Params;
use bitcoin::hashes::Hash as _;
use bitcoin::pow::{CompactTarget, Target};

use crate::{Action, Config, Core, Error, Outcome};

/// The easiest valid Bitcoin target on the test network, so synthetic headers
/// mine immediately.
pub(super) const EASY_BITS: u32 = 0x207f_ffff;
/// A recent mainnet difficulty-period start (2016 × 481).
///
/// Each machine starts here, so synthetic headers have recent block numbers
/// without mining recent mainnet difficulty.
pub(super) const CHECKPOINT_HEIGHT: u64 = 969_696;
/// The initial checkpoint's timestamp: 2026-10-14, near the checkpoint
/// height in the real
/// chain, so the synthetic chain has recent times.
pub(super) const CHECKPOINT_TIME: u32 = 1_792_000_000;
/// The clock the future-time machine connects at. It is a week after the
/// initial checkpoint,
/// so every ordinary header is in the past.
pub(super) const NOW: u64 = CHECKPOINT_TIME as u64 + 7 * 86_400;

/// Mainnet consensus parameters with the proof-of-work limit relaxed to
/// [`EASY_BITS`], so the specs keep the mainnet difficulty rule but mine with
/// almost no work.
pub(super) fn mainnet_params() -> Params {
    let mut params = Params::new(Network::Bitcoin);
    params.max_attainable_target = Target::from_compact(CompactTarget::from_consensus(EASY_BITS));
    params
}

/// Testnet3 consensus parameters with the same relaxed limit.
///
/// The minimum-difficulty rule stays on, so HDR-05 can use a normal target that
/// differs from the proof-of-work limit.
pub(super) fn testnet_params() -> Params {
    let mut params = Params::new(Network::Testnet);
    params.max_attainable_target = Target::from_compact(CompactTarget::from_consensus(EASY_BITS));
    params
}

/// An in-memory core for the specs.
///
/// It keeps the consensus rules of `params` and disables only the estimated-work
/// finality requirement, which a synthetic chain cannot clear. `checkpoints`
/// is the trusted checkpoint set the machine starts from; production code
/// cannot replace the embedded set. `check_difficulty` is false for the delivery machine: with the
/// expected-bits rules on, an out-of-range target fails before HDR-02 can see
/// it, so the `BadTarget` transition would be unreachable. HDR-04 has its own
/// machine.
pub(super) fn synthetic_mainnet_core(
    params: Params,
    checkpoints: &[(u64, BitcoinHeader)],
    check_difficulty: bool,
) -> Core {
    let mut config = Config::in_memory(Network::Bitcoin)
        .with_params(params)
        .without_partition_safety()
        .with_checkpoints(checkpoints);
    if !check_difficulty {
        config = config.without_difficulty_check();
    }
    Core::new(config).expect("in-memory store")
}

pub(super) fn mainnet_core(checkpoints: &[(u64, BitcoinHeader)]) -> Core {
    synthetic_mainnet_core(mainnet_params(), checkpoints, true)
}

pub(super) fn delivery_core(checkpoints: &[(u64, BitcoinHeader)]) -> Core {
    synthetic_mainnet_core(mainnet_params(), checkpoints, false)
}

/// A header with a chosen compact target and no mining.
///
/// Use it when the rule under test runs before proof of work, so the hash does
/// not matter.
pub(super) fn header_with(prev: BlockHash, time: u32, bits: u32) -> BitcoinHeader {
    BitcoinHeader {
        version: Version::ONE,
        prev_blockhash: prev,
        merkle_root: TxMerkleNode::all_zeros(),
        time,
        bits: CompactTarget::from_consensus(bits),
        nonce: 0,
    }
}

pub(super) fn mine(prev: BlockHash, time: u32, tag: u32) -> BitcoinHeader {
    let target = CompactTarget::from_consensus(EASY_BITS);
    let mut nonce = tag;
    loop {
        let header = BitcoinHeader {
            version: Version::ONE,
            prev_blockhash: prev,
            merkle_root: TxMerkleNode::all_zeros(),
            time,
            bits: target,
            nonce,
        };
        if header.validate_pow(header.target()).is_ok() {
            return header;
        }
        nonce = nonce.wrapping_add(1);
        assert!(nonce != 0, "could not mine a header");
    }
}

pub(super) fn mine_bad(prev: BlockHash, time: u32, bits: u32) -> BitcoinHeader {
    let target = CompactTarget::from_consensus(bits);
    let mut nonce = 0u32;
    loop {
        let header = BitcoinHeader {
            version: Version::ONE,
            prev_blockhash: prev,
            merkle_root: TxMerkleNode::all_zeros(),
            time,
            bits: target,
            nonce,
        };
        if header.validate_pow(header.target()).is_err() {
            return header;
        }
        nonce = nonce.wrapping_add(1);
    }
}

pub(super) fn mine_with_bits(prev: BlockHash, time: u32, tag: u32, bits: u32) -> BitcoinHeader {
    let target = CompactTarget::from_consensus(bits);
    let mut nonce = tag;
    loop {
        let header = BitcoinHeader {
            version: Version::ONE,
            prev_blockhash: prev,
            merkle_root: TxMerkleNode::all_zeros(),
            time,
            bits: target,
            nonce,
        };
        if header.validate_pow(header.target()).is_ok() {
            return header;
        }
        nonce = nonce.wrapping_add(1);
    }
}

/// One header in the delivery universe and the rule that rejects it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HeaderKind {
    Valid,
    BadPow,
    BadTime,
    BadTarget,
}

/// The chain-delivery fixture: a trunk, a competing fork, a fork of a fork
/// and a few invalid headers, with the batches a peer can deliver.
#[derive(Clone, Debug)]
pub(super) struct HeaderUniverse {
    pub(super) genesis: BitcoinHeader,
    pub(super) headers: Vec<BitcoinHeader>,
    pub(super) kinds: Vec<HeaderKind>,
    /// Contiguous batches the peer can deliver: every header alone, every
    /// adjacent pair and triple in a run, plus link-break, bad-PoW, duplicate
    /// and empty batches.
    pub(super) batches: Vec<Vec<usize>>,
}

/// Build the chain-delivery fixture.
pub(super) fn header_universe() -> HeaderUniverse {
    let genesis = mine(BlockHash::all_zeros(), CHECKPOINT_TIME, 0);
    let g = genesis.block_hash();
    let mut headers = Vec::new();
    let mut kinds = Vec::new();

    // A1..A12, the initial chain. A1 is index 0.
    let a1 = mine(g, CHECKPOINT_TIME + 1, 0);
    headers.push(a1);
    kinds.push(HeaderKind::Valid);
    let mut prev = a1;
    for i in 2..=12u32 {
        let next = mine(prev.block_hash(), CHECKPOINT_TIME + i, 0);
        headers.push(next);
        kinds.push(HeaderKind::Valid);
        prev = next;
    }

    // B3..B8 from A2 (index 1): a competing fork.
    let a2 = headers[1];
    let mut prev = mine(a2.block_hash(), a2.time + 100, 40);
    headers.push(prev);
    kinds.push(HeaderKind::Valid);
    for _ in 4..=8u32 {
        let next = mine(prev.block_hash(), prev.time + 1, 0);
        headers.push(next);
        kinds.push(HeaderKind::Valid);
        prev = next;
    }

    // From B7 (index 16): a fork of a fork.
    let b7 = headers[16];
    let c8 = mine(b7.block_hash(), b7.time + 500, 70);
    headers.push(c8);
    kinds.push(HeaderKind::Valid);

    // P4: linked to A1 but fails proof of work (index 19).
    let p4 = mine_bad(a1.block_hash(), a1.time + 900, EASY_BITS);
    headers.push(p4);
    kinds.push(HeaderKind::BadPow);

    // Q12: linked to A11 (index 10) but its timestamp equals the median of the
    // previous eleven headers (index 20). The full window makes MTP apply, and
    // equality is the HDR-07 boundary.
    let q12 = mine(headers[10].block_hash(), CHECKPOINT_TIME + 6, 77);
    headers.push(q12);
    kinds.push(HeaderKind::BadTime);

    // X2: a valid hash at a target above the relaxed mainnet proof-of-work
    // limit (index 21).
    let x2 = mine_with_bits(a1.block_hash(), CHECKPOINT_TIME + 2, 88, 0x2100_ffff);
    headers.push(x2);
    kinds.push(HeaderKind::BadTarget);

    let mut batches = Vec::new();
    for i in 0..headers.len() {
        batches.push(vec![i]);
    }
    let runs: [&[usize]; 3] = [
        &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
        &[12, 13, 14, 15, 16, 17],
        &[18],
    ];
    for run in runs {
        for len in 2..=3 {
            for window in run.windows(len) {
                batches.push(window.to_vec());
            }
        }
    }
    batches.push(vec![0, 2]); // A1 then A3: link break
    batches.push(vec![0, 19]); // A1 then P4: bad PoW
    batches.push(vec![0, 21]); // A1 then X2: target above the pow limit
    batches.push(vec![0, 0]); // duplicate inside one batch
    batches.push(Vec::new()); // empty batch

    HeaderUniverse {
        genesis,
        headers,
        kinds,
        batches,
    }
}

pub(super) fn work(core: &Core) -> U256 {
    U256::from_be_slice(
        &core
            .chainwork()
            .expect("read")
            .expect("initialized")
            .to_be_bytes(),
    )
}

/// The canonical headers from the first trusted checkpoint through the tip.
///
/// The first trusted checkpoint may start above height 0, so the canonical read
/// starts there.
pub(super) fn canonical_chain(core: &Core) -> Vec<BitcoinHeader> {
    core.canonical_headers()
        .expect("read")
        .collect::<Result<_, _>>()
        .expect("read")
}

/// Submit `headers` and return the outcome, or panic.
pub(super) fn submit(core: &Core, headers: &[BitcoinHeader], now: Option<u64>) -> Outcome {
    core.apply(Action::SubmitBitcoinHeaders { headers, now })
        .unwrap_or_else(|err| panic!("unexpected refusal: {err:?}"))
}

/// Submit `headers` and return the error, or panic.
pub(super) fn expect_err(core: &Core, headers: &[BitcoinHeader], now: Option<u64>) -> Error {
    match core.apply(Action::SubmitBitcoinHeaders { headers, now }) {
        Ok(_) => panic!("expected a refusal"),
        Err(err) => err,
    }
}

/// Submit `headers` and return the accepted count, or panic.
pub(super) fn expect_accepted(core: &Core, headers: &[BitcoinHeader], now: Option<u64>) -> usize {
    match submit(core, headers, now) {
        Outcome::BitcoinSubmitted(result) => result.accepted,
        other => panic!("expected a submitted outcome, got {other:?}"),
    }
}
