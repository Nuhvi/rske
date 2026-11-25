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
//!
//! Each spec scenario carries a `// Scenario:` comment at the assertion that
//! proves it.

use bitcoin::Network;
use bitcoin::block::BlockHash;
use bitcoin::consensus::Params;
use bitcoin::hashes::Hash as _;
use bitcoin::pow::{CompactTarget, Target};

use super::support::{
    CHECKPOINT_HEIGHT, CHECKPOINT_TIME, EASY_BITS, NOW, expect_accepted, expect_err,
    header_universe, header_with, mainnet_core, mine, mine_bad, mine_with_bits,
    synthetic_mainnet_core, testnet_params,
};
use crate::btc::validation::MAX_FUTURE_BLOCK_TIME;
use crate::{Config, Core, Error};

/// HDR-00: only mainnet, testnet3 and regtest open.
#[test]
fn hdr_00_supported_networks() {
    // Scenario: Supported network
    for network in [Network::Bitcoin, Network::Testnet, Network::Regtest] {
        if let Err(err) = Core::in_memory(network) {
            panic!("{network} must open: {err:?}");
        }
    }

    // Scenario: Unsupported network
    // The configured checkpoints must not be initialized either.
    let checkpoint = mine(BlockHash::all_zeros(), CHECKPOINT_TIME, 0);
    for network in [Network::Testnet4, Network::Signet] {
        let path = std::env::temp_dir().join(format!(
            "rsk-core-unsupported-{}-{}.redb",
            std::process::id(),
            network
        ));
        let _ = std::fs::remove_file(&path);
        let err = match Core::new(
            Config::open(path.clone(), network).with_checkpoints(&[(0, checkpoint)]),
        ) {
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

    // Scenario: Unknown parent
    let unknown = BlockHash::from_byte_array([0x11; 32]);
    let orphan = mine(unknown, CHECKPOINT_TIME + 1, 0);
    let err = expect_err(&core, &[orphan], None);
    assert!(
        matches!(err, Error::UnknownAncestor(hash) if hash == unknown),
        "got {err:?}"
    );

    // Scenario: Stored parent
    let child = mine(genesis.block_hash(), CHECKPOINT_TIME + 1, 0);
    assert_eq!(expect_accepted(&core, &[child], None), 1);
}

/// HDR-02: the compact target must be in range.
#[test]
fn hdr_02_target_range() {
    let genesis = mine(BlockHash::all_zeros(), CHECKPOINT_TIME, 0);
    let core = mainnet_core(&[(CHECKPOINT_HEIGHT, genesis)]);
    let parent = genesis.block_hash();

    // Scenario: Negative target
    // Scenario: Zero target
    // Scenario: Overflowing target
    // Scenario: Target above the network limit
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

    // Real mainnet proof-of-work limit (0x1d00_ffff): the next step up
    // (0x1d01_0000) is above the network limit.
    let real_mainnet = Core::new(
        Config::in_memory(Network::Bitcoin).with_checkpoints(&[(CHECKPOINT_HEIGHT, genesis)]),
    )
    .expect("in-memory store");
    let above_mainnet = header_with(parent, CHECKPOINT_TIME + 1, 0x1d01_0000);
    let err = expect_err(&real_mainnet, &[above_mainnet], None);
    assert!(
        matches!(&err, Error::BadPow { detail, .. } if detail == "target out of range"),
        "got {err:?}"
    );

    // Scenario: Range check before proof of work
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

    // Scenario: Hash above the target
    let bad = mine_bad(parent, CHECKPOINT_TIME + 1, EASY_BITS);
    let err = expect_err(&core, &[bad], None);
    assert!(
        matches!(&err, Error::BadPow { detail, .. } if detail != "target out of range"),
        "got {err:?}"
    );

    // Scenario: Hash at or below the target
    let good = mine(parent, CHECKPOINT_TIME + 2, 0);
    assert_eq!(expect_accepted(&core, &[good], None), 1);
}

/// HDR-04: on mainnet, inside a period, the child carries the parent's bits.
#[test]
fn hdr_04_expected_bits_inside_a_period() {
    let genesis = mine(BlockHash::all_zeros(), CHECKPOINT_TIME, 0);
    let core = mainnet_core(&[(CHECKPOINT_HEIGHT, genesis)]);
    let parent = genesis.block_hash();

    // Scenario: Parent bits
    let good = mine(parent, CHECKPOINT_TIME + 1, 0);
    assert_eq!(expect_accepted(&core, &[good], None), 1);

    // Scenario: Different bits
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
    let core = synthetic_mainnet_core(testnet_params(), &[(0, genesis)]);
    let parent = genesis.block_hash();

    let t1 = CHECKPOINT_TIME + 600;
    let block1 = mine_with_bits(parent, t1, 0, normal);
    assert_eq!(expect_accepted(&core, &[block1], None), 1);

    // Scenario: Long gap with the limit
    let t2 = t1 + 1201;
    let block2 = mine_with_bits(block1.block_hash(), t2, 0, EASY_BITS);
    assert_eq!(expect_accepted(&core, &[block2], None), 1);

    // Scenario: Long gap with the parent bits
    // A long gap must carry the limit, not the parent's normal bits.
    let stale = mine_with_bits(block1.block_hash(), t2, 1, normal);
    let err = expect_err(&core, &[stale], None);
    assert!(
        matches!(err, Error::BadDifficulty { expected, got, .. }
            if expected == EASY_BITS && got == normal),
        "got {err:?}"
    );

    // Scenario: Gap equal to twice the spacing
    // Exactly twice the spacing is not a long gap.
    let equal = mine_with_bits(block1.block_hash(), t1 + 1200, 2, EASY_BITS);
    let err = expect_err(&core, &[equal], None);
    assert!(
        matches!(err, Error::BadDifficulty { expected, got, .. }
            if expected == normal && got == EASY_BITS),
        "got {err:?}"
    );

    // Scenario: Walk back to the last normal header
    // A normal block after a minimum-difficulty parent walks back to block1.
    let t3 = t2 + 600;
    let block3 = mine_with_bits(block2.block_hash(), t3, 0, normal);
    assert_eq!(expect_accepted(&core, &[block3], None), 1);

    // Scenario: Walk back skipped
    // Skipping the walk-back and keeping the limit is refused.
    let limit = mine_with_bits(block2.block_hash(), t3, 1, EASY_BITS);
    let err = expect_err(&core, &[limit], None);
    assert!(
        matches!(err, Error::BadDifficulty { expected, got, .. }
            if expected == normal && got == EASY_BITS),
        "got {err:?}"
    );
}

/// HDR-06: mainnet and testnet3 retarget at a boundary; regtest keeps the bits.
#[test]
fn hdr_06_mainnet_and_regtest_boundaries() {
    // Use a base target with 4 leading zero bits (0x200f_ffff, ~16 hashes per
    // block) and a target timespan of 4 seconds (quarter clamp = 1 s,
    // four-times clamp = 16 s). That keeps `target * timespan` strictly below
    // 2^256 (no 256-bit wrap) and gives exact non-tautological retarget
    // constants:
    //   - 1/4x clamped retarget: 0x0f_ffff / 4 = 0x03_ffff -> 0x2003_ffff
    //   - 4x clamped retarget:   0x0f_ffff * 4 = 0x3f_fffc -> 0x203f_fffc
    //   - 5x unclamped (20 s):   0x0f_ffff * 5 = 0x4f_fffb -> 0x204f_fffb
    const BASE_BITS: u32 = 0x200f_ffff;
    const QUARTER_CLAMP_BITS: u32 = 0x2003_ffff;
    const UNCLAMPED_FAST_BITS: u32 = 0x2001_ffff;
    const FOUR_TIMES_CLAMP_BITS: u32 = 0x203f_fffc;
    const UNCLAMPED_SLOW_BITS: u32 = 0x204f_fffb;
    const POW_LIMIT_CLAMP_BITS: u32 = 0x201f_ffff;

    let mut params = Params::new(Network::Bitcoin);
    params.max_attainable_target = Target::from_compact(CompactTarget::from_consensus(EASY_BITS));
    params.pow_target_timespan = 4;

    let genesis = mine_with_bits(BlockHash::all_zeros(), CHECKPOINT_TIME + 2015, 0, BASE_BITS);
    let core = synthetic_mainnet_core(params.clone(), &[(0, genesis)]);

    let mut h2014 = genesis;
    let mut run = Vec::with_capacity(2015);
    for i in 1..=2014u32 {
        let header = mine_with_bits(h2014.block_hash(), CHECKPOINT_TIME + i, 0, BASE_BITS);
        run.push(header);
        h2014 = header;
    }
    // `parent_fast` at height 2015 ends the period at `genesis.time` (0 s
    // elapsed, less than one quarter of the 4 s target timespan).
    let parent_fast = mine_with_bits(h2014.block_hash(), CHECKPOINT_TIME + 2015, 0, BASE_BITS);
    run.push(parent_fast);
    assert_eq!(expect_accepted(&core, &run, None), 2015);

    // `parent_slow` is a sibling at height 2015 that ends the period 20 s
    // after `genesis.time` (5x the 4 s target timespan, exceeding the 4x clamp).
    let parent_slow = mine_with_bits(
        h2014.block_hash(),
        CHECKPOINT_TIME + 2015 + 20,
        1,
        BASE_BITS,
    );
    assert_eq!(expect_accepted(&core, &[parent_slow], None), 1);

    // Scenario: Mainnet quarter-timespan clamp
    let boundary_fast = mine_with_bits(
        parent_fast.block_hash(),
        parent_fast.time + 1,
        0,
        QUARTER_CLAMP_BITS,
    );
    assert_eq!(expect_accepted(&core, &[boundary_fast], None), 1);

    // Scenario: Mainnet boundary with the parent bits
    let stale = mine_with_bits(parent_fast.block_hash(), parent_fast.time + 1, 1, BASE_BITS);
    let err = expect_err(&core, &[stale], None);
    assert!(
        matches!(err, Error::BadDifficulty { expected, got, .. }
            if expected == QUARTER_CLAMP_BITS && got == BASE_BITS),
        "got {err:?}"
    );

    // Scenario: Unclamped retarget
    let bad_fast = header_with(
        parent_fast.block_hash(),
        parent_fast.time + 1,
        UNCLAMPED_FAST_BITS,
    );
    let err = expect_err(&core, &[bad_fast], None);
    assert!(
        matches!(err, Error::BadDifficulty { expected, got, .. }
            if expected == QUARTER_CLAMP_BITS && got == UNCLAMPED_FAST_BITS),
        "got {err:?}"
    );
    let bad_slow = header_with(
        parent_slow.block_hash(),
        parent_slow.time + 1,
        UNCLAMPED_SLOW_BITS,
    );
    let err = expect_err(&core, &[bad_slow], None);
    assert!(
        matches!(err, Error::BadDifficulty { expected, got, .. }
            if expected == FOUR_TIMES_CLAMP_BITS && got == UNCLAMPED_SLOW_BITS),
        "got {err:?}"
    );

    // Scenario: Mainnet four-times-timespan clamp
    let boundary_slow = mine_with_bits(
        parent_slow.block_hash(),
        parent_slow.time + 1,
        0,
        FOUR_TIMES_CLAMP_BITS,
    );
    assert_eq!(expect_accepted(&core, &[boundary_slow], None), 1);

    // Scenario: Retarget clamped to the proof-of-work limit
    // Lower the network proof-of-work limit to 2x BASE_BITS (0x201f_ffff), so
    // the 4x retarget on `parent_slow` clamps at the proof-of-work limit.
    let mut capped_params = params.clone();
    capped_params.max_attainable_target =
        Target::from_compact(CompactTarget::from_consensus(POW_LIMIT_CLAMP_BITS));
    let Core { store, config } = core;
    let core = Core {
        store,
        config: config.with_params(capped_params),
    };
    let capped_boundary = mine_with_bits(
        parent_slow.block_hash(),
        parent_slow.time + 2,
        1,
        POW_LIMIT_CLAMP_BITS,
    );
    assert_eq!(expect_accepted(&core, &[capped_boundary], None), 1);

    // Scenario: Testnet3 boundary retarget
    // On testnet3, a period boundary recomputes the retarget from the parent's
    // bits even when the gap after the parent exceeds twice the target spacing.
    let mut tnet_params = testnet_params();
    tnet_params.pow_target_timespan = 4;
    let Core { store, config } = core;
    {
        let txn = store.write().expect("write txn");
        txn.set_network(Network::Testnet).expect("set network");
        txn.commit().expect("commit");
    }
    let core = Core {
        store,
        config: config.with_params(tnet_params),
    };
    let tnet_boundary = mine_with_bits(
        parent_fast.block_hash(),
        parent_fast.time + 2_000,
        2,
        QUARTER_CLAMP_BITS,
    );
    assert_eq!(expect_accepted(&core, &[tnet_boundary], None), 1);
    let tnet_min_diff = header_with(
        parent_fast.block_hash(),
        parent_fast.time + 2_000,
        EASY_BITS,
    );
    let err = expect_err(&core, &[tnet_min_diff], None);
    assert!(
        matches!(err, Error::BadDifficulty { expected, got, .. }
            if expected == QUARTER_CLAMP_BITS && got == EASY_BITS),
        "got {err:?}"
    );

    // Scenario: Regtest boundary
    // Regtest keeps the parent's bits at a boundary.
    let Core { store, config } = core;
    {
        let txn = store.write().expect("write txn");
        txn.set_network(Network::Regtest).expect("set network");
        txn.commit().expect("commit");
    }
    let core = Core {
        store,
        config: config.with_params(Params::new(Network::Regtest)),
    };
    let regtest_boundary =
        mine_with_bits(parent_fast.block_hash(), parent_fast.time + 3, 3, BASE_BITS);
    assert_eq!(expect_accepted(&core, &[regtest_boundary], None), 1);

    // Scenario: Regtest retarget
    // The value a retarget would produce is refused on regtest.
    let wrong = header_with(
        parent_fast.block_hash(),
        parent_fast.time + 4,
        QUARTER_CLAMP_BITS,
    );
    let err = expect_err(&core, &[wrong], None);
    assert!(
        matches!(err, Error::BadDifficulty { expected, got, .. }
            if expected == BASE_BITS && got == QUARTER_CLAMP_BITS),
        "got {err:?}"
    );
}

/// HDR-07: the median of the previous 11 headers, and the sparse-window skip.
#[test]
fn hdr_07_median_time_past() {
    let universe = header_universe();

    // Scenario: Sparse window
    // 10 stored ancestors (`genesis` + 9 headers): one below the 11-block
    // window, so the check is skipped even for an old timestamp.
    let ten = mainnet_core(&[(CHECKPOINT_HEIGHT, universe.genesis)]);
    assert_eq!(expect_accepted(&ten, &universe.headers[..9], None), 9);
    let old_at_ten = mine(universe.headers[8].block_hash(), CHECKPOINT_TIME - 1_000, 3);
    assert_eq!(expect_accepted(&ten, &[old_at_ten], None), 1);

    // A fresh store has 1 stored ancestor; the same skip applies.
    let checkpoint = mine(BlockHash::all_zeros(), CHECKPOINT_TIME, 0);
    let sparse = mainnet_core(&[(CHECKPOINT_HEIGHT, checkpoint)]);
    let old_at_one = mine(checkpoint.block_hash(), CHECKPOINT_TIME - 1_000, 4);
    assert_eq!(expect_accepted(&sparse, &[old_at_one], None), 1);

    // Exactly 11 stored ancestors (`genesis` at +0 and `A1..A10` at +1..=10):
    // the median-time-past check activates at 11 ancestors, with median +5.
    let eleven = mainnet_core(&[(CHECKPOINT_HEIGHT, universe.genesis)]);
    let first_ten = &universe.headers[..10];
    assert_eq!(expect_accepted(&eleven, first_ten, None), 10);

    // Scenario: Timestamp equal to the median
    let equal_at_eleven = mine(first_ten[9].block_hash(), CHECKPOINT_TIME + 5, 1);
    let err = expect_err(&eleven, &[equal_at_eleven], None);
    assert!(
        matches!(err, Error::BadTimestamp { median, got, .. }
            if median == CHECKPOINT_TIME + 5 && got == CHECKPOINT_TIME + 5),
        "got {err:?}"
    );

    // Scenario: One second above the median
    let above_at_eleven = mine(first_ten[9].block_hash(), CHECKPOINT_TIME + 6, 2);
    assert_eq!(expect_accepted(&eleven, &[above_at_eleven], None), 1);

    // Scenario: Window excludes the new header
    // Above `A1..A11` (+1..=11, median +6), a new header at +7 is accepted
    // even though a window that wrongly included it would have median +7.
    let core = mainnet_core(&[(CHECKPOINT_HEIGHT, universe.genesis)]);
    let trunk = &universe.headers[..11];
    assert_eq!(expect_accepted(&core, trunk, None), 11);
    let above = mine(trunk[10].block_hash(), CHECKPOINT_TIME + 7, 2);
    assert_eq!(expect_accepted(&core, &[above], None), 1);
}

/// HDR-08: the two-hour future-time window is inclusive, and optional.
#[test]
fn hdr_08_future_time() {
    let genesis = mine(BlockHash::all_zeros(), CHECKPOINT_TIME, 0);
    let core = mainnet_core(&[(CHECKPOINT_HEIGHT, genesis)]);
    let parent = genesis.block_hash();

    // Scenario: Exactly two hours ahead
    let at_limit = mine(parent, (NOW + MAX_FUTURE_BLOCK_TIME) as u32, 0);
    assert_eq!(expect_accepted(&core, &[at_limit], Some(NOW)), 1);

    // Scenario: More than two hours ahead
    let over = mine(parent, (NOW + MAX_FUTURE_BLOCK_TIME + 1) as u32, 1);
    let err = expect_err(&core, &[over], Some(NOW));
    assert!(
        matches!(err, Error::TimestampTooNew { now, got, max_ahead, .. }
            if now == NOW
                && got == (NOW + MAX_FUTURE_BLOCK_TIME + 1) as u32
                && max_ahead == MAX_FUTURE_BLOCK_TIME),
        "got {err:?}"
    );

    // Scenario: No clock
    let no_clock = mine(parent, (NOW + 1_000_000) as u32, 2);
    assert_eq!(expect_accepted(&core, &[no_clock], None), 1);
}
