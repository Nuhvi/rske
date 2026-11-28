# Design

## Context

See `proposal.md` - Why. The current state that shapes the approach:

- `crates/core/src/btc/finality.rs` holds the mark rule: `COINBASE_MATURITY`,
  the price-law constants, `estimated_work`, `partition_safety_required` and
  `FinalityStatus`.
- `crates/core/src/btc.rs` holds `finalize_in_txn`, `measure_finality`,
  `effective_clock` and `chain_time_at`. The mark starts at the last embedded
  checkpoint. It advances only while `best_fork == Some(0)` (`btc.rs:1086`).
  It moves to `tip - 100` when the work above the mark clears the bar
  (`btc.rs:993-1007`). The advance deletes coinbases below the new finalized
  checkpoint (`btc.rs:873-879`).
- `crates/core/src/store/btc.rs` stores the mark as `META_FINALIZED`, a `u64`
  height. `finalized_checkpoints` reads the best fork (`btc.rs:388-401`).
- `crates/core/src/error.rs` defines `ForkBelowFinalized`; no code uses it.
- `crates/core/src/core.rs` exposes `Config::partition_safety`, a public bool
  that weakens finality.
- `crates/rsk/src/sync.rs` asserts that fork 0 is best after a sync. A reorg
  above the checkpoint is legitimate, so this assertion is wrong.
- The embedded spine holds 483 period starts, to height 971,712 (October 2026).
- Constraints: every state change runs in one `Core::apply` transaction. The
  core has no clock, no network and no I/O. `Core::state()` returns the full
  stored state. Requirement IDs are append-only.

## Goals / Non-Goals

**Goals:**

- One write path for the mark. The automatic policy and the manual caller use
  it.
- Fork 0 loses every special role. The automatic rule runs on the best fork.
- The mark is a block identity. The consumers read the mark's fork and hash.
- The estimate is a policy value that a test can inject.
- A conflict is an error. The caller halts and reacts. The core does not
  recover automatically.
- The mark move prunes nothing.

**Non-Goals:**

- Binohash, watchtowers, certificate types and a public policy surface.
- Pruning headers or coinbases below the mark inside this capability.
- A consensus change. The mark is local.
- Changing the per-header rules in `bitcoin-header-validation`.
- A runtime checkpoint ladder.

## Decisions

### D1. The mark is a height plus a hash, stored in two keys

The mark is `(height, hash)`. The header time comes from the stored header at
the mark.

Storage stays additive: `META_FINALIZED` keeps the height and its `u64` type,
and a new key, `META_FINALIZED_HASH`, holds the hash. An older binary still
reads the height and ignores the hash. A newer binary on an old store derives
the hash from the best fork at the height; if the height is pruned, it resets
the mark to the spine.

Alternative: one tagged value. Rejected because a rollback to an older binary
would fail to decode it.

Alternative: a fork id plus a height. Rejected because an open can prune or
reuse fork ids, while the hash is durable.

### D2. One writer: `advance_final_block(Option<u64>)`

The method sets the mark to the best tip minus `n`:

- `Some(n)` is the block `n` below the best tip. `Some(0)` is the tip.
- `None` is `COINBASE_MATURITY`, which is 100.
- It requires the block to exist at that height on the best fork.
- It returns the previous mark.
- It does not check the bar, the clock, continuity or the competing forks.
- It does not prune.

The spec says "the block `n` below the best tip". The code parameter is
`depth`, not `confirmations`, because Bitcoin's "100 confirmations" means 99
blocks below the tip. The off-by-one would be a trap.

Alternative: a separate `set_mark(height, hash)` and `advance_to_depth(n)`.
Rejected: two write paths, two retention paths, two outcome shapes.

### D3. The automatic policy calls D2

`finalize_in_txn` becomes the automatic caller:

1. Read `best_fork` and its tip. Stop when the store has no best fork.
2. Read the mark. Stop when the store has no mark.
3. Check continuity: the block at the mark height on the best fork must be the
   mark block. Otherwise return `FinalityConflict`.
4. Compute the anchor work and the bar (D5).
5. Compare the work above the mark with the bar. Stop when it is short.
6. Compute the candidate `tip - 100`.
7. Cap the candidate at the common prefix with every competing fork (D8).
8. Stop when the candidate is at or below the mark.
9. Call `advance_final_block(Some(tip - candidate))`.

