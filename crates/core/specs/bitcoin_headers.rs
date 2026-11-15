//! Executable specification for `specs/bitcoin-headers.md`.
//!
//! Clause IDs in comments (`O1`, `HDR-01`, `E1`, ...) point at the OpenSpec
//! capability for validation and the machine clauses for the rest. This file is
//! the executable form that is expected to fail when a clause is broken.
//!
//! Four machines, all on mainnet parameters:
//!
//! - `bitcoin_headers_match_the_spec` checks E1–E2, HDR-01–HDR-03, HDR-07 and E3–E5 over a
//!   delivery universe with a trunk, a competing fork, a fork of a fork, a
//!   bad-PoW header, a stale-timestamp header, an out-of-range target header,
//!   duplicates and empty batches.
//! - `finality_and_fork_gate_matches_the_spec` checks E3 and E6 on a long trunk
//!   delivered in runs, at the production depth, including light forks
//!   below/above the mark.
//! - `difficulty_matches_the_spec` checks HDR-04 with the mainnet expected-bits
//!   rule.
//! - `future_time_matches_the_spec` checks HDR-08 against the caller clock.
//!
//! A plain test, `store_identity_matches_the_spec`, checks O1 at open.
//! `the_finality_requirement_gates_the_mark` pins E6. The rule scenarios live in
//! `bitcoin_header_validation.rs`.
//!
//! The synthetic chain seeds a recent mainnet height and relaxes the
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
use bitcoin::consensus::Params;
use bitcoin::hashes::Hash as _;
use proptest::prelude::*;
use proptest_state_machine::{ReferenceStateMachine, StateMachineTest, prop_state_machine};

use super::support::{
    EASY_BITS, NOW, SEED_HEIGHT, SEED_TIME, canonical_chain, delivery_core, mainnet_core,
    mainnet_params, mine, mine_bad, mine_with_bits, work,
};
use crate::{Action, Config, Core, Error, Outcome, RskState};

// ── 1. Bitcoin delivery: forks, duplicates, errors ─────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeaderKind {
    Valid,
    BadPow,
    BadTime,
    BadTarget,
}

#[derive(Clone, Debug)]
pub(super) struct HeaderUniverse {
    pub(super) genesis: BitcoinHeader,
    pub(super) headers: Vec<BitcoinHeader>,
    kinds: Vec<HeaderKind>,
    /// Contiguous batches the peer can deliver: every header alone, every
    /// adjacent pair and triple in a run, plus link-break, bad-PoW, duplicate
    /// and empty batches.
    batches: Vec<Vec<usize>>,
}

