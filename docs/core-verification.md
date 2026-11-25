# Verification guide

This document is the verification playbook for the workspace. It states what to
test, how to test it, and how to read the result. Follow it for every change.

The capability specifications in `openspec/specs/` are the source. The
repository-root `specs/` directory holds legacy documents. Do not edit them.

## The gate

Run these commands before you finish a change.

The whole gate is one command:

```bash
scripts/verify.sh          # format, lints, tests, specification structure
scripts/verify.sh --full   # adds the Kani proofs
```

The table below lists the individual steps.

CI runs the workspace suite with `cargo nextest`, and the documentation tests
with `cargo test --doc`. The fast workflow lives in
`.github/workflows/ci.yml`; the proofs, mutation and fuzz jobs live in
`.github/workflows/verify.yml`.

| Command | Scope |
|---|---|
| `cargo test -p rsk-core --lib specs::` | every capability scenario and every state machine |
| `cargo test -p rsk-core` | the `rsk-core` unit tests too |
| `cargo test -p rsk` | the node driver |
| `cargo check --workspace` | the whole workspace |
| `openspec validate --all --strict` | the specification structure |
| `cargo mutants -p rsk-core -f FILE -- --lib` | one source file, on demand |

Add `PROPTEST_VERBOSE=1` to a state machine command to see the generated
sequences. Run one capability alone:

```bash
cargo test -p rsk-core --lib specs::bitcoin_header_validation
cargo test -p rsk-core --lib specs::bitcoin_header_chain
```

## The five layers

Use the lowest layer that can prove the property. A change adds tests at the
layer of the rule it touches, not above it.

| Layer | Tool | Proves |
|---|---|---|
| 1. Scenario | `#[test]` and `Core::apply` | one requirement, at its boundaries |
| 2. State machine | `proptest-state-machine` | sequences, against a reference model |
| 3. Exhaustive search | a breadth-first driver | every reachable state in a small space |
| 4. Mutation | `cargo-mutants` | that the tests constrain the code |
| 5. Symbolic | Kani, fuzzing | all inputs, or untrusted bytes |

## Layer 1: capability scenarios

One `#[test]` per requirement. The test name carries the requirement ID, so a
rename is a visible break. Examples: `btc_07_best_fork` and
`hdr_05_minimum_difficulty_networks`.

Every scenario calls `Core::apply(Action)`. Do not call a private helper, and
do not disable a production validation flag. The only permitted test switches
are `Config::with_checkpoints`, `Config::without_partition_safety` and a
relaxed proof-of-work limit.

Rules:

1. **Test both sides of every boundary.** For every `<`, `<=`, `>`, `>=`,
   window size `N`, clamp factor and ratio, write one case at the boundary and
   one case one step past it. The `openspec/config.yaml` rule states the same.
2. **Change one field from a valid baseline.** Start from a header the core
   accepts. Change one field. Re-mine when the change alters the hash.
3. **Assert the exact `Error` variant and its fields.** Do not assert that "an
   error happened" when the spec names the error.
4. **Assert atomicity.** Read `Core::state()` before and after a refusal, and
   compare them. A refused action must change nothing.
5. **Do not shadow a rule.** If an earlier rule necessarily fails first, repair
   the fixture. Do not assert a later rule through a fixture the earlier rule
   already rejected.
6. **Do not pin log text.** A log line is not behaviour. If a function only
   formats output, exclude it from mutation testing instead of testing it.

## Layer 2: state machine tests

`crates/core/specs/` holds one file per capability. Each file owns its model,
its transitions and its invariants. `support.rs` holds the shared fixtures.

The model rules:

- Derive the model from the capability specification and the public `Core`
  types. Do not copy an implementation branch into the model. A model that
  mirrors the code proves only that the code equals itself.
- Predict the full `CoreState`, the `StoredHeader` rows and the owned
  `Outcome` or `Error` fields. A hash set of headers is not enough.
