//! Executable specification for the `bitcoin-header-chain` capability.
//!
//! The state machine checks BTC-02 to BTC-07 over a delivery universe with a
//! trunk, a competing fork, a fork of a fork, a bad-PoW header, a
//! stale-timestamp header, an out-of-range target header, duplicates and empty
//! batches. BTC-03 skips the stored prefix and BTC-04 links the new run.
//!
//! The per-header rules live in `bitcoin_header_validation.rs`: HDR-01, HDR-02,
//! HDR-03 and HDR-07. The synthetic chain starts at a recent mainnet height
//! and relaxes the proof-of-work limit, so it mines in no time. The mainnet
//! estimated-work finality requirement is disabled because a synthetic chain
//! cannot clear it. Each spec scenario carries a `// Scenario:` comment at the
//! assertion that proves it.
//!
//! Run just this capability's spec:
//!
//! ```text
//! cargo test -p rsk-core --lib specs::bitcoin_header_chain
//! PROPTEST_VERBOSE=1 cargo test -p rsk-core --lib specs::bitcoin_header_chain
//! ```

use alloy_primitives::U256;
use bitcoin::Network;
use bitcoin::block::{BlockHash, Header as BitcoinHeader};
use bitcoin::hashes::Hash as _;
use bitcoin::pow::Work;
use proptest::prelude::*;
use proptest_state_machine::{ReferenceStateMachine, StateMachineTest, prop_state_machine};

use super::support::{
    CHECKPOINT_HEIGHT, CHECKPOINT_TIME, EASY_BITS, HeaderKind, HeaderUniverse, NOW,
    canonical_chain, expect_accepted, expect_err, header_universe, mainnet_core, mainnet_params,
    mine, mine_bad, mine_with_bits, submit, synthetic_mainnet_core, testnet_params, work,
};
use crate::{
    Action, Config, Core, DIFFICULTY_PERIOD, Error, ForkState, Outcome, RskState, StoredHeader,
    SubmitBitcoinOutcome, header_work,
};

// ── 1. Bitcoin delivery: forks, duplicates, errors ─────────────────────────

/// What the model expects a batch to do.
#[derive(Clone, Debug, PartialEq, Eq)]
enum HeaderPrediction {
    Noop {
        best_fork: u64,
        tip_height: u64,
    },
    Connected {
        accepted: usize,
        prev_best_fork: u64,
        best_fork: u64,
        tip_height: u64,
    },
    UnknownAncestor,
    LinkBreak,
    BadPow,
    BadTimestamp,
    BadTarget,
}

/// One fork in the abstract reference model.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ModelFork {
    id: u64,
    forkpoint: u64,
    parent: Option<u64>,
    /// Contiguous `(height, header, cumulative_work)` rows from `forkpoint`.
    rows: Vec<(u64, BitcoinHeader, Work)>,
}

impl ModelFork {
    fn tip_height(&self) -> u64 {
        self.rows.last().expect("fork has at least one row").0
    }

    fn tip_hash(&self) -> BlockHash {
        self.rows
            .last()
            .expect("fork has at least one row")
            .1
            .block_hash()
    }

    fn tip_work(&self) -> Work {
        self.rows.last().expect("fork has at least one row").2
    }

    fn to_fork_state(&self) -> ForkState {
        ForkState {
            id: self.id,
            forkpoint: self.forkpoint,
            parent: self.parent,
            tip_height: self.tip_height(),
            tip_hash: self.tip_hash().to_byte_array(),
            tip_work: self.tip_work().to_be_bytes(),
        }
    }
}

#[derive(Clone, Debug)]
struct HeaderModel {
    universe: HeaderUniverse,
    forks: Vec<ModelFork>,
    best_fork: u64,
    last: HeaderPrediction,
}

impl HeaderModel {
    /// Locate a stored header by hash across all forks: `(fork_id, height)`.
    fn locate(&self, hash: BlockHash) -> Option<(u64, u64)> {
        for fork in &self.forks {
            for (height, header, _) in &fork.rows {
                if header.block_hash() == hash {
                    return Some((fork.id, *height));
                }
            }
        }
        None
    }

    /// Resolve `(header, cumulative_work)` at `height` on `fork_id`, walking
    /// through ancestor forks below `forkpoint`.
    fn row_at(&self, fork_id: u64, height: u64) -> (BitcoinHeader, Work) {
        let mut cursor = fork_id;
        loop {
            let fork = &self.forks[cursor as usize];
            if height >= fork.forkpoint {
                let (_, header, cum_work) = fork
                    .rows
                    .iter()
                    .find(|(h, _, _)| *h == height)
                    .expect("contiguous heights on fork");
                return (*header, *cum_work);
            }
            cursor = fork.parent.expect("ancestor fork covers height");
        }
    }

    fn expected_forks(&self) -> Vec<ForkState> {
        self.forks.iter().map(ModelFork::to_fork_state).collect()
    }

    fn expected_stored_headers(&self) -> Vec<StoredHeader> {
        let mut rows = Vec::new();
        for fork in &self.forks {
            for (height, header, _) in &fork.rows {
                rows.push(StoredHeader {
                    fork: fork.id,
                    height: *height,
                    hash: header.block_hash(),
                });
            }
        }
        rows
    }

    fn expected_canonical_chain(&self) -> Vec<BitcoinHeader> {
        let tip = self.forks[self.best_fork as usize].tip_height();
        (CHECKPOINT_HEIGHT..=tip)
            .map(|h| self.row_at(self.best_fork, h).0)
            .collect()
    }
}

#[derive(Clone, Debug)]
struct HeaderTransition {
    batch: Vec<usize>,
}