pub(super) fn header_universe() -> HeaderUniverse {
    let genesis = mine(BlockHash::all_zeros(), SEED_TIME, 0);
    let g = genesis.block_hash();
    let mut headers = Vec::new();
    let mut kinds = Vec::new();

    // A1..A12, the seed chain. A1 is index 0.
    let a1 = mine(g, SEED_TIME + 1, 0);
    headers.push(a1);
    kinds.push(HeaderKind::Valid);
    let mut prev = a1;
    for i in 2..=12u32 {
        let next = mine(prev.block_hash(), SEED_TIME + i, 0);
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
    let q12 = mine(headers[10].block_hash(), SEED_TIME + 6, 77);
    headers.push(q12);
    kinds.push(HeaderKind::BadTime);

    // X2: a valid hash at a target above the relaxed mainnet proof-of-work
    // limit (index 21).
    let x2 = mine_with_bits(a1.block_hash(), SEED_TIME + 2, 88, 0x2100_ffff);
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

/// What the model expects a batch to do.
#[derive(Clone, Debug, PartialEq, Eq)]
enum HeaderPrediction {
    Noop,
    Connected(usize),
    UnknownAncestor,
    LinkBreak,
    BadPow,
    BadTimestamp,
    BadTarget,
}

#[derive(Clone, Debug)]
struct HeaderModel {
    universe: HeaderUniverse,
    stored: HashSet<BlockHash>,
    last: HeaderPrediction,
}

#[derive(Clone, Debug)]
struct HeaderTransition {
    batch: Vec<usize>,
}

/// The model's reading of `connect_headers`. It mirrors the skip-stored-prefix
/// step, the parent lookup, then link and PoW validation in the same order as
/// the implementation, so it can predict the exact error variant.
///
/// The finality mark is out of reach in this universe (the depth is the
/// production 100 and the deepest header is height 12), so
/// `ForkBelowFinalized` cannot occur here. A machine that models the mark has
/// to add that branch.
fn predict(
    universe: &HeaderUniverse,
    stored: &HashSet<BlockHash>,
    batch: &[usize],
) -> (HeaderPrediction, Vec<BlockHash>) {
    let headers: Vec<BitcoinHeader> = batch.iter().map(|&i| universe.headers[i]).collect();
    let Some(start) = headers
        .iter()
        .position(|header| !stored.contains(&header.block_hash()))
    else {
        return (HeaderPrediction::Noop, Vec::new());
    };

    // HDR-01: the parent of the first new header must be stored.
    let parent_known = if start == 0 {
        stored.contains(&headers[0].prev_blockhash)
    } else {
        true // the skipped prefix is stored
    };
    if !parent_known {
        return (HeaderPrediction::UnknownAncestor, Vec::new());
    }

    let mut prev = if start == 0 {
        headers[0].prev_blockhash
    } else {
        headers[start - 1].block_hash()
    };
    for (offset, header) in headers[start..].iter().enumerate() {
        // E2: the run must link, and the first new header must link to the
        // batch's parent (the previous element when a prefix was skipped).
        if header.prev_blockhash != prev {
            return (HeaderPrediction::LinkBreak, Vec::new());
        }
        // HDR-07: with eleven ancestors available, the timestamp must beat their
        // median. Q12 is positioned so that its parent implies the full window.
        // HDR-02: the target must be in range and at or below the pow limit.
        // HDR-03: the header must satisfy its own target.
        match universe.kinds[batch[start + offset]] {
            HeaderKind::Valid => {}
            HeaderKind::BadPow => return (HeaderPrediction::BadPow, Vec::new()),
            HeaderKind::BadTime => return (HeaderPrediction::BadTimestamp, Vec::new()),
            HeaderKind::BadTarget => return (HeaderPrediction::BadTarget, Vec::new()),
        }
        prev = header.block_hash();
    }

    let accepted = headers[start..].iter().map(|h| h.block_hash()).collect();
    (HeaderPrediction::Connected(headers.len() - start), accepted)
}

struct HeaderMachine;

impl ReferenceStateMachine for HeaderMachine {
    type State = HeaderModel;
    type Transition = HeaderTransition;

    fn init_state() -> BoxedStrategy<Self::State> {
        let universe = header_universe();
        let mut stored = HashSet::new();
        stored.insert(universe.genesis.block_hash());
        Just(HeaderModel {
            universe,
            stored,
            last: HeaderPrediction::Noop,
        })
        .boxed()
    }

    fn transitions(state: &Self::State) -> BoxedStrategy<Self::Transition> {
        // `transitions` must return a `'static` strategy, so move the batch
        // table out of the borrowed reference state.
        let batches = state.universe.batches.clone();
        (0..batches.len())
            .prop_map(move |i| HeaderTransition {
                batch: batches[i].clone(),
            })
            .boxed()
    }

    fn apply(mut state: Self::State, transition: &Self::Transition) -> Self::State {
        let (prediction, accepted) = predict(&state.universe, &state.stored, &transition.batch);
        for hash in accepted {
            state.stored.insert(hash);
        }
        state.last = prediction;
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
        let core = delivery_core(&[(SEED_HEIGHT, model.universe.genesis)]);
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
        let outcome = sut.core.apply(Action::ConnectHeaders {
            headers: &headers,
            now: None,
        });
        let after = sut.core.state().expect("state");

        // E5: a best-fork change must be a strict improvement; equal work never
        // promotes the challenger. The mark-crossing half of E5 is out of
        // reach here because the mark stays at the seed height.
        if let (Some(old), Some(new)) = (before.best_fork, after.best_fork)
            && old != new
        {
            let work_of = |state: &crate::CoreState, id: u64| {
                U256::from_be_slice(
                    &state
                        .forks
                        .iter()
                        .find(|fork| fork.id == id)
                        .expect("best fork is listed")
                        .tip_work,
                )
            };
            assert!(
                work_of(&after, new) > work_of(&before, old),
                "switched to a fork that was not strictly heavier"
            );
        }

        // E1: an error leaves the store untouched, checked below.
        match (&model.last, &outcome) {
            // E2: an all-stored batch is a no-op.
            (HeaderPrediction::Noop, Ok(Outcome::Connected(result))) => {
                assert_eq!(result.connected, 0, "model expected a no-op");
                assert_eq!(before, after, "a no-op changed the store");
            }
            (HeaderPrediction::Connected(expected), Ok(Outcome::Connected(result))) => {
                assert_eq!(result.connected, *expected, "connected count diverged");
                assert_eq!(
                    result.best_fork, after.best_fork,
                    "the outcome's best fork disagrees with the store"
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
        let mark = after.finalized.expect("seeded store has a mark");
        assert!(
            mark >= before.finalized.unwrap_or(0),
            "the finality mark regressed"
        );
        while SEED_HEIGHT + sut.finalized_prefix.len() as u64 <= mark {
            let height = SEED_HEIGHT + sut.finalized_prefix.len() as u64;
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

        // E4: every stored row, blocked forks included.
        let stored: HashSet<BlockHash> = sut
            .core
            .stored_headers()
            .expect("read")
            .map(|header| header.expect("read").hash)
            .collect();
        assert_eq!(stored, model.stored, "the stored header set diverged");

        // S3: the canonical chain is the best fork's chain.
        let best = state.best_fork.expect("seeded store has a best fork");
        let best_fork = state
            .forks
            .iter()
            .find(|fork| fork.id == best)
            .expect("best fork is listed");
        let chain = canonical_chain(&sut.core);
        assert_eq!(
            chain.first().map(|header| header.block_hash()),
            Some(model.universe.genesis.block_hash())
        );
        for pair in chain.windows(2) {
            assert_eq!(
                pair[1].prev_blockhash,
                pair[0].block_hash(),
                "canonical chain is not linked"
            );
        }
        assert_eq!(chain.len() as u64, best_fork.tip_height - SEED_HEIGHT + 1);
        assert_eq!(
            chain
                .last()
                .expect("seed header")
                .block_hash()
                .to_byte_array(),
            best_fork.tip_hash
        );
        assert_eq!(
            sut.core
                .chainwork()
                .expect("read")
                .expect("seeded")
                .to_be_bytes(),
            best_fork.tip_work
        );
        assert_eq!(
            sut.core
                .height_of(&chain.last().expect("seed header").block_hash())
                .expect("read"),
            Some(best_fork.tip_height)
        );

        // E3: no fork can be blocked by the mark here (depth 100, tip at most
        // 12), so the best fork must carry the maximum work. Ties are
        // allowed to stay with the incumbent; `apply` checks that a *switch*
        // only ever goes to a strictly heavier fork.
        let best_work = U256::from_be_slice(&best_fork.tip_work);
        for fork in &state.forks {
            let fork_work = U256::from_be_slice(&fork.tip_work);
            assert!(fork_work <= best_work, "the best fork is not the heaviest");
        }

        // E6: every height at or below the mark still has the block it had.
        let mark = state.finalized.expect("seeded store has a mark");
        assert!(mark <= best_fork.tip_height);
        assert_eq!(sut.finalized_prefix.len() as u64, mark - SEED_HEIGHT + 1);
        for (offset, expected) in sut.finalized_prefix.iter().enumerate() {
            let height = SEED_HEIGHT + offset as u64;
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
    /// header `i` has height `SEED_HEIGHT + i + 2`; it can never outweigh the
    /// trunk when the parent is below the tip.
    forks: Vec<BitcoinHeader>,
}

fn finality_universe() -> FinalityUniverse {
    let genesis = mine(BlockHash::all_zeros(), SEED_TIME, 0);
    let mut headers = Vec::with_capacity(FINALITY_BLOCKS);
    let mut prev = genesis;
    for i in 1..=FINALITY_BLOCKS as u32 {
        prev = mine(prev.block_hash(), SEED_TIME + i, 0);
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
            tip: SEED_HEIGHT,
            mark: SEED_HEIGHT,
            last: FinalityPrediction::Noop,
        })
        .boxed()
    }

    fn transitions(state: &Self::State) -> BoxedStrategy<Self::Transition> {
        let len = state.universe.headers.len();
        let frontier = state.tip.saturating_sub(SEED_HEIGHT) as usize;
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
                let height = SEED_HEIGHT + *index as u64 + 1;
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
                    state.tip = state.tip.max(SEED_HEIGHT + (start + offset) as u64 + 1);
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
                    let forkpoint = SEED_HEIGHT + *index as u64 + 2;
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
        let core = mainnet_core(&[(SEED_HEIGHT, model.universe.genesis)]);
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
            .apply(Action::ConnectHeaders { headers, now: None });
        let after = sut.core.state().expect("state");

        // E6: `ConnectOutcome::finalized` reports exactly the mark movement the
        // model predicted.
        match (&model.last, &outcome) {
            (FinalityPrediction::Noop, Ok(Outcome::Connected(result))) => {
                assert_eq!(result.connected, 0, "model expected a no-op");
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
                Ok(Outcome::Connected(result)),
            ) => {
                assert_eq!(result.connected, *connected);
                assert_eq!(result.tip_height, model.tip);
                assert_eq!(
                    result.finalized,
                    Some(model.mark),
                    "the connect did not report the new mark"
                );
            }
            (
                FinalityPrediction::Connected {
                    connected,
                    advanced: false,
                },
                Ok(Outcome::Connected(result)),
            ) => {
                assert_eq!(result.connected, *connected);
                assert_eq!(result.tip_height, model.tip);
                assert_eq!(result.finalized, None, "the mark advanced unexpectedly");
            }
            (FinalityPrediction::ForkStored, Ok(Outcome::Connected(result))) => {
                assert_eq!(result.connected, 1);
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

        let mark = after.finalized.expect("seeded store has a mark");
        while SEED_HEIGHT + sut.finalized_prefix.len() as u64 <= mark {
            let height = SEED_HEIGHT + sut.finalized_prefix.len() as u64;
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
                .max(SEED_HEIGHT),
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
        let trunk = model.tip - SEED_HEIGHT + 1;
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
            model.mark - SEED_HEIGHT + 1
        );
        assert_eq!(
            sut.finalized_prefix,
            chain[..=(model.mark - SEED_HEIGHT) as usize]
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
    let genesis = mine(BlockHash::all_zeros(), SEED_TIME, 0);
    let good = mine(genesis.block_hash(), SEED_TIME + 1, 0);
    // A valid header at a slightly different target. HDR-04 rejects it for the
    // bits, before HDR-02 or PoW.
    let wrong = mine_with_bits(genesis.block_hash(), SEED_TIME + 1, 1, EASY_BITS - 1);
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
        let core = mainnet_core(&[(SEED_HEIGHT, model.universe.genesis)]);
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
        let outcome = sut.core.apply(Action::ConnectHeaders {
            headers: std::slice::from_ref(&header),
            now: None,
        });
        let after = sut.core.state().expect("state");

        match (&model.last, &outcome) {
            (DifficultyPrediction::Noop, Ok(Outcome::Connected(result))) => {
                assert_eq!(result.connected, 0, "model expected a no-op");
                assert_eq!(before, after, "a no-op changed the store");
            }
            (DifficultyPrediction::Connected, Ok(Outcome::Connected(result))) => {
                assert_eq!(result.connected, 1);
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
                assert_eq!(*height, SEED_HEIGHT + 1);
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
        assert_eq!(state.finalized, Some(SEED_HEIGHT));

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
    let genesis = mine(BlockHash::all_zeros(), SEED_TIME, 0);
    let good = mine(genesis.block_hash(), SEED_TIME + 1, 0);
    // One second beyond the two-hour window in `connect_headers`.
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
        let core = mainnet_core(&[(SEED_HEIGHT, model.universe.genesis)]);
        FutureSut { core }
    }

    fn apply(sut: FutureSut, model: &FutureModel, transition: FutureTransition) -> FutureSut {
        let header = match transition {
            FutureTransition::Good => model.universe.good,
            FutureTransition::Future => model.universe.future,
        };
        let before = sut.core.state().expect("state");
        let outcome = sut.core.apply(Action::ConnectHeaders {
            headers: std::slice::from_ref(&header),
            now: Some(NOW),
        });
        let after = sut.core.state().expect("state");

        match (&model.last, &outcome) {
            (FuturePrediction::Noop, Ok(Outcome::Connected(result))) => {
                assert_eq!(result.connected, 0, "model expected a no-op");
                assert_eq!(before, after, "a no-op changed the store");
            }
            (FuturePrediction::Connected, Ok(Outcome::Connected(result))) => {
                assert_eq!(result.connected, 1);
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
                assert_eq!(*height, SEED_HEIGHT + 1);
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
        assert_eq!(state.finalized, Some(SEED_HEIGHT));

        let chain = canonical_chain(&sut.core);
        assert_eq!(chain[0].block_hash(), model.universe.genesis.block_hash());
        for pair in chain.windows(2) {
            assert_eq!(pair[1].prev_blockhash, pair[0].block_hash());
        }
        assert_eq!(state.rootstock, RskState::default());
    }
}

// ── the runnable specs ───────────────────────────────────────────────────────

/// O1: an open with a stored chain refuses another network or another seed,
/// and adds no checkpoint.
#[test]
fn store_identity_matches_the_spec() {
    let genesis = mine(BlockHash::all_zeros(), SEED_TIME, 0);
    let other = mine(BlockHash::all_zeros(), SEED_TIME + 1, 0);
    let core = delivery_core(&[(SEED_HEIGHT, genesis)]);

    // A different network.
    let Core { store, config } = core;
    let core = Core {
        store,
        config: config.with_params(Params::new(Network::Regtest)),
    };
    assert!(matches!(
        core.ensure_anchor(),
        Err(Error::NetworkMismatch { .. })
    ));

    // A different header at the same checkpoint height.
    let Core { store, config } = core;
    let core = Core {
        store,
        config: config
            .with_params(Params::new(Network::Bitcoin))
            .with_anchor(&[(SEED_HEIGHT, other)]),
    };
    assert!(matches!(
        core.ensure_anchor(),
        Err(Error::AnchorMismatch { height, .. }) if height == SEED_HEIGHT
    ));
}

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
            .with_anchor(&[(SEED_HEIGHT, universe.genesis)]),
    )
    .expect("in-memory store");
    let outcome = core
        .apply(Action::ConnectHeaders {
            headers: &universe.headers,
            now: None,
        })
        .expect("connect");
    let Outcome::Connected(result) = outcome else {
        panic!("expected a connected outcome");
    };
    assert_eq!(result.connected, FINALITY_BLOCKS);
    assert_eq!(result.tip_height, SEED_HEIGHT + FINALITY_BLOCKS as u64);

    let finality = result.finality.expect("the requirement is measurable");
    assert!(
        !finality.clears(),
        "a synthetic mainnet chain must not clear the estimated work"
    );
    assert_eq!(result.finalized, None, "the mark advanced without the work");
    assert_eq!(
        core.state().expect("state").finalized,
        Some(SEED_HEIGHT),
        "the mark advanced without the required work"
    );
}

prop_state_machine! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        .. ProptestConfig::default()
    })]

    /// E1–E2, HDR-01–HDR-03, HDR-07, E3–E5: acceptance, storage and fork choice on the delivery universe.
    #[test]
    fn bitcoin_headers_match_the_spec(sequential 1..32 => HeaderMachine);
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