- Compare the store after every step, not only at the end.
- Shrink on failure. `proptest-state-machine` re-runs a minimal sequence from a
  fresh system under test.

Check the transition topology. The current machines use a fixed fixture, so
the generated sequence varies the order and the batch cut, not the graph.
Extend the fixture when a rule needs it. Cover:

- forks with different per-block work, so a shorter but heavier fork exists;
- forks above and below the moving finality mark, so the admittance rule runs;
- caller clock steps (`now: Some` and `now: None`), so the high-water mark
  moves;
- a child proposed before its parent, for the Rootstock forest;
- batch cuts at the first new header, at the fork point and at a stored
  suffix.

The regression file is `crates/core/specs/<machine>.txt`. Keep it in version
control.

The model must not call the crate helper that the system under test uses. A
model that shares the helper agrees with a broken helper by construction. For
example, the chain model computes a header's work from the target itself, not
through `crate::header_work`.

A mutation run replays the whole suite for every mutant. Lower the sample for
speed, then confirm each survivor at the full count: a rare path, such as an
equal-work tie, can need all 64 cases.

```bash
RSK_PROPTEST_CASES=16 cargo mutants -p rsk-core -f FILE -- --lib
```

## Layer 3: exhaustive search

Random walks sample the state space. When the space is small, search it
completely. Use the `Hash` derive that `CoreState` already has.

The pattern:

```rust
use std::collections::{HashSet, VecDeque};

use bitcoin::Network;
use rsk_core::{Action, Core, CoreState};

const DEPTH: usize = 6;

fn explore() {
    let mut queue: VecDeque<Vec<Step>> = VecDeque::from([Vec::new()]);
    let mut seen: HashSet<CoreState> = HashSet::new();

    while let Some(path) = queue.pop_front() {
        // Replay the path on a fresh in-memory store and on the model.
        let core = Core::in_memory(Network::Bitcoin).expect("open");
        let mut model = Model::init();
        for step in &path {
            let expected = model.step(*step);
            let actual = apply(&core, *step);
            assert_eq!(expected, actual, "diverged at {path:?} then {step:?}");
        }

        if !seen.insert(core.state().expect("state")) {
            continue;
        }
        if path.len() == DEPTH {
            continue;
        }
        for step in model.steps() {
            let mut next = path.clone();
            next.push(step);
            queue.push_back(next);
        }
    }
}
```

`apply` maps one `Step` to one `Action` and calls `Core::apply`. Replay from a
fresh store costs one open and one `path.len()` steps per node. Use this layer
for a universe below about 10,000 states: fork choice, the 2/3 threshold, the
sync tip.

Why not TLA+? The Rust reference model already is the transition relation, and
it runs against the real store. A separate `.tla` file would model the model,
not the code, and it cannot see a `redb` row, a 256-bit overflow or an
incorrect height by one. Keep TLA+ for a new concurrent or distributed
protocol, not for this deterministic core.

## Layer 4: mutation testing

`cargo-mutants` changes the code and runs the tests. A mutant that survives
shows code no test constrains.

Run one file at a time. Do not mutate the whole workspace; it takes hours and
most survivors are untestable I/O.

```bash
cargo mutants -p rsk-core -f crates/core/src/btc/validation.rs -- --lib
```

For a change, run only the touched lines. CI uses the same commands:

```bash
git diff origin/main..HEAD > pr.diff
cargo mutants --in-diff pr.diff -p rsk-core -- --lib
```

Preview without running:

```bash
cargo mutants --list -p rsk-core -f crates/core/src/btc/finality.rs
```

Read the result:

- `mutants.out/missed.txt` — the survivors, one line each;
- `mutants.out/diff/<file>_line_<n>.diff` — the change that survived;
- `mutants.out/log/<file>_line_<n>.log` — the test output for that mutant;
- `cargo mutants --iterate` — skip the mutants an earlier run caught.

Every run replaces `mutants.out/`. Move a run you want to keep before you start
the next one. Both paths are ignored by Git.