/// Apply one transition to the reference model according to BTC-02..BTC-08 and
/// HDR-01..HDR-03, HDR-07.
fn apply_to_model(state: &mut HeaderModel, batch: &[usize]) {
    let prev_best_fork = state.best_fork;
    let current_tip = state.forks[prev_best_fork as usize].tip_height();
    let headers: Vec<BitcoinHeader> = batch.iter().map(|&i| state.universe.headers[i]).collect();

    // BTC-03: skip the longest leading run of stored headers.
    let Some(start) = headers
        .iter()
        .position(|header| state.locate(header.block_hash()).is_none())
    else {
        state.last = HeaderPrediction::Noop {
            best_fork: prev_best_fork,
            tip_height: current_tip,
        };
        return;
    };

    // HDR-01 / BTC-04: find the parent of the first new header.
    let parent_hash = if start == 0 {
        headers[0].prev_blockhash
    } else {
        headers[start - 1].block_hash()
    };
    let Some((parent_fork, parent_height)) = state.locate(parent_hash) else {
        state.last = HeaderPrediction::UnknownAncestor;
        return;
    };

    // BTC-04 and HDR-02, HDR-03, HDR-07 validation on the new run.
    let mut prev = parent_hash;
    for (offset, header) in headers[start..].iter().enumerate() {
        if header.prev_blockhash != prev {
            state.last = HeaderPrediction::LinkBreak;
            return;
        }
        match state.universe.kinds[batch[start + offset]] {
            HeaderKind::Valid => {}
            HeaderKind::BadTarget => {
                state.last = HeaderPrediction::BadTarget;
                return;
            }
            HeaderKind::BadTime => {
                state.last = HeaderPrediction::BadTimestamp;
                return;
            }
            HeaderKind::BadPow => {
                state.last = HeaderPrediction::BadPow;
                return;
            }
        }
        prev = header.block_hash();
    }

    // BTC-05 / BTC-06: extend the parent fork if `parent` is its tip, else
    // create a new fork at `parent_height + 1`.
    let parent_tip = state.forks[parent_fork as usize].tip_height();
    let mut cumulative = state.row_at(parent_fork, parent_height).1;
    let forkpoint = parent_height + 1;
    let mut new_rows = Vec::with_capacity(headers.len() - start);
    for (offset, header) in headers[start..].iter().enumerate() {
        cumulative = cumulative + header_work(header);
        new_rows.push((forkpoint + offset as u64, *header, cumulative));
    }

    let target_fork = if parent_height == parent_tip {
        state.forks[parent_fork as usize].rows.extend(new_rows);
        parent_fork
    } else {
        let id = state.forks.len() as u64;
        state.forks.push(ModelFork {
            id,
            forkpoint,
            parent: Some(parent_fork),
            rows: new_rows,
        });
        id
    };

    // BTC-07: switch best_fork only when strictly heavier.
    let best_work = state.forks[state.best_fork as usize].tip_work();
    let target_work = state.forks[target_fork as usize].tip_work();
    if target_fork != state.best_fork && target_work > best_work {
        state.best_fork = target_fork;
    }

    let tip_height = state.forks[state.best_fork as usize].tip_height();
    state.last = HeaderPrediction::Connected {
        accepted: headers.len() - start,
        prev_best_fork,
        best_fork: state.best_fork,
        tip_height,
    };
}

struct HeaderMachine;

impl ReferenceStateMachine for HeaderMachine {
    type State = HeaderModel;
    type Transition = HeaderTransition;

    fn init_state() -> BoxedStrategy<Self::State> {
        let universe = header_universe();
        let genesis = universe.genesis;
        let root = ModelFork {
            id: 0,
            forkpoint: CHECKPOINT_HEIGHT,
            parent: None,
            rows: vec![(CHECKPOINT_HEIGHT, genesis, header_work(&genesis))],
        };
        Just(HeaderModel {
            universe,
            forks: vec![root],
            best_fork: 0,
            last: HeaderPrediction::Noop {
                best_fork: 0,
                tip_height: CHECKPOINT_HEIGHT,
            },
        })
        .boxed()
    }

    fn transitions(state: &Self::State) -> BoxedStrategy<Self::Transition> {
        let batches = state.universe.batches.clone();
        (0..batches.len())
            .prop_map(move |i| HeaderTransition {
                batch: batches[i].clone(),
            })
            .boxed()
    }

    fn apply(mut state: Self::State, transition: &Self::Transition) -> Self::State {
        apply_to_model(&mut state, &transition.batch);
        state
    }
}

/// The SUT plus the history the invariants need: the canonical prefix at the
/// mark the first time it was finalized, and the chainwork of the previous
/// step.
struct HeaderSut {
    core: Core,
    finalized_prefix: Vec<BlockHash>,
    last_work: U256,
}

impl StateMachineTest for HeaderMachine {
    type SystemUnderTest = HeaderSut;
    type Reference = HeaderMachine;

    fn init_test(model: &HeaderModel) -> HeaderSut {
        let core = mainnet_core(&[(CHECKPOINT_HEIGHT, model.universe.genesis)]);
        let last_work = work(&core);
        HeaderSut {
            core,
            finalized_prefix: vec![model.universe.genesis.block_hash()],
            last_work,
        }
    }

    fn apply(mut sut: HeaderSut, model: &HeaderModel, transition: HeaderTransition) -> HeaderSut {
        let headers: Vec<BitcoinHeader> = transition
            .batch
            .iter()
            .map(|&i| model.universe.headers[i])
            .collect();
        let before = sut.core.state().expect("state");
        let outcome = sut.core.apply(Action::SubmitBitcoinHeaders {
            headers: &headers,
            now: None,
        });
        let after = sut.core.state().expect("state");

        // BTC-02, BTC-03, BTC-08: compare the outcome against the full model prediction.
        match (&model.last, &outcome) {
            (
                HeaderPrediction::Noop {
                    best_fork,
                    tip_height,
                },
                Ok(Outcome::BitcoinSubmitted(result)),
            ) => {
                assert_eq!(result.accepted, 0, "model expected a no-op");
                assert_eq!(result.prev_best_fork, Some(*best_fork));
                assert_eq!(result.best_fork, Some(*best_fork));
                assert_eq!(result.tip_height, *tip_height);
                assert_eq!(before, after, "a no-op changed the store");
            }
            (
                HeaderPrediction::Connected {
                    accepted,
                    prev_best_fork,
                    best_fork,
                    tip_height,
                },
                Ok(Outcome::BitcoinSubmitted(result)),
            ) => {
                assert_eq!(result.accepted, *accepted, "accepted count diverged");
                assert_eq!(
                    result.prev_best_fork,
                    Some(*prev_best_fork),
                    "prev_best_fork diverged"
                );
                assert_eq!(
                    result.best_fork,
                    Some(*best_fork),
                    "outcome best_fork diverged"
                );
                assert_eq!(
                    result.tip_height, *tip_height,
                    "outcome tip_height diverged"
                );
            }
            (HeaderPrediction::UnknownAncestor, Err(Error::UnknownAncestor(_)))
            | (HeaderPrediction::LinkBreak, Err(Error::LinkBreak { .. }))
            | (HeaderPrediction::BadPow, Err(Error::BadPow { .. }))
            | (HeaderPrediction::BadTimestamp, Err(Error::BadTimestamp { .. })) => {
                assert_eq!(before, after, "a rejected batch changed the store");
            }
            (HeaderPrediction::BadTarget, Err(Error::BadPow { detail, .. }))
                if detail == "target out of range" =>
            {
                assert_eq!(before, after, "a rejected target changed the store");
            }
            (expected, other) => panic!("model expected {expected:?}, core returned {other:?}"),
        }

        // The mark is monotone and the finalized prefix never moves.
        let mark = after.finalized.expect("initialized store has a mark");
        assert!(
            mark >= before.finalized.unwrap_or(0),
            "the finality mark regressed"
        );
        while CHECKPOINT_HEIGHT + sut.finalized_prefix.len() as u64 <= mark {
            let height = CHECKPOINT_HEIGHT + sut.finalized_prefix.len() as u64;
            let header = sut
                .core
                .header_at(height)
                .expect("read")
                .unwrap_or_else(|| panic!("no canonical header at {height}"));
            sut.finalized_prefix.push(header.block_hash());
        }
        sut.last_work = work(&sut.core);
        sut
    }