The `best_fork == Some(0)` gate is deleted. Fork 0 has no role in the chain,
the mark or the conflict check. The continuity check in step 3 replaces it: a
normal reorg above the mark keeps continuity, so the mark keeps advancing.

Alternative: replace fork 0's rows when a heavier fork wins. Rejected: it
copies rows on every reorg and retargets storage ids. There is no need.

### D4. The mark move prunes nothing

The automatic advance no longer calls `delete_coinbases_below`. The mark is a
decision; it is not a retention trigger. The separate `prune_coinbases` action
keeps its own policy and reads the retention floor from the mark.

The node driver may call `prune_coinbases` after an advance, but the core does
not. This differs from today's behavior (`btc.rs:873-879`) and is intentional.

### D5. The bar and the estimate

The bar is:

```text
need_blocks = max(COINBASE_MATURITY, est_blocks)
need        = need_blocks * w
```

`w` is the work of the block at the mark, so `need_blocks` is a count of
blocks at the mark's difficulty. `est_blocks` is the estimate, a `u64`.

The estimate is a policy value, not a stored one:

```text
trait WorkEstimate {
    fn blocks(anchor_time, now, anchor_work) -> u64;
}
```

The production value is `PowerLaw`. It holds the genesis time and the five
constants. The estimate returns blocks; the product with `work_at(mark)` is
the work comparison. A genesis anchor or zero anchor work fails closed at
`U256::MAX`.

The test value is `Blocks(n)` or a `PowerLaw` with changed constants. The
injection is crate-private and refused on non-mainnet. No public bool remains.

### D6. The estimate, explained

The estimate answers one question: how much work should the honest network
have done between the anchor's time and the caller's clock? The node cannot
measure the honest network. It uses the Bitcoin price power law as a
reference.

Four conversions:

```text
price(days)   = 10^(-17.432 + 5.834 * log10(days))            USD/BTC
difficulty    = 54_621_616_002 * (price / 455.910_174)^(1/0.673_473)
hashrate      = difficulty * 2^32 / 600                       hashes/s
work(t0, now) = hashrate(t0) * a0 / (p + 1) * ((1 + T/a0)^(p + 1) - 1)
```

`p = 5.834 / 0.673_473 = 8.6626`. `a0` is the chain's age at the anchor, in
seconds: `t0 - genesis`. `T` is the window, in seconds: `now - t0`.

Read it in four steps:

1. Price at the anchor, from the chain's age in days.
2. Price to difficulty, through the frozen dbtc relation.
3. Difficulty to a hashrate. One block needs `difficulty * 2^32` hashes on
   average, and the target spacing is 600 seconds.
4. Sum the hashrate over the window. The power law makes the hashrate grow
   with the chain's age, so the sum has the closed form above.

The result is in hashes. Divide by `w0`, the work of one anchor block, to get
blocks at the anchor's difficulty:

```text
est_blocks = work / w0
           = r0 * (a0 / (600 * (p + 1))) * ((1 + T/a0)^(p + 1) - 1)
r0         = H_est(t0) / H_actual(t0)
```

`r0` is the ratio of the price-law hashrate to the real hashrate at the
anchor. It is the only place the estimate's level enters. For 2016 and later
anchors the bottom law sits below the real hashrate, so `r0 < 1`; in 2026 it
is about 0.4 to 0.65.

For a short window (`T << a0`), the bracket is about `(p + 1) * T / a0`, so:

```text
est_blocks ~= r0 * T / 600
```

That is the clearest reading: the honest block count in the window,
multiplied by `r0`. For a 100-block window `T/600 = 100`; with `r0 = 0.6` the
estimate is 60, so the 100-block floor binds.

Worked case: the shipped spine from October 2026, a clock in late November.
`T` is about 42 days, `a0` is about 17.8 years, `r0` is about 0.61. The
estimate is about 3,800 blocks. The honest chain produced about 6,100 blocks in
that window. The bar is 3,800 blocks. After the first advance the anchor is
`tip - 100`, so `T` is about 17 hours, the estimate is about 60 blocks, and the
100-block floor binds.

Why the bottom law: it sits below the realised hashrate for 2015 and later, so
the honest chain clears the bar. A fork must pay at least the bar to pass.
Over-estimating only delays finality. Under-estimating weakens the bar, which
is why the bottom variant is the security case and the median variant is not.

