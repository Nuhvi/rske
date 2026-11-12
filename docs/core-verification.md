# Core verification

The reviewable specification of `rsk-core` lives in two paired directories:

- `specs/` — human-readable requirements, written as short Markdown documents;
- `crates/core/specs/` — the executable state-machine tests for those
  requirements.

| Human spec | Executable form |
|---|---|
| [`specs/bitcoin-headers.md`](../specs/bitcoin-headers.md) | [`crates/core/specs/bitcoin_headers.rs`](../crates/core/specs/bitcoin_headers.rs) |
| [`specs/rootstock-checkpoints.md`](../specs/rootstock-checkpoints.md) | [`crates/core/specs/rootstock_checkpoints.rs`](../crates/core/specs/rootstock_checkpoints.rs) |

The Markdown is the source. Clause IDs (`O1`, `V1`, `E1`, …) are referenced by
comments in the Rust file, so a reviewer can follow a statement from the
document to the exact check that enforces it.

```bash
cargo test -p rsk-core --lib specs::
PROPTEST_VERBOSE=1 cargo test -p rsk-core --lib specs::
```

## What the specs cover

### Bitcoin headers (`bitcoin-headers.md`)

All four machines run on mainnet parameters. The synthetic chain seeds a recent
mainnet difficulty-period start, relaxes the proof-of-work limit so mining is
cheap, and disables the mainnet estimated-work finality requirement, which a
synthetic chain cannot clear. This keeps the mainnet network, difficulty and
finality code paths while the headers stay cheap to mine.

`bitcoin_headers_match_the_spec` checks header and batch delivery over a fixed
universe containing a 12-block trunk, a competing fork, a fork of a fork, a
bad-PoW header, a stale-timestamp header, an out-of-range target header,
duplicate batches and an empty batch:

- E1–E2: atomicity and idempotence. A rejected action leaves the store unchanged.
- V1–V4: parent, linkage, proof of work and median-time-past. The errors are
  `UnknownAncestor`, `LinkBreak`, `BadPow` and `BadTimestamp`.
- V7: a header more than two hours after `now` gives `TimestampTooNew`.
- E3–E5: fork creation, the exact stored header set and fork choice. The
  canonical chain is the best fork's chain (S3).

This machine turns V5 off with a test-only switch, because the expected-bits
rule would reject an out-of-range target before V6 can see it. V5 has its own
machine.

`finality_and_fork_gate_matches_the_spec` checks the finality mark and the fork
gate on a 240-block trunk with the production depth, delivered in short runs:

- E3 and E6: forks at or below the mark are refused, the mark never regresses
  or passes the tip, and `finalized` reports each move.

`difficulty_matches_the_spec` checks V5 through `Core` with mainnet parameters
and a relaxed proof-of-work limit: a child with the parent's bits is accepted, a
sibling with different bits is `BadDifficulty`. V6, the target-range and
pow-limit check, is covered by the delivery machine and by unit tests that pin
the mainnet pow limit. `the_boundary_retarget_is_enforced` covers the
period-boundary retarget through `Core`, which the machine does not reach.
Testnet's minimum-difficulty gap and walk-back, testnet4's BIP94 retarget and
regtest's no-retarget rule are unit tests in `btc::validation` and `btc`.

`future_time_matches_the_spec` checks V7 against the caller clock.

`store_identity_matches_the_spec` covers O1 (store identity) without a
machine: an open with a stored chain refuses another network or another
seed.

Not yet checked by any machine:

- the clock floor and clockless connects (E7), including the
  median-time-past chain clock and its sparse-window fallback, which only unit
  tests cover;
- a heavier fork that crosses the mark (the mark-crossing half of E5, and the
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
- `Core::new(Config)` — opens a store from one configuration value. It seeds a
  fresh store from the compiled-in anchor and refuses an existing store whose
  network or checkpoints disagree, so no caller can switch chains. `Config::in_memory`/`Config::open` select the backend, and
  `Config::with_params`/`Config::without_partition_safety` override the network
  defaults (`Core::in_memory`/`Core::open` are the default-option shortcuts).
  `Config::with_anchor` and `Config::without_difficulty_check` are test-only and
  crate-private;
- `Core::reset`, `Core::reset_rsk` — replay from a clean fixture. `Core::reset`
  re-anchors from the configuration; `Core::reset_rsk` drops only the Rootstock
  tables. These are ordinary public methods; no feature flag is needed.

If a requirement is not in `specs/` and has no executable counterpart, it is
covered — if at all — by an ordinary unit test in the module that owns the
behaviour.