    fn check_invariants(sut: &HeaderSut, model: &HeaderModel) {
        let state = sut.core.state().expect("state");

        // BTC-05, BTC-06: every fork's id, parent, forkpoint, tip height, tip
        // hash and cumulative work must match the model.
        assert_eq!(
            state.forks,
            model.expected_forks(),
            "fork tree metadata diverged"
        );

        // BTC-06: every stored (fork, height, hash) row must match the model.
        let stored_rows: Vec<StoredHeader> = sut
            .core
            .stored_headers()
            .expect("read")
            .map(|row| row.expect("read"))
            .collect();
        assert_eq!(
            stored_rows,
            model.expected_stored_headers(),
            "stored (fork, height, hash) rows diverged"
        );

        // BTC-07: best_fork and canonical chain must match the model.
        assert_eq!(state.best_fork, Some(model.best_fork), "best_fork diverged");
        let chain = canonical_chain(&sut.core);
        assert_eq!(
            chain,
            model.expected_canonical_chain(),
            "canonical chain diverged"
        );

        let best_fork = &state.forks[model.best_fork as usize];
        assert_eq!(
            sut.core
                .chainwork()
                .expect("read")
                .expect("initialized")
                .to_be_bytes(),
            best_fork.tip_work
        );
        assert_eq!(
            sut.core
                .height_of(&chain.last().expect("an initial checkpoint").block_hash())
                .expect("read"),
            Some(best_fork.tip_height)
        );

        // BTC-07: no fork is strictly heavier than the best fork.
        let best_work = U256::from_be_slice(&best_fork.tip_work);
        for fork in &state.forks {
            let fork_work = U256::from_be_slice(&fork.tip_work);
            assert!(fork_work <= best_work, "the best fork is not the heaviest");
        }

        // BTC-07: no admitted fork can move a height at or below the mark.
        let mark = state.finalized.expect("initialized store has a mark");
        assert!(mark <= best_fork.tip_height);
        assert_eq!(
            sut.finalized_prefix.len() as u64,
            mark - CHECKPOINT_HEIGHT + 1
        );
        for (offset, expected) in sut.finalized_prefix.iter().enumerate() {
            let height = CHECKPOINT_HEIGHT + offset as u64;
            assert_eq!(
                sut.core
                    .header_at(height)
                    .expect("read")
                    .map(|header| header.block_hash()),
                Some(*expected),
                "height {height} is at or below the mark and moved"
            );
        }

        assert!(work(&sut.core) >= sut.last_work, "chainwork regressed");
        assert_eq!(
            state.rootstock,
            RskState::default(),
            "RSK state was touched"
        );
    }
}

// ── 2. Store identity (BTC-00, BTC-01) ──────────────────────────────────────

/// BTC-00: opening an empty store initializes it with the trusted
/// checkpoints.
#[test]
fn btc_00_trusted_checkpoints() {
    let first = mine(BlockHash::all_zeros(), CHECKPOINT_TIME, 0);
    let second = mine(first.block_hash(), CHECKPOINT_TIME + 1, 1);
    let high = CHECKPOINT_HEIGHT + DIFFICULTY_PERIOD;
    let core = mainnet_core(&[(CHECKPOINT_HEIGHT, first), (high, second)]);

    let state = core.state().expect("state");

    // Scenario: Fresh store
    assert_eq!(state.best_fork, Some(0));
    assert_eq!(state.forks.len(), 1);
    let root = &state.forks[0];
    assert_eq!(root.id, 0);
    assert_eq!(root.parent, None);
    assert_eq!(root.forkpoint, CHECKPOINT_HEIGHT);
    assert_eq!(root.tip_height, high);
    assert_eq!(root.tip_hash, second.block_hash().to_byte_array());

    let stored: Vec<_> = core
        .stored_headers()
        .expect("read")
        .map(|row| row.expect("read"))
        .collect();
    assert_eq!(stored.len(), 2);
    assert_eq!(
        (stored[0].fork, stored[0].height, stored[0].hash),
        (0, CHECKPOINT_HEIGHT, first.block_hash())
    );
    assert_eq!(
        (stored[1].fork, stored[1].height, stored[1].hash),
        (0, high, second.block_hash())
    );

    // Scenario: Initial cumulative work
    // One full period at the first checkpoint's bits, plus the second
    // checkpoint's own work.
    let expected_work = U256::from(2016u64)
        * U256::from_be_slice(&header_work(&first).to_be_bytes())
        + U256::from_be_slice(&header_work(&second).to_be_bytes());
    assert_eq!(root.tip_work, expected_work.to_be_bytes());

    // Scenario: Initial finality mark
    assert_eq!(state.finalized, Some(high));
    assert_eq!(canonical_chain(&core), vec![first, second]);

    // Scenario: Malformed checkpoint set
    // A start that is not a period boundary and a gap that is not one period
    // are both refused before a store is initialized.
    let not_a_boundary = Core::new(
        Config::in_memory(Network::Bitcoin)
            .with_params(mainnet_params())
            .without_partition_safety()
            .with_checkpoints(&[(CHECKPOINT_HEIGHT + 1, first)]),
    );
    assert!(
        matches!(not_a_boundary, Err(Error::Internal(_))),
        "a non-boundary start must be refused"
    );
    let uneven = Core::new(
        Config::in_memory(Network::Bitcoin)
            .with_params(mainnet_params())
            .without_partition_safety()
            .with_checkpoints(&[(CHECKPOINT_HEIGHT, first), (high + 1, second)]),
    );
    assert!(
        matches!(uneven, Err(Error::Internal(_))),
        "a non-period-spaced set must be refused"
    );
}

