//! Executable specification for the `bitcoin-header-validation` capability.
//!
//! Each test carries the requirement ID it proves:
//!
//! - `hdr_00_supported_networks` — HDR-00
//! - `hdr_01_parent_of_the_first_new_header_is_known` — HDR-01
//! - `hdr_02_target_range` — HDR-02
//! - `hdr_03_proof_of_work` — HDR-03
//! - `hdr_04_expected_bits_inside_a_period` — HDR-04
//! - `hdr_05_minimum_difficulty_networks` — HDR-05
//! - `hdr_06_mainnet_and_regtest_boundaries` — HDR-06
//! - `hdr_07_median_time_past` — HDR-07
//! - `hdr_08_future_time` — HDR-08

use alloy_primitives::U256;
use bitcoin::Network;
use bitcoin::block::BlockHash;
use bitcoin::consensus::Params;
use bitcoin::hashes::Hash as _;
use bitcoin::pow::{CompactTarget, Target};

use super::support::{
    CHECKPOINT_HEIGHT, CHECKPOINT_TIME, EASY_BITS, NOW, delivery_core, expect_accepted, expect_err,
    header_universe, header_with, mainnet_core, mine, mine_bad, mine_with_bits,
    synthetic_mainnet_core, testnet_params,
};
use crate::btc::validation::MAX_FUTURE_BLOCK_TIME;
use crate::{Config, Core, Error};

/// HDR-00: only mainnet, testnet3 and regtest open.
#[test]
fn hdr_00_supported_networks() {
    for network in [Network::Bitcoin, Network::Testnet, Network::Regtest] {
        if let Err(err) = Core::in_memory(network) {
            panic!("{network} must open: {err:?}");
        }
    }

    for network in [Network::Testnet4, Network::Signet] {
        let path = std::env::temp_dir().join(format!(
            "rsk-core-unsupported-{}-{}.redb",
            std::process::id(),
            network
        ));
        let _ = std::fs::remove_file(&path);
        let err = match Core::new(Config::open(path.clone(), network)) {
            Ok(_) => panic!("{network} must be refused"),
            Err(err) => err,
        };
        let refused = matches!(&err, Error::UnsupportedNetwork(refused) if *refused == network);
        assert!(refused, "got {err:?}");
        assert!(!path.exists(), "a refused network created a store");
    }
}

/// HDR-01: the first new header must name a stored parent.
#[test]
fn hdr_01_parent_of_the_first_new_header_is_known() {
    let genesis = mine(BlockHash::all_zeros(), CHECKPOINT_TIME, 0);
    let core = mainnet_core(&[(CHECKPOINT_HEIGHT, genesis)]);

    let unknown = BlockHash::from_byte_array([0x11; 32]);
    let orphan = mine(unknown, CHECKPOINT_TIME + 1, 0);
    let err = expect_err(&core, &[orphan], None);
    assert!(
        matches!(err, Error::UnknownAncestor(hash) if hash == unknown),
        "got {err:?}"
    );

    let child = mine(genesis.block_hash(), CHECKPOINT_TIME + 1, 0);
    assert_eq!(expect_accepted(&core, &[child], None), 1);
}

/// HDR-02: the compact target must be in range.
#[test]
fn hdr_02_target_range() {
    let genesis = mine(BlockHash::all_zeros(), CHECKPOINT_TIME, 0);
    let core = delivery_core(&[(CHECKPOINT_HEIGHT, genesis)]);
    let parent = genesis.block_hash();

    for (label, bits) in [
        ("negative", 0x1d80_0000),
        ("zero", 0x1d00_0000),
        ("overflow", 0x2300_0001),
        ("above the limit", 0x2100_ffff),
    ] {
        let header = header_with(parent, CHECKPOINT_TIME + 1, bits);
        let err = expect_err(&core, &[header], None);
        assert!(
            matches!(&err, Error::BadPow { detail, .. } if detail == "target out of range"),
            "{label}: got {err:?}"
        );
    }

    // The range check runs before proof of work: the hash misses its own
    // target too, but the reported detail is the range one.
    let y2 = mine_bad(parent, CHECKPOINT_TIME + 2, 0x2100_ffff);
    let err = expect_err(&core, &[y2], None);
    assert!(
        matches!(&err, Error::BadPow { detail, .. } if detail == "target out of range"),
        "got {err:?}"
    );
}

/// HDR-03: the hash must meet its target.
#[test]
fn hdr_03_proof_of_work() {
    let genesis = mine(BlockHash::all_zeros(), CHECKPOINT_TIME, 0);
    let core = mainnet_core(&[(CHECKPOINT_HEIGHT, genesis)]);
    let parent = genesis.block_hash();

    let bad = mine_bad(parent, CHECKPOINT_TIME + 1, EASY_BITS);
    let err = expect_err(&core, &[bad], None);
    assert!(
        matches!(&err, Error::BadPow { detail, .. } if detail != "target out of range"),
        "got {err:?}"
    );

    let good = mine(parent, CHECKPOINT_TIME + 2, 0);
    assert_eq!(expect_accepted(&core, &[good], None), 1);
}