Always pass `--lib`, so the unit tests in the same crate run. A narrower
filter such as `--lib specs::bitcoin_header_chain` is a fast loop, but it can
hide a survivor that only a unit test kills. Confirm a survivor with `--lib`
before you record it.

### Triage a missed mutant

Decide the class first. Do not write a test to make a survivor disappear.

| Class | Test | Action |
|---|---|---|
| Spec violation | The mutant breaks a clause in `spec.md` | Add or tighten the scenario. |
| Unspecified behaviour | The mutant changes behaviour `spec.md` does not state | Stop. Update the specification first, in its own change. |
| Equivalent mutant | No input can reach the change, or an earlier guard already blocks it | Delete the redundant branch, or exclude it in the config with a reason. |

An exclusion needs a stated reason. Put it in `.cargo/mutants.toml`, next to
the pattern. Do not exclude a function that computes a value.

## Layer 5: Kani and fuzzing

**Kani** proves a property for all inputs. Use it for the pure arithmetic that
a random test samples poorly:

- compact-target decode, encode and range check in `btc/validation.rs`;
- the retarget timespan clamp and the 256-bit multiply;
- the estimated-work finality law in `btc/finality.rs`;
- the CPV sample reach in `rsk/score.rs`.

A proof lives next to the function it proves, in a `#[cfg(kani)] mod proofs`
block in the same file: `btc/validation.rs`, `btc/finality.rs` and
`rsk/score.rs`. A block in the same module reaches the private helpers that a
sibling module cannot. Run one:

```bash
cargo kani --harness a_mainnet_header_inside_a_period_keeps_the_parent_bits
```

Run one proof at a time. A proof that reasons about a symbolic sort or about
floating point is slow in CBMC. Keep a harness to integer arithmetic and
concrete network parameters.

**Fuzzing** finds a crash in a byte parser. Two layers cover the untrusted
bytes:

- `crates/core/tests/robustness.rs` runs under `cargo test` with the `bolero`
  proptest engine. It is the fast check on every commit.
- `fuzz/` holds the equivalent `cargo-fuzz` targets. Run one:

```bash
cargo fuzz run checkpoints_text
```

The `cargo-fuzz` targets need a C++ toolchain for `libfuzzer-sys`. A host
without one keeps the bolero layer and skips this one.

The surfaces:

- the trusted-checkpoint text parser (`checkpoints_text`);
- RSK header RLP decoding (`rsk_header_rlp`);
- Bitcoin P2P frame decoding (`btc_message`).

## Current coverage

### `bitcoin-header-validation` (HDR-00 to HDR-08)

`crates/core/specs/bitcoin_header_validation.rs` holds one scenario per
requirement: `hdr_00_supported_networks` to `hdr_08_future_time`.
`crates/core/specs/bitcoin_headers.rs` holds the HDR-04 and HDR-08 machines.

### `bitcoin-header-chain` (BTC-00 to BTC-08)

`crates/core/specs/bitcoin_header_chain.rs` holds the scenarios
`btc_00_trusted_checkpoints` to `btc_08_outcome`, and the state machine
`bitcoin_header_chain_matches_the_spec`.

The machine delivers a 24-header universe: a trunk, a competing fork, a fork
of a fork, a bad-PoW header, a stale-timestamp header, an out-of-range target,
a wrong-bits header, and duplicate and empty batches. It submits at two caller
clocks and checks the stored clock high-water mark. After every step it
compares the full fork tree, every stored header row, the canonical chain and
the outcome against the model. The model computes a header's work from the
header itself, not through the crate helper.

### Untrusted bytes

`crates/core/tests/robustness.rs` feeds arbitrary bytes to the trusted-
checkpoint text parser, RSK header RLP decoding and Bitcoin header decoding.
It runs under `cargo test`. `fuzz/` holds the coverage-guided equivalents.

### Finality and the Rootstock policy

- `finality_and_fork_gate_matches_the_spec` — the mark and the fork gate on a
  trunk of 240 blocks, delivered in runs.