/// BTC-01: reopening compares the network and every stored checkpoint, and
/// changes nothing.
#[test]
fn btc_01_store_identity() {
    let path = std::env::temp_dir().join(format!(
        "rsk-core-btc-01-{}-identity.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);

    let first = mine(BlockHash::all_zeros(), CHECKPOINT_TIME, 0);
    let second = mine(first.block_hash(), CHECKPOINT_TIME + 1, 1);
    let high = CHECKPOINT_HEIGHT + DIFFICULTY_PERIOD;
    let checkpoints = [(CHECKPOINT_HEIGHT, first), (high, second)];

    let open = |checkpoints: &[(u64, BitcoinHeader)]| {
        let mut params = mainnet_params();
        params.no_pow_retargeting = true;
        Core::new(
            Config::open(&path, Network::Bitcoin)
                .with_params(params)
                .without_partition_safety()
                .with_checkpoints(checkpoints),
        )
    };

    let before = {
        let core = open(&checkpoints).expect("first open");
        core.state().expect("state")
    };

    // Scenario: Reopen
    // The same set: the open succeeds and adds no header.
    let core = open(&checkpoints).expect("reopen");
    assert_eq!(core.state().expect("state"), before);
    assert_eq!(core.stored_headers().expect("read").count(), 2);
    drop(core);

    // Scenario: Longer checkpoint set
    // A longer set on the same chain: the open succeeds and keeps the tip.
    let third = mine(second.block_hash(), CHECKPOINT_TIME + 2, 2);
    let longer = [
        (CHECKPOINT_HEIGHT, first),
        (high, second),
        (high + DIFFICULTY_PERIOD, third),
    ];
    let core = open(&longer).expect("longer checkpoint set");
    assert_eq!(core.tip_height().expect("read"), Some(high));
    assert_eq!(core.state().expect("state"), before);
    assert_eq!(
        core.header_at(high + DIFFICULTY_PERIOD).expect("read"),
        None
    );
    drop(core);

    // Scenario: Different network
    // A different network: refused, and the store is unchanged.
    let err = match Core::new(
        Config::open(&path, Network::Testnet)
            .with_params(testnet_params())
            .without_partition_safety()
            .with_checkpoints(&checkpoints),
    ) {
        Ok(_) => panic!("a different network must be refused"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        Error::NetworkMismatch {
            stored: Network::Bitcoin,
            opened: Network::Testnet,
        }
    ));
    let core = open(&checkpoints).expect("reopen after the network refusal");
    assert_eq!(core.state().expect("state"), before);
    drop(core);

    // Scenario: Different checkpoint
    // A different header at a checkpoint height: refused, and unchanged.
    let other = mine(BlockHash::all_zeros(), CHECKPOINT_TIME + 3, 3);
    let wrong = [(CHECKPOINT_HEIGHT, other), (high, second)];
    let err = match open(&wrong) {
        Ok(_) => panic!("a different checkpoint must be refused"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        Error::CheckpointMismatch { height, .. } if height == CHECKPOINT_HEIGHT
    ));
    let core = open(&checkpoints).expect("reopen after the checkpoint refusal");
    assert_eq!(core.state().expect("state"), before);
    drop(core);

    // The comparison visits every configured checkpoint, not only the first.
    let other_high = mine(second.block_hash(), CHECKPOINT_TIME + 4, 4);
    let wrong_high = [(CHECKPOINT_HEIGHT, first), (high, other_high)];
    let err = match open(&wrong_high) {
        Ok(_) => panic!("a different highest checkpoint must be refused"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        Error::CheckpointMismatch { height, .. } if height == high
    ));
    let core = open(&checkpoints).expect("reopen after the highest checkpoint refusal");
    assert_eq!(core.state().expect("state"), before);
    drop(core);

    // Sync the store past the next period boundary on a winning fork and verify
    // that reopening with a longer checkpoint set checks the canonical header
    // at `high + DIFFICULTY_PERIOD`.
    let (synced_boundary, synced_state) = {
        let core = open(&checkpoints).expect("open to sync");
        let root_step = mine(second.block_hash(), second.time + 1, 10);
        assert_eq!(expect_accepted(&core, &[root_step], None), 1);

        let mut fork_run = Vec::with_capacity(DIFFICULTY_PERIOD as usize);
        let mut prev = second;
        for i in 1..=DIFFICULTY_PERIOD as u32 {
            let next = mine(prev.block_hash(), second.time + i * 600, 100);
            fork_run.push(next);
            prev = next;
        }
        assert_eq!(
            expect_accepted(&core, &fork_run, None),
            DIFFICULTY_PERIOD as usize
        );
        let boundary = *fork_run.last().expect("period boundary");
        let state = core.state().expect("state");
        assert_ne!(state.best_fork, Some(0), "the winning fork must be best");
        (boundary, state)
    };

    let matching_longer = [
        (CHECKPOINT_HEIGHT, first),
        (high, second),
        (high + DIFFICULTY_PERIOD, synced_boundary),
    ];
    let core = open(&matching_longer).expect("reopen synced store with matching longer set");
    assert_eq!(core.state().expect("state"), synced_state);
    drop(core);

    let mismatching_longer = [
        (CHECKPOINT_HEIGHT, first),
        (high, second),
        (high + DIFFICULTY_PERIOD, third),
    ];
    let err = match open(&mismatching_longer) {
        Ok(_) => panic!("a conflicting checkpoint at a synced boundary must be refused"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        Error::CheckpointMismatch { height, .. } if height == high + DIFFICULTY_PERIOD
    ));
    let core = open(&matching_longer).expect("reopen after the synced checkpoint refusal");
    assert_eq!(core.state().expect("state"), synced_state);
    drop(core);

    let _ = std::fs::remove_file(&path);
}

// ── 3. Batch contract (BTC-02, BTC-03, BTC-04) ─────────────────────────────

/// The initial checkpoint every chain test starts from.
fn initial_checkpoint() -> BitcoinHeader {
    mine(BlockHash::all_zeros(), CHECKPOINT_TIME, 0)
}

/// `count` contiguous headers above `parent`.
fn contiguous(parent: BitcoinHeader, count: usize, tag: u32) -> Vec<BitcoinHeader> {
    let mut out = Vec::with_capacity(count);
    let mut prev = parent;
    for offset in 0..count {
        let next = mine(prev.block_hash(), prev.time + 1, tag + offset as u32);
        out.push(next);
        prev = next;
    }
    out
}

/// An initialized core with a stored trunk of `count` headers above the
/// initial checkpoint.
fn store_with_trunk(count: usize) -> (Core, BitcoinHeader, Vec<BitcoinHeader>) {
    let checkpoint = initial_checkpoint();
    let core = mainnet_core(&[(CHECKPOINT_HEIGHT, checkpoint)]);
    let trunk = contiguous(checkpoint, count, 0);
    assert_eq!(expect_accepted(&core, &trunk, None), count);
    (core, checkpoint, trunk)
}

/// BTC-02: a refused submission changes nothing, and a failed fork leaves no
/// fork.
#[test]
fn btc_02_atomicity() {
    let (core, _, trunk) = store_with_trunk(5);
    let before = core.state().expect("state");
    let stored = || {
        core.stored_headers()
            .expect("read")
            .map(|row| row.expect("read"))
            .collect::<Vec<_>>()
    };
    let stored_before = stored();

    // Scenario: Fork then failure
    // A new fork from a non-tip parent, then a broken link.
    let first = mine(trunk[1].block_hash(), trunk[1].time + 1, 10);
    let rogue = mine(trunk[3].block_hash(), trunk[3].time + 1, 11);
    let err = expect_err(&core, &[first, rogue], None);
    assert!(matches!(
        err,
        Error::LinkBreak { height, .. } if height == CHECKPOINT_HEIGHT + 4
    ));
    assert_eq!(
        core.state().expect("state"),
        before,
        "a failed fork changed the tree"
    );
    assert_eq!(stored(), stored_before, "a failed fork stored a header");

    // Scenario: Refused batch
    // A refused batch on the best fork.
    let bad = mine_bad(trunk[4].block_hash(), trunk[4].time + 1, EASY_BITS);
    let err = expect_err(&core, &[bad], None);
    assert!(matches!(err, Error::BadPow { .. }));
    assert_eq!(
        core.state().expect("state"),
        before,
        "a refused batch changed the tree"
    );
    assert_eq!(stored(), stored_before, "a refused batch stored a header");
}

/// BTC-03: the store skips the stored prefix, and a no-op changes nothing.
#[test]
fn btc_03_idempotency() {
    let (core, _, trunk) = store_with_trunk(6);

    // Scenario: Repeated prefix
    // A repeated prefix plus one new header.
    let next = mine(trunk[5].block_hash(), trunk[5].time + 1, 20);
    let mut batch = trunk[3..].to_vec();
    batch.push(next);
    assert_eq!(expect_accepted(&core, &batch, None), 1);
    assert!(
        core.stored_headers()
            .expect("read")
            .map(|row| row.expect("read").hash)
            .any(|hash| hash == next.block_hash()),
        "the new header is not stored"
    );

    // Scenario: All stored
    // The whole trunk is stored: a no-op.
    let before = core.state().expect("state");
    assert_eq!(expect_accepted(&core, &trunk, None), 0);
    assert_eq!(
        core.state().expect("state"),
        before,
        "an all-stored batch changed the store"
    );

    // Scenario: Empty
    // An empty batch is a no-op too.
    let before = core.state().expect("state");
    assert_eq!(expect_accepted(&core, &[], Some(NOW)), 0);
    assert_eq!(
        core.state().expect("state"),
        before,
        "an empty batch changed the store"
    );

    // Scenario: Prefix on a losing fork
    // A prefix on a losing fork extends that fork.
    let (core, _, trunk) = store_with_trunk(5);
    let branch = contiguous(trunk[1], 2, 30);
    assert_eq!(expect_accepted(&core, &branch, None), 2);
    let state = core.state().expect("state");
    assert_eq!(state.best_fork, Some(0), "the light branch became best");
    let branch_id = state
        .forks
        .iter()
        .find(|fork| fork.forkpoint == CHECKPOINT_HEIGHT + 3 && fork.parent == Some(0))
        .expect("the branch fork is listed")
        .id;

    let new = mine(branch[1].block_hash(), branch[1].time + 1, 40);
    let mut batch = branch.clone();
    batch.push(new);
    assert_eq!(expect_accepted(&core, &batch, None), 1);
    let state = core.state().expect("state");
    assert_eq!(state.best_fork, Some(0), "the tying branch became best");
    let branch_tip = state
        .forks
        .iter()
        .find(|fork| fork.id == branch_id)
        .expect("the branch fork is listed");
    assert_eq!(branch_tip.tip_height, CHECKPOINT_HEIGHT + 5);
    assert_eq!(branch_tip.tip_hash, new.block_hash().to_byte_array());
}

/// BTC-04: every new header names the header before it.
#[test]
fn btc_04_contiguity() {
    let (core, _, trunk) = store_with_trunk(8);
    let before = core.state().expect("state");

    // Scenario: Gap in the new run
    // A gap in the new run.
    let h1 = mine(trunk[7].block_hash(), trunk[7].time + 1, 50);
    let h3 = mine(trunk[6].block_hash(), trunk[6].time + 1, 51);
    let err = expect_err(&core, &[h1, h3], None);
    assert!(matches!(
        err,
        Error::LinkBreak { height, .. } if height == CHECKPOINT_HEIGHT + 10
    ));
    assert_eq!(
        core.state().expect("state"),
        before,
        "a gap changed the store"
    );

    // Scenario: Prefix link
    // The first new header does not name the last skipped header.
    let rogue = mine(trunk[5].block_hash(), trunk[5].time + 1, 52);
    let mut batch = trunk[0..2].to_vec();
    batch.push(rogue);
    let err = expect_err(&core, &batch, None);
    assert!(matches!(
        err,
        Error::LinkBreak { height, .. } if height == CHECKPOINT_HEIGHT + 3
    ));
    assert_eq!(
        core.state().expect("state"),
        before,
        "a bad prefix link changed the store"
    );

    // Scenario: Stored header after a new header
    // A stored header after a new header is not skipped.
    let first = mine(trunk[2].block_hash(), trunk[2].time + 1, 53);
    let err = expect_err(&core, &[first, trunk[4]], None);
    assert!(matches!(
        err,
        Error::LinkBreak { height, .. } if height == CHECKPOINT_HEIGHT + 5
    ));
    assert_eq!(
        core.state().expect("state"),
        before,
        "a stored header after a new header changed the store"
    );
}

// ── 4. Tree (BTC-05, BTC-06) ───────────────────────────────────────────────

/// BTC-05: the parent block, not the tip, chooses the fork.
#[test]
fn btc_05_forks() {
    let (core, _, trunk) = store_with_trunk(6);
    let root_tip = *trunk.last().expect("trunk");

    // Scenario: Extend the tip
    // Extend the tip: no fork is created.
    let next = mine(root_tip.block_hash(), root_tip.time + 1, 60);
    assert_eq!(expect_accepted(&core, &[next], None), 1);
    assert_eq!(core.state().expect("state").forks.len(), 1);
    assert_eq!(
        core.tip_height().expect("read"),
        Some(CHECKPOINT_HEIGHT + 7),
        "the tip did not extend"
    );

    // Scenario: Fork from a non-tip
    // Fork from a non-tip: a new fork at the parent's height plus one.
    let f1 = mine(trunk[3].block_hash(), trunk[3].time + 1, 61);
    assert_eq!(expect_accepted(&core, &[f1], None), 1);
    let (fork_a, height_a) = core
        .locate(&f1.block_hash())
        .expect("read")
        .expect("the first header is stored");
    assert_eq!(height_a, CHECKPOINT_HEIGHT + 5);
    let state = core.state().expect("state");
    let a = state
        .forks
        .iter()
        .find(|fork| fork.id == fork_a)
        .expect("the fork is listed");
    assert_eq!(a.forkpoint, CHECKPOINT_HEIGHT + 5);
    assert_eq!(a.parent, Some(0));
    assert_eq!(a.tip_height, CHECKPOINT_HEIGHT + 5);

    // Scenario: Siblings
    // Siblings: the same parent, different first headers.
    let b1 = mine(trunk[3].block_hash(), trunk[3].time + 2, 62);
    assert_eq!(expect_accepted(&core, &[b1], None), 1);
    let (fork_b, height_b) = core
        .locate(&b1.block_hash())
        .expect("read")
        .expect("the sibling header is stored");
    assert_eq!(height_b, CHECKPOINT_HEIGHT + 5);
    assert_ne!(fork_a, fork_b);
    let state = core.state().expect("state");
    let a = state
        .forks
        .iter()
        .find(|fork| fork.id == fork_a)
        .expect("fork A is listed");
    let b = state
        .forks
        .iter()
        .find(|fork| fork.id == fork_b)
        .expect("fork B is listed");
    assert_eq!(a.forkpoint, b.forkpoint);
    assert_ne!(a.tip_hash, b.tip_hash);

    // Scenario: Contiguous fork
    // A second submission at a fork tip: the fork fills contiguous heights.
    let f2 = mine(f1.block_hash(), f1.time + 1, 63);
    assert_eq!(expect_accepted(&core, &[f2], None), 1);
    let state = core.state().expect("state");
    let a = state
        .forks
        .iter()
        .find(|fork| fork.id == fork_a)
        .expect("fork A is listed");
    assert_eq!(a.tip_height, CHECKPOINT_HEIGHT + 6);
    let heights: Vec<u64> = core
        .stored_headers()
        .expect("read")
        .map(|row| row.expect("read"))
        .filter(|row| row.fork == fork_a)
        .map(|row| row.height)
        .collect();
    assert_eq!(heights, vec![CHECKPOINT_HEIGHT + 5, CHECKPOINT_HEIGHT + 6]);

    // Scenario: Fork of a fork
    // Fork of a fork: the parent is the fork that stores the parent block.
    // Scenario: Distinct first hash
    // Starting the batch with fork A's first header (`f1`) skips `f1` and
    // creates the new fork at `CHECKPOINT_HEIGHT + 6` with first header `c`,
    // so no two forks share a first hash.
    let c = mine(f1.block_hash(), f1.time + 2, 64);
    assert_eq!(expect_accepted(&core, &[f1, c], None), 1);
    let (fork_c, height_c) = core
        .locate(&c.block_hash())
        .expect("read")
        .expect("the fork-of-a-fork header is stored");
    assert_eq!(height_c, CHECKPOINT_HEIGHT + 6);
    let state = core.state().expect("state");
    let c_meta = state
        .forks
        .iter()
        .find(|fork| fork.id == fork_c)
        .expect("the fork of a fork is listed");
    assert_eq!(c_meta.parent, Some(fork_a));
    assert_eq!(c_meta.forkpoint, CHECKPOINT_HEIGHT + 6);

    // Scenario: Below the best tip
    // Below the best tip: the run attaches to the fork that stores the parent.
    let below = mine(trunk[2].block_hash(), trunk[2].time + 1, 65);
    let mut batch = vec![trunk[2]];
    batch.push(below);
    assert_eq!(expect_accepted(&core, &batch, None), 1);
    let (fork_d, height_d) = core
        .locate(&below.block_hash())
        .expect("read")
        .expect("the below-tip header is stored");
    assert_eq!(height_d, CHECKPOINT_HEIGHT + 4);
    let state = core.state().expect("state");
    let d = state
        .forks
        .iter()
        .find(|fork| fork.id == fork_d)
        .expect("the below-tip fork is listed");
    assert_eq!(d.parent, Some(0));
    assert_eq!(d.forkpoint, CHECKPOINT_HEIGHT + 4);

    // Every fork in the store has a distinct first header hash.
    let stored_rows: Vec<_> = core
        .stored_headers()
        .expect("read")
        .map(|row| row.expect("read"))
        .collect();
    let first_hashes: std::collections::HashSet<BlockHash> = state
        .forks
        .iter()
        .map(|fork| {
            stored_rows
                .iter()
                .find(|row| row.fork == fork.id && row.height == fork.forkpoint)
                .expect("every fork stores its first header")
                .hash
        })
        .collect();
    assert_eq!(
        first_hashes.len(),
        state.forks.len(),
        "two forks shared a first hash"
    );

    // Scenario: Below the best tip
    // With fork A best, a run from a block below its forkpoint attaches to the
    // root fork, and a run at the root fork's own tip extends it without
    // creating a fork.
    let extend = contiguous(f2, 10, 66);
    assert_eq!(expect_accepted(&core, &extend, None), 10);
    let state = core.state().expect("state");
    assert_eq!(
        state.best_fork,
        Some(fork_a),
        "fork A must be best before the below-tip runs"
    );
    let below_non_root = mine(trunk[2].block_hash(), trunk[2].time + 3, 67);
    assert_eq!(expect_accepted(&core, &[below_non_root], None), 1);
    let (fork_e, height_e) = core
        .locate(&below_non_root.block_hash())
        .expect("read")
        .expect("the below-tip header is stored");
    assert_eq!(height_e, CHECKPOINT_HEIGHT + 4);
    let state = core.state().expect("state");
    let e = state
        .forks
        .iter()
        .find(|fork| fork.id == fork_e)
        .expect("the below-tip fork is listed");
    assert_eq!(e.parent, Some(0));
    assert_eq!(e.forkpoint, CHECKPOINT_HEIGHT + 4);
    let forks_before = state.forks.len();
    let root_next = mine(next.block_hash(), next.time + 1, 68);
    assert_eq!(expect_accepted(&core, &[root_next], None), 1);
    let state = core.state().expect("state");
    assert_eq!(
        state.forks.len(),
        forks_before,
        "extending a tip created a fork"
    );
    assert_eq!(
        state.best_fork,
        Some(fork_a),
        "the heavier fork must stay best"
    );
}

/// BTC-06: the store keeps every header and every fork, and the tip work is
/// the parent work plus the run's work.
#[test]
fn btc_06_stored_set() {
    let (core, _, trunk) = store_with_trunk(6);

    // Scenario: Losing fork
    // A losing fork keeps its headers and its metadata.
    let branch = contiguous(trunk[3], 2, 70);
    assert_eq!(expect_accepted(&core, &branch, None), 2);
    let state = core.state().expect("state");
    assert_eq!(state.best_fork, Some(0));
    let branch_id = core
        .locate(&branch[0].block_hash())
        .expect("read")
        .expect("the branch is stored")
        .0;
    let branch_meta = state
        .forks
        .iter()
        .find(|fork| fork.id == branch_id)
        .expect("the branch fork is listed");
    assert_eq!(branch_meta.forkpoint, CHECKPOINT_HEIGHT + 5);
    assert_eq!(branch_meta.tip_height, CHECKPOINT_HEIGHT + 6);
    assert_eq!(
        branch_meta.tip_hash,
        branch[1].block_hash().to_byte_array(),
        "the losing fork tip hash is wrong"
    );
    let stored: Vec<_> = core
        .stored_headers()
        .expect("read")
        .map(|row| row.expect("read"))
        .collect();
    for header in &branch {
        assert!(
            stored
                .iter()
                .any(|row| row.fork == branch_id && row.hash == header.block_hash()),
            "a losing fork's header is missing"
        );
    }

    // Scenario: No removal
    // A heavier fork becomes best; the old best fork's headers stay stored.
    let heavy = contiguous(trunk[0], 7, 80);
    assert_eq!(expect_accepted(&core, &heavy, None), 7);
    let state = core.state().expect("state");
    let heavy_id = core
        .locate(&heavy[0].block_hash())
        .expect("read")
        .expect("the heavy fork is stored")
        .0;
    assert_eq!(state.best_fork, Some(heavy_id));
    let stored: Vec<_> = core
        .stored_headers()
        .expect("read")
        .map(|row| row.expect("read"))
        .collect();
    for height in 1..=6u64 {
        assert!(
            stored
                .iter()
                .any(|row| row.fork == 0 && row.height == CHECKPOINT_HEIGHT + height),
            "the old best fork lost a header"
        );
    }

    // Scenario: Cumulative work
    // Cumulative work: the tip work is the parent work plus the run's work.
    let (core, _, trunk) = store_with_trunk(6);
    let run = contiguous(trunk[5], 3, 90);
    assert_eq!(expect_accepted(&core, &run, None), 3);
    let parent_work = core
        .chainwork_at(CHECKPOINT_HEIGHT + 6)
        .expect("read")
        .expect("the parent has work");
    let tip_work = core
        .chainwork_at(CHECKPOINT_HEIGHT + 9)
        .expect("read")
        .expect("the tip has work");
    let run_work = run
        .iter()
        .map(header_work)
        .fold(Work::from_be_bytes([0; 32]), |acc, work| acc + work);
    assert_eq!(tip_work - parent_work, run_work);
    let root = core
        .state()
        .expect("state")
        .forks
        .into_iter()
        .find(|fork| fork.id == 0)
        .expect("the root fork is listed");
    assert_eq!(root.tip_work, tip_work.to_be_bytes());
}

// ── 5. Best fork and outcome (BTC-07, BTC-08) ─────────────────────────────

/// Submit `headers` and return the outcome, or panic.
fn submitted(core: &Core, headers: &[BitcoinHeader], now: Option<u64>) -> SubmitBitcoinOutcome {
    match submit(core, headers, now) {
        Outcome::BitcoinSubmitted(out) => out,
        other => panic!("expected a submitted outcome, got {other:?}"),
    }
}

/// BTC-07: strict work decides, ties keep the incumbent, and the root fork can
/// win back.
#[test]
fn btc_07_best_fork() {
    let (core, initial, trunk) = store_with_trunk(4);

    // Scenario: Lighter stays
    // Lighter stays.
    let lighter = contiguous(trunk[1], 1, 100);
    assert_eq!(expect_accepted(&core, &lighter, None), 1);
    assert_eq!(core.state().expect("state").best_fork, Some(0));

    // Scenario: Equal keeps
    // Equal keeps.
    let equal = contiguous(trunk[2], 1, 101);
    assert_eq!(expect_accepted(&core, &equal, None), 1);
    assert_eq!(core.state().expect("state").best_fork, Some(0));

    // Scenario: Heavier wins
    // Heavier wins.
    let heavy = contiguous(trunk[0], 5, 102);
    assert_eq!(expect_accepted(&core, &heavy, None), 5);
    let state = core.state().expect("state");
    let heavy_id = core
        .locate(&heavy[0].block_hash())
        .expect("read")
        .expect("the heavy fork is stored")
        .0;
    assert_eq!(state.best_fork, Some(heavy_id));
    let heavy_meta = state
        .forks
        .iter()
        .find(|fork| fork.id == heavy_id)
        .expect("the heavy fork is listed");
    assert_eq!(heavy_meta.tip_height, CHECKPOINT_HEIGHT + 6);

    // Scenario: Equal keeps
    // Extend the root fork to exactly the heavy fork's cumulative work: the
    // tie keeps the non-root incumbent.
    let tie = contiguous(trunk[3], 2, 105);
    assert_eq!(expect_accepted(&core, &tie, None), 2);
    assert_eq!(
        core.state().expect("state").best_fork,
        Some(heavy_id),
        "equal work must keep the incumbent, not the root fork"
    );

    // Scenario: Canonical
    // Canonical: the best fork's chain from the checkpoint height is canonical.
    let chain = canonical_chain(&core);
    assert_eq!(chain.len() as u64, 7);
    assert_eq!(chain[0].block_hash(), initial.block_hash());
    assert_eq!(chain[6].block_hash(), heavy[4].block_hash());
    assert_eq!(
        core.tip_height().expect("read"),
        Some(CHECKPOINT_HEIGHT + 6)
    );

    // Scenario: Later win
    // Later win: the stored, lighter fork becomes strictly heavier.
    let lighter_id = core
        .locate(&lighter[0].block_hash())
        .expect("read")
        .expect("the lighter fork is stored")
        .0;
    let mut later = lighter.clone();
    later.extend(contiguous(lighter[0], 4, 103));
    assert_eq!(expect_accepted(&core, &later, None), 4);
    let state = core.state().expect("state");
    assert_eq!(state.best_fork, Some(lighter_id));
    let lighter_meta = state
        .forks
        .iter()
        .find(|fork| fork.id == lighter_id)
        .expect("the lighter fork is listed");
    assert_eq!(lighter_meta.tip_height, CHECKPOINT_HEIGHT + 7);

    // Scenario: Root wins back
    // Root wins back from its tied tip, so it is strictly heavier than the
    // lighter fork.
    let more = contiguous(tie[1], 5, 104);
    assert_eq!(expect_accepted(&core, &more, None), 5);
    let state = core.state().expect("state");
    assert_eq!(state.best_fork, Some(0));
    assert_eq!(
        core.tip_height().expect("read"),
        Some(CHECKPOINT_HEIGHT + 11)
    );

    // Scenario: Shorter but heavier fork wins
    // On testnet3, a 2-block normal-difficulty fork (per-block work 16, total
    // branch work 32) outweighs a 3-block minimum-difficulty trunk (per-block
    // work 2, total branch work 6) and becomes best despite a lower tip height.
    const NORMAL_BITS: u32 = 0x200f_ffff;
    let tnet_genesis = mine_with_bits(BlockHash::all_zeros(), CHECKPOINT_TIME, 0, NORMAL_BITS);
    let tnet_core = synthetic_mainnet_core(testnet_params(), &[(0, tnet_genesis)]);

    let min1 = mine_with_bits(
        tnet_genesis.block_hash(),
        CHECKPOINT_TIME + 1_201,
        1,
        EASY_BITS,
    );
    let min2 = mine_with_bits(min1.block_hash(), min1.time + 1_201, 2, EASY_BITS);
    let min3 = mine_with_bits(min2.block_hash(), min2.time + 1_201, 3, EASY_BITS);
    assert_eq!(expect_accepted(&tnet_core, &[min1, min2, min3], None), 3);
    assert_eq!(tnet_core.state().expect("state").best_fork, Some(0));
    assert_eq!(tnet_core.tip_height().expect("read"), Some(3));

    let norm1 = mine_with_bits(
        tnet_genesis.block_hash(),
        CHECKPOINT_TIME + 600,
        10,
        NORMAL_BITS,
    );
    let norm2 = mine_with_bits(norm1.block_hash(), norm1.time + 600, 11, NORMAL_BITS);
    assert_eq!(expect_accepted(&tnet_core, &[norm1, norm2], None), 2);
    let (shorter_fork_id, _) = tnet_core
        .locate(&norm1.block_hash())
        .expect("read")
        .expect("the shorter heavier fork is stored");
    assert_eq!(
        tnet_core.state().expect("state").best_fork,
        Some(shorter_fork_id),
        "a shorter fork with more cumulative work must become best"
    );
    assert_eq!(
        tnet_core.tip_height().expect("read"),
        Some(2),
        "canonical tip height must follow the shorter heavier fork"
    );
}

/// BTC-08: the four chain outcome fields.
#[test]
fn btc_08_outcome() {
    let (core, _, trunk) = store_with_trunk(4);

    // Scenario: Extend
    // Extend: same best fork, new tip.
    let next = mine(trunk[3].block_hash(), trunk[3].time + 1, 110);
    let out = submitted(&core, &[next], None);
    assert_eq!(out.accepted, 1);
    assert_eq!(out.prev_best_fork, Some(0));
    assert_eq!(out.best_fork, Some(0));
    assert_eq!(out.tip_height, CHECKPOINT_HEIGHT + 5);

    // Scenario: Switch
    // Switch: previous best, new best, the new fork's tip.
    let heavy = contiguous(trunk[0], 5, 111);
    let out = submitted(&core, &heavy, None);
    let heavy_id = core
        .locate(&heavy[0].block_hash())
        .expect("read")
        .expect("the heavy fork is stored")
        .0;
    assert_eq!(out.accepted, 5);
    assert_eq!(out.prev_best_fork, Some(0));
    assert_eq!(out.best_fork, Some(heavy_id));
    assert_eq!(out.tip_height, CHECKPOINT_HEIGHT + 6);

    // Scenario: No-op
    // No-op: zero accepted, the same best fork, the current canonical tip.
    let out = submitted(&core, &trunk, None);
    assert_eq!(out.accepted, 0);
    assert_eq!(out.prev_best_fork, Some(heavy_id));
    assert_eq!(out.best_fork, Some(heavy_id));
    assert_eq!(out.tip_height, CHECKPOINT_HEIGHT + 6);

    // Scenario: Losing fork
    // Losing fork: the accepted count, the same best fork, the canonical tip.
    let light = mine(trunk[3].block_hash(), trunk[3].time + 1, 112);
    let out = submitted(&core, &[light], None);
    assert_eq!(out.accepted, 1);
    assert_eq!(out.prev_best_fork, Some(heavy_id));
    assert_eq!(out.best_fork, Some(heavy_id));
    assert_eq!(out.tip_height, CHECKPOINT_HEIGHT + 6);
}

prop_state_machine! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        .. ProptestConfig::default()
    })]

    /// BTC-02–BTC-07, HDR-01–HDR-03, HDR-07: acceptance, storage and fork
    /// choice on the delivery universe.
    #[test]
    fn bitcoin_header_chain_matches_the_spec(sequential 1..32 => HeaderMachine);
}
