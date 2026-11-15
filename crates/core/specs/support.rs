//! Shared fixtures for the executable specifications.
//!
//! The machines and the rule scenarios both build synthetic Bitcoin chains.
//! They use the same relaxed proof-of-work limit and the same in-memory
//! [`Core`], so the fixtures live here instead of in either test file.
//!
//! The synthetic chain seeds at [`SEED_HEIGHT`] with [`EASY_BITS`], so headers
//! mine in a few hashes while the mainnet difficulty rule stays active.

use alloy_primitives::U256;
use bitcoin::Network;
use bitcoin::TxMerkleNode;
use bitcoin::block::{BlockHash, Header as BitcoinHeader, Version};
use bitcoin::consensus::Params;
use bitcoin::hashes::Hash as _;
use bitcoin::pow::{CompactTarget, Target};

use crate::{Config, Core};

/// The easiest valid Bitcoin target on the test network, so synthetic headers
/// mine immediately.
pub(super) const EASY_BITS: u32 = 0x207f_ffff;
/// A recent mainnet difficulty-period start (2016 × 481).
///
/// Each machine seeds here, so synthetic headers have recent block numbers
/// without mining recent mainnet difficulty.
pub(super) const SEED_HEIGHT: u64 = 969_696;
/// The seed header's timestamp: 2026-10-14, near the seed height in the real
/// chain, so the synthetic chain has recent times.
pub(super) const SEED_TIME: u32 = 1_792_000_000;
/// The clock the future-time machine connects at. It is a week after the seed,
/// so every ordinary header is in the past.
pub(super) const NOW: u64 = SEED_TIME as u64 + 7 * 86_400;

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
/// finality requirement, which a synthetic chain cannot clear. `anchor` is the
/// seed the machine starts from; production code cannot replace the embedded
/// spine. `check_difficulty` is false for the delivery machine: with the
/// expected-bits rules on, an out-of-range target fails before HDR-02 can see
/// it, so the `BadTarget` transition would be unreachable. HDR-04 has its own
/// machine.
pub(super) fn synthetic_mainnet_core(
    params: Params,
    anchor: &[(u64, BitcoinHeader)],
    check_difficulty: bool,
) -> Core {
    let mut config = Config::in_memory(Network::Bitcoin)
        .with_params(params)
        .without_partition_safety()
        .with_anchor(anchor);
    if !check_difficulty {
        config = config.without_difficulty_check();
    }
    Core::new(config).expect("in-memory store")
}

pub(super) fn mainnet_core(anchor: &[(u64, BitcoinHeader)]) -> Core {
    synthetic_mainnet_core(mainnet_params(), anchor, true)
}

pub(super) fn delivery_core(anchor: &[(u64, BitcoinHeader)]) -> Core {
    synthetic_mainnet_core(mainnet_params(), anchor, false)
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

pub(super) fn work(core: &Core) -> U256 {
    U256::from_be_slice(
        &core
            .chainwork()
            .expect("read")
            .expect("seeded")
            .to_be_bytes(),
    )
}

/// The canonical headers from the root seed through the tip.
///
/// The seed starts above height 0, so the canonical read starts at the seed.
pub(super) fn canonical_chain(core: &Core) -> Vec<BitcoinHeader> {
    core.canonical_headers()
        .expect("read")
        .collect::<Result<_, _>>()
        .expect("read")
}