The estimate is not the chain's own work. That is `work(tip) - work(mark)`. It
is not a forecast. It prices the window up to `now`. It is not a hashrate
oracle. It is a trend, fitted in sample on history through 2025.

The bar's guarantee is a cost, not a proof. With `Phi = bar / honest_work`, the
honest chain clears while `Phi <= 1`. A fork with network share `s` holds
`s / (1 - s)` of the honest work, so it passes when `s >= Phi / (1 + Phi)`.
The sampled 2016+ minimum is `Phi = 0.0546` at 2018.53, so 5.18% is the largest
share kept out at every sampled anchor. The current 2026 anchor gives
`Phi ~= 0.62`, so the line is about 38%. The 5% is the worst in-sample floor,
not the current value.

Edge cases: `now <= anchor_time` gives zero. A genesis anchor fails closed.
Zero anchor work fails closed. A huge window saturates at `u64::MAX` blocks.
Non-mainnet does not use the estimate.

### D7. The clock is a trust input

`now = max(caller_clock, stored_high_water_mark, chain_time(best_tip))`.

- The caller's clock is the trust input.
- The high-water mark stops a wound-back clock from shrinking the bar.
- The chain time is a floor for a caller clock that lags the chain. It is the
  median of the last 11 header times, so one miner cannot move it.
- A submission with no clock does not advance the mark. The chain time alone
  would let a pre-mined old chain finalize itself.

The high-water mark is updated on every submission with a clock, whether or
not the bar clears.

### D8. Competing forks cap the candidate

A stored fork is a threat when `work(fork_tip) + need >= work(best_tip)`. It
could still win within the bar. The candidate is capped at the deepest block
shared by the best chain and every such fork.

A fork below that threshold is ignored. It cannot win without more work than
the bar, so it cannot stall finality cheaply.

If the shared prefix is below the current mark, the advance does not happen.
That is not a conflict yet: the fork is stored and below the best chain. It
becomes a conflict only if it becomes best, which the continuity check in D3
detects.

### D9. Conflicts are errors, not recovery

Three cases fail:

| Case | Error |
|---|---|
| A new fork at or below the mark, above the floor | `ForkBelowFinalized` |
| A stored fork at or below the mark would become best | `FinalityConflict` |
| The best chain excludes the mark | `FinalityConflict` |

The checkpoint floor is checked first, so a fork at or below the floor reports
`ForkBelowCheckpoint`. Each failure changes nothing: no header, no fork, no
mark move. The caller halts and reacts. The core does not re-anchor, retry or
roll back.

A manual off-best mark in automatic mode is a conflict on the next submission.
That is correct: the caller asked for a mark the best chain does not contain.
Use the manual policy or move the best chain first.

### D10. The spine is a first-class open input

`Config` takes `Spine::Embedded` or `Spine::From { height, header }`.

- `Embedded` is the shipped period-start set. It starts at the network genesis.
- `From` is one trusted header at any height. The root fork starts there. The
  work below it is unknown and irrelevant.
- A higher spine on reopen raises the mark to the new entry. The open's
  checkpoint prune is BTC-11 and is separate from the mark move.
- A conflicting spine fails with `CheckpointMismatch`.

Alternative: keep `with_checkpoints` (test-only) and `with_trusted_checkpoints`
(period-aligned). Rejected: two shapes for one concept, and the test-only one
is a production concept in disguise.

## Deviation table

| # | Deviation | Bitcoin Core or rskj | Here | Why |
|---|---|---|---|---|
| D1 | The mark is local | Core has no finality mark; it has `assumevalid` and checkpoints | a local weak-subjectivity mark | the node needs a stable prefix |
| D2 | The bar uses a work estimate | Core follows the most-work chain only | `max(100, W_est)` | a partition can hide the best chain |
| D3 | The mark runs on the best fork | not applicable | best fork plus continuity | fork 0 was an artifact, not a role |
| D4 | A fork below the mark is refused | Core reorgs to the most work | `ForkBelowFinalized`, `FinalityConflict` | the mark is a promise |
| D5 | The mark move prunes nothing | Core prunes block data on its own schedule | no prune on advance | the mark is a decision, not retention |
| D6 | The estimate is mainnet-only | not applicable | depth-only on testnet and regtest | no reliable hashrate history |
| D7 | The spine is one header at any height | Core's `assumehash` | `Spine::From` | the Electrum and assumehash model |