/// HDR-04: on mainnet, inside a period, the child carries the parent's bits.
#[test]
fn hdr_04_expected_bits_inside_a_period() {
    let genesis = mine(BlockHash::all_zeros(), CHECKPOINT_TIME, 0);
    let core = mainnet_core(&[(CHECKPOINT_HEIGHT, genesis)]);
    let parent = genesis.block_hash();

    let good = mine(parent, CHECKPOINT_TIME + 1, 0);
    assert_eq!(expect_accepted(&core, &[good], None), 1);

    let wrong = mine_with_bits(parent, CHECKPOINT_TIME + 2, 1, EASY_BITS - 1);
    let err = expect_err(&core, &[wrong], None);
    assert!(
        matches!(err, Error::BadDifficulty { expected, got, .. }
            if expected == EASY_BITS && got == EASY_BITS - 1),
        "got {err:?}"
    );
}

/// HDR-05: the minimum-difficulty rule on testnet3.
#[test]
fn hdr_05_minimum_difficulty_networks() {
    let normal = EASY_BITS - 1;
    let genesis = mine_with_bits(BlockHash::all_zeros(), CHECKPOINT_TIME, 0, normal);
    let core = synthetic_mainnet_core(testnet_params(), &[(0, genesis)], true);
    let parent = genesis.block_hash();

    let t1 = CHECKPOINT_TIME + 600;
    let block1 = mine_with_bits(parent, t1, 0, normal);
    assert_eq!(expect_accepted(&core, &[block1], None), 1);

    let t2 = t1 + 1201;
    let block2 = mine_with_bits(block1.block_hash(), t2, 0, EASY_BITS);
    assert_eq!(expect_accepted(&core, &[block2], None), 1);

    // A long gap must carry the limit, not the parent's normal bits.
    let stale = mine_with_bits(block1.block_hash(), t2, 1, normal);
    let err = expect_err(&core, &[stale], None);
    assert!(
        matches!(err, Error::BadDifficulty { got, .. } if got == normal),
        "got {err:?}"
    );

    // Exactly twice the spacing is not a long gap.
    let equal = mine_with_bits(block1.block_hash(), t1 + 1200, 2, EASY_BITS);
    let err = expect_err(&core, &[equal], None);
    assert!(
        matches!(err, Error::BadDifficulty { got, .. } if got == EASY_BITS),
        "got {err:?}"
    );

    // A normal block after a minimum-difficulty parent walks back to block1.
    let t3 = t2 + 600;
    let block3 = mine_with_bits(block2.block_hash(), t3, 0, normal);
    assert_eq!(expect_accepted(&core, &[block3], None), 1);

    // Skipping the walk-back and keeping the limit is refused.
    let limit = mine_with_bits(block2.block_hash(), t3, 1, EASY_BITS);
    let err = expect_err(&core, &[limit], None);
    assert!(
        matches!(err, Error::BadDifficulty { got, .. } if got == EASY_BITS),
        "got {err:?}"
    );
}

/// The retarget without the one-quarter and four-times clamp.
fn unclamped_bits(bits: CompactTarget, timespan: u64, target_timespan: u64) -> u32 {
    let target = Target::from_compact(bits);
    let raw = U256::from_be_slice(&target.to_be_bytes());
    let scaled = raw * U256::from(timespan) / U256::from(target_timespan);
    Target::from_be_bytes(scaled.to_be_bytes())
        .to_compact_lossy()
        .to_consensus()
}