- `the_finality_requirement_gates_the_mark` — mainnet estimated work does not
  clear, so the mark stays.
- `rsk_checkpoint_policy_matches_the_spec` — proposals, the checkpoint attempt
  and the sync tip over a real finalized Bitcoin tail.

### Shared fixture

`crates/core/specs/support.rs` holds `mine`, `mine_with_bits`, `mine_bad`, the
parameter builders, `mainnet_core`, `header_universe` and the submit helpers.
`crates/core/specs/mod.rs` is wired into the crate at `crates/core/src/lib.rs`
as a test-only module.

## Audit record

### 2026-11-25 — `bitcoin_header_chain`

Scope: the submit path and the finality rule in `crates/core/src/btc.rs`
(lines 623 to 1010), against `cargo test -p rsk-core --lib
specs::bitcoin_header_chain` at `RSK_PROPTEST_CASES=8`.

| Outcome | Count |
|---|---|
| Caught | 42 |
| Missed | 21 |
| Unviable | 8 |
| Timeout (a kill: the test hangs) | 1 |

The caught set includes every mutant on the stored-prefix skip, the parent
lookup, the all-stored branch and the parent-work arithmetic. The suite is not
vacuous: it constrains the rules a reviewer worries about most.

The survivors were re-run against the full library suite at 64 cases.

| Class | Mutants | Decision |
|---|---|---|
| Killed by another suite | 10 | none; the first run sampled too narrowly |
| Equivalent | 8 | keep, or delete the dead branch |
| Real gap | 4 | the coinbase prune needs a test |

The equivalent group is `754:17` (the `&&` whose right side is unused when the
minimum-difficulty rule is off), six rewrites of `tip_height` in the
`prev_best: None` branch that initialization makes unreachable, and `975:34`
(where `>=` writes the same high-water mark). The gap is `863:17` and the
three `873` rewrites: the coinbase prune on a best-fork switch, and the delete
below the newest finalized boundary. Both run with no stored coinbase, so no
test can see them.

A negative control checks the oracle. Change the model's tie rule from `>` to
`>=`, and the machine fails at 64 cases. It passes at 8 cases, which is why a
mutation run confirms every survivor at the full sample.

## Known gaps

A requirement added to a capability specification needs a matching scenario,
a state machine transition, or an entry in this list.

- The clockless connect path (`now: None`) inside the chain machine. The
  machine submits at two explicit clocks; the clock floor and the chain clock
  have unit tests only.
- Variable per-block work inside the chain machine. Every valid header there
  carries the same `bits`, so fork choice by work equals fork choice by length.
  The `btc_07` scenario covers a shorter but heavier fork on testnet.
- A heavier fork that crosses the finality mark and would win. The finality
  machine stores light forks only, so the admittance rule that refuses the
  switch is not exercised. The `crosses_mark` comparison itself is killed by
  the suite.
- Median-time-past with fewer than 11 stored headers, and at a height where an
  ancestor header is missing.
- The coinbase actions and the Rootstock actions inside the chain machine. They
  are tested by their own scenarios only. The coinbase prune on a best-fork
  switch has no test.
- A fork tree deeper than the fixed delivery universe.
- One machine that interleaves Bitcoin and Rootstock actions. The Bitcoin mark
  cannot move inside the Rootstock fixture, so the cross-capability seam is not
  covered.
- The node driver in `crates/rsk` has no state machine. `sync`, `ibd`,
  `coinbase` and `peers` are covered by unit tests and by mutation testing
  only.
- The `prev_best_fork` is `Option` but always present after initialization, so
  the submit path's `None` branch is dead. A mutant there survives as an
  equivalent.

## Anti-patterns

- Do not test a log string.
- Do not assert an internal branch that `spec.md` does not state.
- Do not disable the difficulty check in a capability scenario.
- Do not mutate the whole workspace.
- Do not add a test only to kill a mutant. Decide the class first.
- Do not write a reference model that copies the implementation.
