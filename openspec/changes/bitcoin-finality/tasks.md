# Tasks

Verification layers: scenario (used, FIN-00 to FIN-11), state machine (used),
mutation (used), Kani (used for the integer bar and depth arithmetic), fuzzing
(skipped: this change adds no byte parser; the existing `checkpoints_text`
target still covers the spine text).

## 1. Mark storage

- [ ] 1.1 Add `META_FINALIZED_HASH` next to `META_FINALIZED` in
  `crates/core/src/store/btc.rs`, with a read/write pair for the
  `(height, hash)` mark. Verify a store round-trips the pair and an old store
  with only the height derives the hash from the best fork at that height.
- [ ] 1.2 Reset the mark to the highest spine entry when the stored height is
  pruned on open. Verify an old store whose mark height is gone opens with the
  spine mark, and the store is not otherwise changed.
- [ ] 1.3 Add `Mark { height, hash, time }` and change `CoreState.finalized` and
  `SubmitBitcoinOutcome.finalized` to it. Verify `Core::state()` returns the
  block identity and the header time.

## 2. Spine and configuration

- [ ] 2.1 Add `Spine::Embedded` and `Spine::From { height, header }`, and
  `Config::with_spine`. Fold the current `with_checkpoints` and
  `with_trusted_checkpoints` into it, so a caller header at any height is a
  normal input. Verify a fresh open with one header at a non-period height
  stores it and sets the mark to it.
- [ ] 2.2 Remove `Config::partition_safety` and
  `Config::without_partition_safety`. Update the seven test call sites and
  `crates/core/specs/support.rs`. Verify `cargo check --workspace` and
  `cargo test -p rsk-core --lib specs::` with no boolean left.
- [ ] 2.3 Add `FinalityPolicy` with `Automatic { estimate }` and `Manual`, and
  the crate-private `WorkEstimate` trait. Verify the default is `Automatic`
  with the price law, and the estimate setter refuses testnet and regtest.

## 3. The estimate

- [ ] 3.1 Move the price-law constants, `estimated_hashrate` and the closed
  form into `PowerLaw`, and make `WorkEstimate::blocks` return `u64` with
  saturation. Verify the closed-form test against numeric integration and the
  short-window approximation test pass.
- [ ] 3.2 Add the `#[cfg(kani)] mod proofs` harnesses in
  `crates/core/src/btc/finality.rs` for the saturating bar multiply and the
  depth subtraction. Verify `cargo kani --harness <name>` on each harness.
- [ ] 3.3 Add the test estimate (`Blocks(n)`) and the modified-constant path.
  Verify `Blocks(0)` leaves the 100-block floor, `Blocks(250)` makes the bar
  250, and a changed `price_n` changes the estimate.

## 4. The mark rule

- [ ] 4.1 Replace the `best_fork == Some(0)` gate in `measure_finality` with
  the best fork and the continuity check. Verify a normal reorg above the mark
  still advances the mark.
- [ ] 4.2 Add the competing-fork cap: the candidate is the deepest block shared
  with every fork whose work plus the bar reaches the best chain. Verify a fork
  at the threshold caps the mark and a fork one below the threshold does not.
- [ ] 4.3 Add the conflict errors in the submit path: `ForkBelowFinalized` for
  a new fork at or below the mark, and `FinalityConflict` for a stored fork
  below the mark that would become best or a best chain that excludes the
  mark. Verify each error and that the store is unchanged.
- [ ] 4.4 Add `advance_final_block(Option<u64>)` as the single writer and make
  the automatic path call it. Verify the automatic result equals a manual call
  at the same depth, and the previous mark is returned.
- [ ] 4.5 Remove the `delete_coinbases_below` call from the advance. Verify a
  mark move leaves every coinbase row until `prune_coinbases` runs.
- [ ] 4.6 Make `finalized_checkpoints`, `coinbase_retention_floor`,
  `btc_tail_work` and the Rootstock `in_tail` read the mark's fork and hash.
  Verify a manual non-best mark is read on its own fork, and a reorg above the
  mark leaves the readings unchanged.

## 5. Scenario tests (layer 1)

- [ ] 5.1 Add `crates/core/specs/bitcoin_finality.rs` with one test per
  requirement, `fin_00_mark_identity` to `fin_11_consumers`, each calling
  `Core::apply` with the production flags. Verify
  `cargo test -p rsk-core --lib specs::bitcoin_finality`.
- [ ] 5.2 Extend `crates/core/specs/support.rs` with the easy mainnet fixture,
  the `From` spine and the estimate injection. Verify the finality scenarios
  and the existing chain scenarios share the fixture without a boolean.
- [ ] 5.3 Update the BTC-03 and BTC-09 scenarios for the mark qualification.
  Verify `cargo test -p rsk-core --lib specs::bitcoin_header_chain`.

## 6. State machine (layer 2)

- [ ] 6.1 Add the `bitcoin_finality` state machine. The model predicts the
  mark, the best fork, every stored header row, the canonical chain, the
  `SubmitBitcoinOutcome` fields and the exact `Error` variant. Verify
  `cargo test -p rsk-core --lib specs::bitcoin_finality` at
  `RSK_PROPTEST_CASES=64`.
- [ ] 6.2 Add the regression file `crates/core/specs/bitcoin_finality.txt` and
  keep it in version control. Verify the shrinking run replays the minimal
  sequence.
- [ ] 6.3 Add the negative control: change the model's mark or tie rule, and
  confirm the machine fails. Verify the failure, then restore the model.

## 7. Integration

- [ ] 7.1 Update `crates/rsk/src/sync.rs` to drop the `best == 0` assertion and
  the finality log. Update `crates/rsk/src/ibd.rs` and
  `crates/rsk/src/coinbase.rs` for the mark reads. Verify `cargo test -p rsk`.
- [ ] 7.2 Update `docs/core-verification.md` coverage and known gaps, and the
  `README.md` finality section. Verify the documented commands run as written.
- [ ] 7.3 Run `openspec validate --all --strict` and
  `cargo test -p rsk-core --lib specs::`. Verify both pass.

## 8. Mutation (layer 4)

- [ ] 8.1 Run `cargo mutants -p rsk-core -f crates/core/src/btc/finality.rs --
  --lib`, and the same for the finality portions of `btc.rs` and
  `store/btc.rs`. Triage every survivor with the rule in
  `docs/core-verification.md`.
- [ ] 8.2 Write `git diff origin/master...HEAD > pr.diff` and run
  `cargo mutants --in-diff pr.diff -p rsk-core -- --lib`. Verify every survivor
  is classified, with an exclusion and a reason where needed.

## Workflow follow-up

- Run `scripts/verify.sh --full` before review.
- Archive the change after the review requirements are satisfied.
