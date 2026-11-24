//! Remaining executable specifications for the legacy `specs/bitcoin-headers.md`.
//!
//! The chain delivery machine moved to `bitcoin_header_chain.rs` with the
//! `bitcoin-header-chain` capability. This file keeps the clauses that do not
//! have an OpenSpec capability yet:
//!
//! - `finality_and_fork_gate_matches_the_spec` checks E3 and E6 on a long trunk
//!   delivered in runs, at the production depth, including light forks
//!   below/above the mark.
//! - `difficulty_matches_the_spec` checks HDR-04 with the mainnet expected-bits
//!   rule.
//! - `future_time_matches_the_spec` checks HDR-08 against the caller clock.
//!
//! `the_finality_requirement_gates_the_mark` pins E6. The rule scenarios live in
//! `bitcoin_header_validation.rs`; the chain scenarios live in
//! `bitcoin_header_chain.rs`. Clause IDs in comments (`E*`) stay until the
//! `bitcoin-finality` capability replaces them.
//!
//! The synthetic chain starts at a recent mainnet height and relaxes the
//! proof-of-work limit, so it mines in no time. The mainnet estimated-work
//! finality requirement is disabled because a synthetic chain cannot clear it.
//!
//! Run just this document's spec:
//!
//! ```text
//! cargo test -p rsk-core --lib specs::bitcoin_headers
//! PROPTEST_VERBOSE=1 cargo test -p rsk-core --lib specs::bitcoin_headers
//! ```

use std::collections::HashSet;

use alloy_primitives::U256;
use bitcoin::Network;
use bitcoin::block::{BlockHash, Header as BitcoinHeader};
use bitcoin::hashes::Hash as _;
use proptest::prelude::*;
use proptest_state_machine::{ReferenceStateMachine, StateMachineTest, prop_state_machine};

use super::support::{
    CHECKPOINT_HEIGHT, CHECKPOINT_TIME, EASY_BITS, NOW, canonical_chain, mainnet_core,
    mainnet_params, mine, mine_with_bits, work,
};
use crate::{Action, Config, Core, Error, Outcome, RskState};

// ── 2. Finality: a trunk, the production depth and a fork gate ──────────────

/// Long enough that a batch-driven run buries a mark at the production depth.
const FINALITY_BLOCKS: usize = 240;
/// Trunk headers delivered per `Advance`, so a short proptest sequence can
/// still reach the coinbase-maturity depth.
const FINALITY_BATCH: usize = 12;

#[derive(Clone, Debug)]
struct FinalityUniverse {
    genesis: BitcoinHeader,
    headers: Vec<BitcoinHeader>,
    /// A light child of each trunk header, used to attempt forks. The child of
    /// header `i` has height `CHECKPOINT_HEIGHT + i + 2`; it can never outweigh the
    /// trunk when the parent is below the tip.
    forks: Vec<BitcoinHeader>,
}