/// HDR-06: mainnet and testnet3 retarget at a boundary; regtest keeps the bits.
#[test]
fn hdr_06_mainnet_and_regtest_boundaries() {
    let mut params = Params::new(Network::Regtest);
    params.allow_min_difficulty_blocks = false;
    params.no_pow_retargeting = false;

    let genesis = mine(BlockHash::all_zeros(), CHECKPOINT_TIME, 0);
    let core = synthetic_mainnet_core(params.clone(), &[(0, genesis)], true);

    let mut parent = genesis;
    let mut run = Vec::with_capacity(2015);
    for i in 1..=2015u32 {
        let header = mine_with_bits(parent.block_hash(), CHECKPOINT_TIME + i * 150, 0, EASY_BITS);
        run.push(header);
        parent = header;
    }
    assert_eq!(expect_accepted(&core, &run, None), 2015);

    let timespan = u64::from(parent.time) - u64::from(genesis.time);
    let expected = CompactTarget::from_next_work_required(parent.bits, timespan, &params);
    assert_ne!(
        expected.to_consensus(),
        EASY_BITS,
        "the period must retarget"
    );

    let boundary = mine_with_bits(
        parent.block_hash(),
        parent.time + 150,
        0,
        expected.to_consensus(),
    );
    assert_eq!(expect_accepted(&core, &[boundary], None), 1);

    let stale = mine_with_bits(parent.block_hash(), parent.time + 150, 1, EASY_BITS);
    let err = expect_err(&core, &[stale], None);
    assert!(
        matches!(err, Error::BadDifficulty { expected: e, got, .. }
            if e == expected.to_consensus() && got == EASY_BITS),
        "got {err:?}"
    );

    // The difficulty rule runs before proof of work, so the unclamped value
    // needs no valid hash.
    let unclamped = unclamped_bits(parent.bits, timespan, params.pow_target_timespan);
    assert_ne!(unclamped, expected.to_consensus());
    let bad = header_with(parent.block_hash(), parent.time + 150, unclamped);
    let err = expect_err(&core, &[bad], None);
    assert!(
        matches!(err, Error::BadDifficulty { got, .. } if got == unclamped),
        "got {err:?}"
    );

    // Regtest keeps the parent's bits at a boundary. The trusted checkpoints
    // must start at a period boundary, so reuse the stored 2015-block chain and change only the
    // params: the network stays Regtest, and the retarget turns off.
    let Core { store, config } = core;
    let core = Core {
        store,
        config: config.with_params(Params::new(Network::Regtest)),
    };
    let boundary = mine(parent.block_hash(), parent.time + 1, 0);
    assert_eq!(expect_accepted(&core, &[boundary], None), 1);

    let wrong = mine_with_bits(parent.block_hash(), parent.time + 2, 1, EASY_BITS - 1);
    let err = expect_err(&core, &[wrong], None);
    assert!(
        matches!(err, Error::BadDifficulty { expected: e, .. } if e == EASY_BITS),
        "got {err:?}"
    );
}

/// HDR-07: the median of the previous 11 headers, and the sparse-window skip.
#[test]
fn hdr_07_median_time_past() {
    let universe = header_universe();
    let core = mainnet_core(&[(CHECKPOINT_HEIGHT, universe.genesis)]);
    let trunk = &universe.headers[..11];
    assert_eq!(expect_accepted(&core, trunk, None), 11);

    // A1..A11 carry CHECKPOINT_TIME + 1..=11, so their median is CHECKPOINT_TIME + 6.
    let equal = mine(trunk[10].block_hash(), CHECKPOINT_TIME + 6, 1);
    let err = expect_err(&core, &[equal], None);
    assert!(
        matches!(err, Error::BadTimestamp { median, got, .. }
            if median == CHECKPOINT_TIME + 6 && got == CHECKPOINT_TIME + 6),
        "got {err:?}"
    );

    // One second above the previous-11 median. A window that wrongly includes
    // this header has median CHECKPOINT_TIME + 7 and would reject it.
    let above = mine(trunk[10].block_hash(), CHECKPOINT_TIME + 7, 2);
    assert_eq!(expect_accepted(&core, &[above], None), 1);

    // A fresh store has only the initial checkpoint, so the window is sparse and
    // the check
    // does not apply, even for an old timestamp.
    let checkpoint = mine(BlockHash::all_zeros(), CHECKPOINT_TIME, 0);
    let sparse = mainnet_core(&[(CHECKPOINT_HEIGHT, checkpoint)]);
    let old = mine(checkpoint.block_hash(), CHECKPOINT_TIME - 1_000, 3);
    assert_eq!(expect_accepted(&sparse, &[old], None), 1);
}

/// HDR-08: the two-hour future-time window is inclusive, and optional.
#[test]
fn hdr_08_future_time() {
    let genesis = mine(BlockHash::all_zeros(), CHECKPOINT_TIME, 0);
    let core = mainnet_core(&[(CHECKPOINT_HEIGHT, genesis)]);
    let parent = genesis.block_hash();

    let at_limit = mine(parent, (NOW + MAX_FUTURE_BLOCK_TIME) as u32, 0);
    assert_eq!(expect_accepted(&core, &[at_limit], Some(NOW)), 1);

    let over = mine(parent, (NOW + MAX_FUTURE_BLOCK_TIME + 1) as u32, 1);
    let err = expect_err(&core, &[over], Some(NOW));
    assert!(
        matches!(err, Error::TimestampTooNew { now, got, max_ahead, .. }
            if now == NOW
                && got == (NOW + MAX_FUTURE_BLOCK_TIME + 1) as u32
                && max_ahead == MAX_FUTURE_BLOCK_TIME),
        "got {err:?}"
    );

    let no_clock = mine(parent, (NOW + 1_000_000) as u32, 2);
    assert_eq!(expect_accepted(&core, &[no_clock], None), 1);
}
