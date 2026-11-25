# Verification guide

This document is the verification playbook for the workspace. It states what to
test, how to test it, and how to read the result. Follow it for every change.

The capability specifications in `openspec/specs/` are the source. The
repository-root `specs/` directory holds legacy documents. Do not edit them.

## The gate

Run these commands before you finish a change.

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

Neither tool is installed in this workspace. Install it before you use it.

**Kani** proves a property for all inputs. Use it for the pure arithmetic that
a random test samples poorly:

- compact-target decode, encode and range check in `btc/validation.rs`;
- the retarget timespan clamp and the 256-bit multiply;
- the estimated-work finality law in `btc/finality.rs`;
- the `3 * work > 2 * tail_work` comparison in `rsk/score.rs`.

The harness shape:

```rust
#[cfg(kani)]
#[kani::proof]
fn compact_target_decode_never_panics() {
    let bits: u32 = kani::any();
    let header = header_with_bits(bits);
    let _ = header.target();
}
```

Run it with `cargo kani`.

**Fuzzing** finds a crash in a byte parser. Use `cargo-fuzz` or `bolero` on
the untrusted input, not on the typed core:

- `btc-p2p` message codec and bloom filter;
- `rsk-consensus` RLP decoding and header decode;
- the coinbase `RSKBLOCK:` tag scan;
- the merged-mining proof check.

## Current coverage

### `bitcoin-header-validation` (HDR-00 to HDR-08)

`crates/core/specs/bitcoin_header_validation.rs` holds one scenario per
requirement: `hdr_00_supported_networks` to `hdr_08_future_time`.
`crates/core/specs/bitcoin_headers.rs` holds the HDR-04 and HDR-08 machines.

### `bitcoin-header-chain` (BTC-00 to BTC-08)

`crates/core/specs/bitcoin_header_chain.rs` holds the scenarios
`btc_00_trusted_checkpoints` to `btc_08_outcome`, and the state machine
`bitcoin_header_chain_matches_the_spec`.

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

## Known gaps

A requirement added to a capability specification needs a matching scenario,
a state machine transition, or an entry in this list.

- The clock floor and clockless connects. The median-time-past chain clock and
  its sparse-window fallback have unit tests only.
- A heavier fork that crosses the finality mark. The admittance rule has no
  state machine.
- Median-time-past with fewer than 11 stored headers, and at a height where an
  ancestor header is missing.
- The coinbase actions and the Rootstock actions inside the chain machine. They
  are tested by their own scenarios only.
- A fork tree deeper than the fixed delivery universe.
- One machine that interleaves Bitcoin and Rootstock actions. The Bitcoin mark
  cannot move inside the Rootstock fixture, so the cross-capability seam is not
  covered.
- The node driver in `crates/rsk` has no state machine. `sync`, `ibd`,
  `coinbase` and `peers` are covered by unit tests and by mutation testing
  only.

## Anti-patterns

- Do not test a log string.
- Do not assert an internal branch that `spec.md` does not state.
- Do not disable the difficulty check in a capability scenario.
- Do not mutate the whole workspace.
- Do not add a test only to kill a mutant. Decide the class first.
- Do not write a reference model that copies the implementation.