fn finality_universe() -> FinalityUniverse {
    let genesis = mine(BlockHash::all_zeros(), CHECKPOINT_TIME, 0);
    let mut headers = Vec::with_capacity(FINALITY_BLOCKS);
    let mut prev = genesis;
    for i in 1..=FINALITY_BLOCKS as u32 {
        prev = mine(prev.block_hash(), CHECKPOINT_TIME + i, 0);
        headers.push(prev);
    }
    let forks = headers
        .iter()
        .enumerate()
        .map(|(i, header)| {
            mine(
                header.block_hash(),
                header.time + 1_000 + i as u32,
                900 + i as u32,
            )
        })
        .collect();
    FinalityUniverse {
        genesis,
        headers,
        forks,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FinalityPrediction {
    Noop,
    UnknownAncestor,
    Connected { connected: usize, advanced: bool },
    ForkStored,
    ForkBelowFinalized { forkpoint: u64, finalized: u64 },
}

#[derive(Clone, Debug)]
struct FinalityModel {
    universe: FinalityUniverse,
    stored: HashSet<BlockHash>,
    tip: u64,
    mark: u64,
    last: FinalityPrediction,
}

#[derive(Clone, Debug)]
enum FinalityTransition {
    /// One header by index: a duplicate, an orphan, or the next trunk header.
    Deliver(usize),
    /// A contiguous run of fresh trunk headers, the way a sync delivers them.
    Advance {
        start: usize,
        len: usize,
    },
    Fork(usize),
}

struct FinalityMachine;

impl ReferenceStateMachine for FinalityMachine {
    type State = FinalityModel;
    type Transition = FinalityTransition;

    fn init_state() -> BoxedStrategy<Self::State> {
        let universe = finality_universe();
        let mut stored = HashSet::new();
        stored.insert(universe.genesis.block_hash());
        Just(FinalityModel {
            universe,
            stored,
            tip: CHECKPOINT_HEIGHT,
            mark: CHECKPOINT_HEIGHT,
            last: FinalityPrediction::Noop,
        })
        .boxed()
    }

    fn transitions(state: &Self::State) -> BoxedStrategy<Self::Transition> {
        let len = state.universe.headers.len();
        let frontier = state.tip.saturating_sub(CHECKPOINT_HEIGHT) as usize;
        let any_deliver = (0..len).prop_map(FinalityTransition::Deliver);
        if frontier >= len {
            // The trunk is exhausted: only duplicates and forks remain.
            let parents = 0..(frontier - 1);
            return prop_oneof![
                1 => any_deliver,
                2 => parents.prop_map(FinalityTransition::Fork),
            ]
            .boxed();
        }
        let advance = (1usize..=FINALITY_BATCH).prop_map(move |n| FinalityTransition::Advance {
            start: frontier,
            len: n.min(len - frontier),
        });
        if frontier < 3 {
            return prop_oneof![3 => advance, 1 => any_deliver].boxed();
        }
        // A branch must leave the current tip as the tip: parents are kept one
        // below it, so the fork can at most tie the trunk's work.
        let parents = 0..(frontier - 1);
        prop_oneof![
            3 => advance,
            1 => any_deliver,
            2 => parents.prop_map(FinalityTransition::Fork),
        ]
        .boxed()
    }

    fn apply(mut state: Self::State, transition: &Self::Transition) -> Self::State {
        match transition {
            FinalityTransition::Deliver(index) => {
                let header = state.universe.headers[*index];
                let height = CHECKPOINT_HEIGHT + *index as u64 + 1;
                if state.stored.contains(&header.block_hash()) {
                    state.last = FinalityPrediction::Noop;
                } else if !state.stored.contains(&header.prev_blockhash) {
                    state.last = FinalityPrediction::UnknownAncestor;
                } else {
                    state.stored.insert(header.block_hash());
                    state.tip = state.tip.max(height);
                    let before = state.mark;
                    advance_mark(&mut state);
                    state.last = FinalityPrediction::Connected {
                        connected: 1,
                        advanced: state.mark > before,
                    };
                }
            }
            FinalityTransition::Advance { start, len } => {
                let before = state.mark;
                for offset in 0..*len {
                    let header = state.universe.headers[start + offset];
                    state.stored.insert(header.block_hash());
                    state.tip = state
                        .tip
                        .max(CHECKPOINT_HEIGHT + (start + offset) as u64 + 1);
                }
                // One finality step per batch, like the core: the rule runs after
                // the whole run is stored.
                advance_mark(&mut state);
                state.last = FinalityPrediction::Connected {
                    connected: *len,
                    advanced: state.mark > before,
                };
            }
            FinalityTransition::Fork(index) => {
                let fork = state.universe.forks[*index];
                if state.stored.contains(&fork.block_hash()) {
                    state.last = FinalityPrediction::Noop;
                } else {
                    // E3: a fork at or below the mark is refused. A fork above
                    // the mark is stored, but it can only tie the trunk here.
                    let forkpoint = CHECKPOINT_HEIGHT + *index as u64 + 2;
                    if forkpoint <= state.mark {
                        state.last = FinalityPrediction::ForkBelowFinalized {
                            forkpoint,
                            finalized: state.mark,
                        };
                    } else {
                        state.stored.insert(fork.block_hash());
                        state.last = FinalityPrediction::ForkStored;
                    }
                }
            }
        }
        state
    }
}

/// E6: the mark follows the tip at the production depth.
///
/// The specs disable the mainnet partition-safety requirement. The remaining
/// requirement is the tip block's own work, which the tip always carries above
/// the mark, so nothing else gates the advance.
fn advance_mark(state: &mut FinalityModel) {
    let candidate = state.tip.saturating_sub(crate::COINBASE_MATURITY);
    if candidate > state.mark {
        state.mark = candidate;
    }
}

struct FinalitySut {
    core: Core,
    finalized_prefix: Vec<BlockHash>,
    last_work: U256,
}

impl StateMachineTest for FinalityMachine {
    type SystemUnderTest = FinalitySut;
    type Reference = FinalityMachine;

    fn init_test(model: &FinalityModel) -> FinalitySut {
        let core = mainnet_core(&[(CHECKPOINT_HEIGHT, model.universe.genesis)]);
        let last_work = work(&core);
        FinalitySut {
            core,
            finalized_prefix: vec![model.universe.genesis.block_hash()],
            last_work,
        }
    }

    fn apply(
        mut sut: FinalitySut,
        model: &FinalityModel,
        transition: FinalityTransition,
    ) -> FinalitySut {
        let headers: &[BitcoinHeader] = match transition {
            FinalityTransition::Deliver(index) => {
                std::slice::from_ref(&model.universe.headers[index])
            }
            FinalityTransition::Advance { start, len } => {
                &model.universe.headers[start..start + len]
            }
            FinalityTransition::Fork(index) => std::slice::from_ref(&model.universe.forks[index]),
        };
        let before = sut.core.state().expect("state");
        let outcome = sut
            .core
            .apply(Action::SubmitBitcoinHeaders { headers, now: None });
        let after = sut.core.state().expect("state");

        // E6: `SubmitBitcoinOutcome::finalized` reports exactly the mark movement the
        // model predicted.
        match (&model.last, &outcome) {
            (FinalityPrediction::Noop, Ok(Outcome::BitcoinSubmitted(result))) => {
                assert_eq!(result.accepted, 0, "model expected a no-op");
                assert!(result.finalized.is_none(), "a no-op advanced the mark");
            }
            (FinalityPrediction::UnknownAncestor, Err(Error::UnknownAncestor(_))) => {
                assert_eq!(before, after, "a rejected header changed the store");
            }
            (
                FinalityPrediction::Connected {
                    connected,
                    advanced: true,
                },
                Ok(Outcome::BitcoinSubmitted(result)),
            ) => {
                assert_eq!(result.accepted, *connected);
                assert_eq!(result.tip_height, model.tip);
                assert_eq!(
                    result.finalized,
                    Some(model.mark),
                    "the submission did not report the new mark"
                );
            }
            (
                FinalityPrediction::Connected {
                    connected,
                    advanced: false,
                },
                Ok(Outcome::BitcoinSubmitted(result)),
            ) => {
                assert_eq!(result.accepted, *connected);
                assert_eq!(result.tip_height, model.tip);
                assert_eq!(result.finalized, None, "the mark advanced unexpectedly");
            }
            (FinalityPrediction::ForkStored, Ok(Outcome::BitcoinSubmitted(result))) => {
                assert_eq!(result.accepted, 1);
                assert_eq!(after.best_fork, Some(0), "a light fork became best");
                assert!(result.finalized.is_none(), "a fork advanced the mark");
            }
            (
                FinalityPrediction::ForkBelowFinalized {
                    forkpoint,
                    finalized,
                },
                Err(Error::ForkBelowFinalized {
                    fork,
                    finalized: got,
                }),
            ) => {
                assert_eq!(*forkpoint, *fork);
                assert_eq!(*finalized, *got);
                assert_eq!(before, after, "a refused fork changed the store");
            }
            (expected, other) => panic!("model expected {expected:?}, core returned {other:?}"),
        }

        let mark = after.finalized.expect("initialized store has a mark");
        while CHECKPOINT_HEIGHT + sut.finalized_prefix.len() as u64 <= mark {
            let height = CHECKPOINT_HEIGHT + sut.finalized_prefix.len() as u64;
            sut.finalized_prefix.push(
                sut.core
                    .header_at(height)
                    .expect("read")
                    .unwrap_or_else(|| panic!("no canonical header at {height}"))
                    .block_hash(),
            );
        }
        sut.last_work = work(&sut.core);
        sut
    }

    fn check_invariants(sut: &FinalitySut, model: &FinalityModel) {
        let state = sut.core.state().expect("state");
        // E6: the mark is the model's mark and sits exactly at the depth.
        assert_eq!(state.finalized, Some(model.mark));
        assert_eq!(state.best_fork, Some(0));
        assert_eq!(sut.core.tip_height().expect("read"), Some(model.tip));
        assert_eq!(
            model.mark,
            model
                .tip
                .saturating_sub(crate::COINBASE_MATURITY)
                .max(CHECKPOINT_HEIGHT),
            "the mark is not tip - depth"
        );

        // E4 (shared): the stored set includes the branches.
        let stored: HashSet<BlockHash> = sut
            .core
            .stored_headers()
            .expect("read")
            .map(|header| header.expect("read").hash)
            .collect();
        assert_eq!(stored, model.stored, "the stored header set diverged");
        // One root fork plus one fork per stored branch header.
        let trunk = model.tip - CHECKPOINT_HEIGHT + 1;
        let branches = model.stored.len() as u64 - trunk;
        assert_eq!(
            state.forks.len() as u64,
            1 + branches,
            "a stored branch must have its own fork"
        );

        // S3 (shared): the best chain still links through the trunk.
        let chain = canonical_chain(&sut.core);
        assert_eq!(chain.len() as u64, trunk);
        assert_eq!(chain[0].block_hash(), model.universe.genesis.block_hash());
        for pair in chain.windows(2) {
            assert_eq!(pair[1].prev_blockhash, pair[0].block_hash());
        }
        assert_eq!(
            sut.finalized_prefix.len() as u64,
            model.mark - CHECKPOINT_HEIGHT + 1
        );
        assert_eq!(
            sut.finalized_prefix,
            chain[..=(model.mark - CHECKPOINT_HEIGHT) as usize]
                .iter()
                .map(|header| header.block_hash())
                .collect::<Vec<_>>()
        );
        assert!(work(&sut.core) >= sut.last_work, "chainwork regressed");
        assert_eq!(
            state.rootstock,
            RskState::default(),
            "RSK state was touched"
        );
    }
}

// ── 3. Expected difficulty: a network that enforces the retarget ────────────

#[derive(Clone, Debug)]
struct DifficultyUniverse {
    genesis: BitcoinHeader,
    good: BitcoinHeader,
    wrong: BitcoinHeader,
}

fn difficulty_universe() -> DifficultyUniverse {
    let genesis = mine(BlockHash::all_zeros(), CHECKPOINT_TIME, 0);
    let good = mine(genesis.block_hash(), CHECKPOINT_TIME + 1, 0);
    // A valid header at a slightly different target. HDR-04 rejects it for the
    // bits, before HDR-02 or PoW.
    let wrong = mine_with_bits(genesis.block_hash(), CHECKPOINT_TIME + 1, 1, EASY_BITS - 1);
    DifficultyUniverse {
        genesis,
        good,
        wrong,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DifficultyPrediction {
    Noop,
    Connected,
    BadDifficulty,
}

#[derive(Clone, Debug)]
struct DifficultyModel {
    universe: DifficultyUniverse,
    stored: HashSet<BlockHash>,
    last: DifficultyPrediction,
}

#[derive(Clone, Debug)]
enum DifficultyTransition {
    Good,
    Wrong,
}

struct DifficultyMachine;

impl ReferenceStateMachine for DifficultyMachine {
    type State = DifficultyModel;
    type Transition = DifficultyTransition;

    fn init_state() -> BoxedStrategy<Self::State> {
        let universe = difficulty_universe();
        let mut stored = HashSet::new();
        stored.insert(universe.genesis.block_hash());
        Just(DifficultyModel {
            universe,
            stored,
            last: DifficultyPrediction::Noop,
        })
        .boxed()
    }

    fn transitions(_state: &Self::State) -> BoxedStrategy<Self::Transition> {
        prop_oneof![
            Just(DifficultyTransition::Good),
            Just(DifficultyTransition::Wrong),
        ]
        .boxed()
    }

    fn apply(mut state: Self::State, transition: &Self::Transition) -> Self::State {
        match transition {
            DifficultyTransition::Good => {
                if state.stored.contains(&state.universe.good.block_hash()) {
                    state.last = DifficultyPrediction::Noop;
                } else {
                    state.stored.insert(state.universe.good.block_hash());
                    state.last = DifficultyPrediction::Connected;
                }
            }
            DifficultyTransition::Wrong => {
                state.last = if state.stored.contains(&state.universe.wrong.block_hash()) {
                    DifficultyPrediction::Noop
                } else {
                    // HDR-04: mainnet enforces the retarget schedule. Off a period
                    // boundary, the child must carry the parent's bits.
                    DifficultyPrediction::BadDifficulty
                };
            }
        }
        state
    }
}

struct DifficultySut {
    core: Core,
}

impl StateMachineTest for DifficultyMachine {
    type SystemUnderTest = DifficultySut;
    type Reference = DifficultyMachine;

    fn init_test(model: &DifficultyModel) -> DifficultySut {
        // Mainnet enforces the expected-bits rule. Only the proof-of-work limit
        // is relaxed, so the test can mine without mainnet work.
        let core = mainnet_core(&[(CHECKPOINT_HEIGHT, model.universe.genesis)]);
        DifficultySut { core }
    }

    fn apply(
        sut: DifficultySut,
        model: &DifficultyModel,
        transition: DifficultyTransition,
    ) -> DifficultySut {
        let header = match transition {
            DifficultyTransition::Good => model.universe.good,
            DifficultyTransition::Wrong => model.universe.wrong,
        };
        let before = sut.core.state().expect("state");
        let outcome = sut.core.apply(Action::SubmitBitcoinHeaders {
            headers: std::slice::from_ref(&header),
            now: None,
        });
        let after = sut.core.state().expect("state");

        match (&model.last, &outcome) {
            (DifficultyPrediction::Noop, Ok(Outcome::BitcoinSubmitted(result))) => {
                assert_eq!(result.accepted, 0, "model expected a no-op");
                assert_eq!(before, after, "a no-op changed the store");
            }
            (DifficultyPrediction::Connected, Ok(Outcome::BitcoinSubmitted(result))) => {
                assert_eq!(result.accepted, 1);
                assert_eq!(after.best_fork, Some(0));
            }
            (
                DifficultyPrediction::BadDifficulty,
                Err(Error::BadDifficulty {
                    height,
                    expected,
                    got,
                }),
            ) => {
                assert_eq!(*height, CHECKPOINT_HEIGHT + 1);
                assert_eq!(*expected, EASY_BITS);
                assert_eq!(*got, EASY_BITS - 1);
                assert_eq!(before, after, "a bad header changed the store");
            }
            (expected, other) => panic!("model expected {expected:?}, core returned {other:?}"),
        }
        sut
    }

    fn check_invariants(sut: &DifficultySut, model: &DifficultyModel) {
        let state = sut.core.state().expect("state");
        let stored: HashSet<BlockHash> = sut
            .core
            .stored_headers()
            .expect("read")
            .map(|header| header.expect("read").hash)
            .collect();
        assert_eq!(stored, model.stored, "the stored header set diverged");
        assert_eq!(state.best_fork, Some(0));
        assert_eq!(state.finalized, Some(CHECKPOINT_HEIGHT));

        let chain = canonical_chain(&sut.core);
        assert_eq!(chain[0].block_hash(), model.universe.genesis.block_hash());
        for pair in chain.windows(2) {
            assert_eq!(pair[1].prev_blockhash, pair[0].block_hash());
        }
        assert_eq!(state.rootstock, RskState::default());
    }
}

// ── 4. Future time: a header beyond the caller clock ────────────────────────

#[derive(Clone, Debug)]
pub(super) struct FutureUniverse {
    pub(super) genesis: BitcoinHeader,
    good: BitcoinHeader,
    future: BitcoinHeader,
}

pub(super) fn future_universe() -> FutureUniverse {
    let genesis = mine(BlockHash::all_zeros(), CHECKPOINT_TIME, 0);
    let good = mine(genesis.block_hash(), CHECKPOINT_TIME + 1, 0);
    // One second beyond the two-hour window in `submit_bitcoin_headers`.
    let future = mine(
        genesis.block_hash(),
        (NOW + crate::btc::validation::MAX_FUTURE_BLOCK_TIME + 1) as u32,
        1,
    );
    FutureUniverse {
        genesis,
        good,
        future,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FuturePrediction {
    Noop,
    Connected,
    TimestampTooNew,
}

#[derive(Clone, Debug)]
struct FutureModel {
    universe: FutureUniverse,
    stored: HashSet<BlockHash>,
    last: FuturePrediction,
}

#[derive(Clone, Debug)]
enum FutureTransition {
    Good,
    Future,
}

struct FutureMachine;

impl ReferenceStateMachine for FutureMachine {
    type State = FutureModel;
    type Transition = FutureTransition;

    fn init_state() -> BoxedStrategy<Self::State> {
        let universe = future_universe();
        let mut stored = HashSet::new();
        stored.insert(universe.genesis.block_hash());
        Just(FutureModel {
            universe,
            stored,
            last: FuturePrediction::Noop,
        })
        .boxed()
    }

    fn transitions(_state: &Self::State) -> BoxedStrategy<Self::Transition> {
        prop_oneof![Just(FutureTransition::Good), Just(FutureTransition::Future),].boxed()
    }

    fn apply(mut state: Self::State, transition: &Self::Transition) -> Self::State {
        match transition {
            FutureTransition::Good => {
                if state.stored.contains(&state.universe.good.block_hash()) {
                    state.last = FuturePrediction::Noop;
                } else {
                    state.stored.insert(state.universe.good.block_hash());
                    state.last = FuturePrediction::Connected;
                }
            }
            FutureTransition::Future => {
                state.last = if state.stored.contains(&state.universe.future.block_hash()) {
                    FuturePrediction::Noop
                } else {
                    // HDR-08: the timestamp is more than two hours after `now`.
                    FuturePrediction::TimestampTooNew
                };
            }
        }
        state
    }
}

struct FutureSut {
    core: Core,
}

impl StateMachineTest for FutureMachine {
    type SystemUnderTest = FutureSut;
    type Reference = FutureMachine;

    fn init_test(model: &FutureModel) -> FutureSut {
        let core = mainnet_core(&[(CHECKPOINT_HEIGHT, model.universe.genesis)]);
        FutureSut { core }
    }

    fn apply(sut: FutureSut, model: &FutureModel, transition: FutureTransition) -> FutureSut {
        let header = match transition {
            FutureTransition::Good => model.universe.good,
            FutureTransition::Future => model.universe.future,
        };
        let before = sut.core.state().expect("state");
        let outcome = sut.core.apply(Action::SubmitBitcoinHeaders {
            headers: std::slice::from_ref(&header),
            now: Some(NOW),
        });
        let after = sut.core.state().expect("state");

        match (&model.last, &outcome) {
            (FuturePrediction::Noop, Ok(Outcome::BitcoinSubmitted(result))) => {
                assert_eq!(result.accepted, 0, "model expected a no-op");
                assert_eq!(before, after, "a no-op changed the store");
            }
            (FuturePrediction::Connected, Ok(Outcome::BitcoinSubmitted(result))) => {
                assert_eq!(result.accepted, 1);
                assert_eq!(after.best_fork, Some(0));
            }
            (
                FuturePrediction::TimestampTooNew,
                Err(Error::TimestampTooNew {
                    height,
                    now,
                    got,
                    max_ahead,
                }),
            ) => {
                assert_eq!(*height, CHECKPOINT_HEIGHT + 1);
                assert_eq!(*now, NOW);
                assert_eq!(*got, header.time);
                assert_eq!(*max_ahead, crate::btc::validation::MAX_FUTURE_BLOCK_TIME);
                assert_eq!(before, after, "a future header changed the store");
            }
            (expected, other) => panic!("model expected {expected:?}, core returned {other:?}"),
        }
        sut
    }

    fn check_invariants(sut: &FutureSut, model: &FutureModel) {
        let state = sut.core.state().expect("state");
        let stored: HashSet<BlockHash> = sut
            .core
            .stored_headers()
            .expect("read")
            .map(|header| header.expect("read").hash)
            .collect();
        assert_eq!(stored, model.stored, "the stored header set diverged");
        assert_eq!(state.best_fork, Some(0));
        assert_eq!(state.finalized, Some(CHECKPOINT_HEIGHT));

        let chain = canonical_chain(&sut.core);
        assert_eq!(chain[0].block_hash(), model.universe.genesis.block_hash());
        for pair in chain.windows(2) {
            assert_eq!(pair[1].prev_blockhash, pair[0].block_hash());
        }
        assert_eq!(state.rootstock, RskState::default());
    }
}

// ── the runnable specs ───────────────────────────────────────────────────────

// ── clause boundaries the machines cannot reach ─────────────────────────────

/// E6: on mainnet the requirement gates the mark, not the depth alone. The
/// synthetic chain is deeper than the window but cannot clear the estimated
/// work, so the mark must not move.
#[test]
fn the_finality_requirement_gates_the_mark() {
    let universe = finality_universe();
    // The default configuration keeps the mainnet partition safety on.
    let core = Core::new(
        Config::in_memory(Network::Bitcoin)
            .with_params(mainnet_params())
            .with_checkpoints(&[(CHECKPOINT_HEIGHT, universe.genesis)]),
    )
    .expect("in-memory store");
    let outcome = core
        .apply(Action::SubmitBitcoinHeaders {
            headers: &universe.headers,
            now: None,
        })
        .expect("submit");
    let Outcome::BitcoinSubmitted(result) = outcome else {
        panic!("expected a submitted outcome");
    };
    assert_eq!(result.accepted, FINALITY_BLOCKS);
    assert_eq!(
        result.tip_height,
        CHECKPOINT_HEIGHT + FINALITY_BLOCKS as u64
    );

    let finality = result.finality.expect("the requirement is measurable");
    assert!(
        !finality.clears(),
        "a synthetic mainnet chain must not clear the estimated work"
    );
    assert_eq!(result.finalized, None, "the mark advanced without the work");
    assert_eq!(
        core.state().expect("state").finalized,
        Some(CHECKPOINT_HEIGHT),
        "the mark advanced without the required work"
    );
}

prop_state_machine! {
    #![proptest_config(ProptestConfig {
        cases: 48,
        .. ProptestConfig::default()
    })]

    /// E3 and E6: the fork gate and the mark on a trunk.
    #[test]
    fn finality_and_fork_gate_matches_the_spec(sequential 1..64 => FinalityMachine);
}

prop_state_machine! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        .. ProptestConfig::default()
    })]

    /// HDR-04: on mainnet a child must carry the parent's expected bits.
    #[test]
    fn difficulty_matches_the_spec(sequential 1..8 => DifficultyMachine);
}

prop_state_machine! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        .. ProptestConfig::default()
    })]

    /// HDR-08: a header more than two hours ahead of `now` is rejected.
    #[test]
    fn future_time_matches_the_spec(sequential 1..8 => FutureMachine);
}