## Risks / Trade-offs

- The mark's strength depends on the spine's age. The first bar is the
  expensive one; later bars are cheap. Mitigation: ship a fresh spine, and log
  the anchor age.
- A manual `Some(0)` mark removes the coinbase buffer. Mitigation: it is the
  caller's explicit choice; FIN-09 states it.
- A forward mark without pruning grows the coinbase table. Mitigation: the node
  driver calls `prune_coinbases`; the mark is not a retention trigger.
- A backward manual mark invalidates dependent work. Mitigation: the call
  returns the previous mark; the caller discards the work.
- The estimate uses floating point. Mitigation: fixed constants, a closed-form
  test against numeric integration, and explicit saturation.
- The spine at an arbitrary height leaves the work below it unknown.
  Mitigation: the root fork starts at the spine; the consumers use the work
  above it; the RSK tail starts at the first finalized boundary at or above the
  spine.

## Verification plan

**Layer 1, scenario.** One test per requirement, named `fin_00_...` to
`fin_11_...`. Each calls `Core::apply`. Boundaries: depth 99, 100 and 101; work
at the bar minus one and at the bar; estimate 50, 100 and 101; a competing fork
at the threshold and one below; the clock forward, backward and absent; the
manual depth 0, 1, 100 and beyond the tip; the floor and mark precedence. The
synthetic chain keeps the mainnet difficulty rule and a relaxed proof-of-work
limit. The estimate injection replaces `without_partition_safety`.

**Layer 2, state machine.** `crates/core/specs/bitcoin_finality.rs` holds a
model that predicts the mark, the best fork, every stored header row, the
canonical chain, the `SubmitBitcoinOutcome` fields and the exact `Error`
variant. It computes the estimate from the injected `Blocks(n)`, not from the
implementation. Transitions: submit runs at two clocks, forks above and below
the mark, a fork of a fork, manual advances, and an open with a higher spine.
Invariants: the mark is stored; the automatic mark is monotone and on the best
fork; the manual mark can go backward and returns the previous mark; a mark
move changes no header and no coinbase row.

**Layer 3, exhaustive search.** Skipped. The rule is a small function of the
best fork, the mark and the clock. The state machine covers the transitions,
and the scenarios cover the constants. An exhaustive search would replay the
same finite space.

**Layer 4, mutation.** Run `cargo mutants -p rsk-core -f
crates/core/src/btc/finality.rs -- --lib` and the same for the finality
portions of `btc.rs` and `store/btc.rs`. Triage every missed mutant with the
rule in `docs/core-verification.md`. Excluded targets:

| Location | Target | Reason |
|---|---|---|
| `finality.rs` `FinalityStatus::progress_percent` | the 999 cap and the percentage | display only; no caller reads it as a decision |
| `btc.rs` `fmt_safety`, `fmt_age` | the log strings | log text, not behavior |
| `finality.rs` `u256_to_f64` | the limb loop | only feeds the display and `phi` |

**Layer 5, symbolic.** Kani proofs in `btc/finality.rs` for the integer parts:
`need_blocks * w` cannot overflow past `U256::MAX` (saturating multiply), the
depth subtraction `tip - 100` saturates at zero, and the bar comparison is
total. The estimate uses `f64`, which CBMC handles poorly, so the closed-form
test and the property tests cover it instead. No new byte parser, so no new
fuzz target; the existing `checkpoints_text` target still covers the spine
text.

## Migration Plan

- Add `META_FINALIZED_HASH` and write it on every mark move. On open, a store
  with a height but no hash derives the hash from the best fork at that height;
  if the height is pruned, it resets the mark to the spine.
- Remove `Config::partition_safety` and `without_partition_safety`. Update the
  seven test call sites and `crates/core/specs/support.rs`.
- Change `Core::finalized` to return `Option<Mark>`. Update the node, the CLI
  and `crates/rsk/src/sync.rs`. Drop the `best == 0` assertion.
- Remove the `delete_coinbases_below` call from the advance. The node calls
  `prune_coinbases` after an advance if it wants the old behavior.
- Rollback: an older binary reads `META_FINALIZED` as before and ignores the
  new hash key. The removed config field is a compile-time break, not a store
  break.

## Open Questions

None. The remaining choice is the code name of the method parameter (`depth`
versus `confirmations`); it does not change the behavior or the spec.
