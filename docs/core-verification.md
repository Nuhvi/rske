# Core verification

The reviewable specification of `rsk-core` lives in two paired directories:

- `specs/` — human-readable requirements, written as short Markdown documents;
- `crates/core/specs/` — the executable state-machine tests for those
  requirements.

| Human spec | Executable form |
|---|---|
| [`specs/bitcoin-headers.md`](../specs/bitcoin-headers.md) | [`crates/core/specs/bitcoin_headers.rs`](../crates/core/specs/bitcoin_headers.rs) |
| [`specs/rootstock-checkpoints.md`](../specs/rootstock-checkpoints.md) | [`crates/core/specs/rootstock_checkpoints.rs`](../crates/core/specs/rootstock_checkpoints.rs) |

The Markdown is the source. Requirement IDs (`R1`, `R2`, …) are referenced by
comments in the Rust file, so a reviewer can follow a statement from the
document to the exact check that enforces it.

```bash
cargo test -p rsk-core --lib specs::
PROPTEST_VERBOSE=1 cargo test -p rsk-core --lib specs::
```

## What the specs cover

### Bitcoin headers (`bitcoin-headers.md`)

`bitcoin_headers_match_the_spec` checks header and batch delivery over a fixed
universe containing a 12-block trunk, a competing fork, a fork of a fork, a
bad-PoW header, a stale-timestamp header, duplicate batches and an empty batch:

- R1–R2: atomicity and idempotence. A rejected action leaves the store unchanged.
- C1–C4: parent, linkage, proof of work and median-time-past. The errors are
  `UnknownAncestor`, `LinkBreak`, `BadPow` and `BadTimestamp`.
- C7: a header more than two hours after `now` gives `TimestampTooNew`.
- R3–R4: the exact stored header set and canonical-chain linkage.
- R5–R6: maximum-work fork choice and strict-heavier switching.

`finality_and_fork_gate_matches_the_spec` checks the finality mark and the fork
gate on a 240-block trunk with the production depth, delivered in short runs:

- R7: the mark never regresses, never passes the tip, and never moves a
  finalized block;
- R8: the mark is exactly `tip - depth`, and `finalized` reports each move;
- R9: forks at or below the mark are refused, while lighter forks above it are
  stored and never become best.

`difficulty_matches_the_spec` checks C5 through `Core` with custom regtest
parameters that enforce the retarget rule: a child with the parent's bits is
accepted, a sibling with different bits is `BadDifficulty`. C6, the target-range
and pow-limit check, is covered by the delivery machine and by unit tests that
pin the mainnet pow limit. `the_boundary_retarget_is_enforced` covers the
period-boundary retarget through `Core`, which the machine does not reach.

`future_time_matches_the_spec` checks C7 against the caller clock.

Not yet checked by any machine:

- the clock floor and clockless connects (R10), which only unit tests cover;
- a heavier fork that crosses the mark (the mark-crossing half of R6, and the
  election path it exercises);
- median-time-past with fewer than 11 stored headers, and at a height where an
  ancestor header is missing;
- coinbase and Rootstock actions;
- fork trees deeper than the fixed delivery universe.

A requirement added to `specs/` should come with a matching transition or
invariant in the executable form, or an entry in this list.

### Rootstock checkpoints (`rootstock-checkpoints.md`)

`rsk_checkpoint_policy_matches_the_spec` proposes roots, attempts a checkpoint
and writes the sync tip over a real finalized Bitcoin tail:

- R1–R3: proposal acceptance, refusal atomicity, and exact forest/root sets;
- R4–R6: the strict 2/3 threshold, heaviest-root selection, and GHOST subtree
  work;
- R7–R8: sync-tip recording/resolution and the read-only attempt.

## How the state machine tests work

Every transition is one `Core::apply(Action)` call — the same entry point the
node driver uses. A small reference model predicts the accepted header set or
the reported checkpoint; the test compares that prediction with the real redb
store after every step and checks the universal invariants there.

`proptest-state-machine` generates a sequence of actions from the model,
shrinks a failing sequence to a minimal one, and re-runs it from a fresh SUT.
`core/specs/mod.rs` is wired into the crate as a test-only module:

```rust
#[cfg(test)]
#[path = "../specs/mod.rs"]
mod specs;
```

## Core surfaces used by the specs

The tests drive only public API:

- `Core::apply(Action)` — the single mutation entry point;
- `Core::state()` — the whole-state snapshot used for atomicity and
  deduplication;
- `Core::stored_headers()`, `Core::canonical_headers()`,
  `Core::finalized_checkpoints()`, `Core::coinbase_tags()`,
  `Core::rsk_headers_back()` — iterators over the row collections that can grow
  without bound;
- `Core::in_memory`, `Core::in_memory_with`, `Core::reset`, `Core::reset_rsk`
  — in-memory fixtures and replay. These are ordinary public methods; no
  feature flag is needed.

If a requirement is not in `specs/` and has no executable counterpart, it is
covered — if at all — by an ordinary unit test in the module that owns the
behaviour.
