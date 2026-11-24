# Tasks

## 1. Remove the shipped reanchor

- [x] 1.1 Remove `Action::ReanchorShipped`, `Outcome::Reanchored` and the
  dispatch arm from `crates/core/src/core.rs`. Verify `cargo check -p rsk-core`
  passes.
- [x] 1.2 Remove `reanchor_shipped_impl`, `Core::reanchor_shipped` and
  `a_resumed_store_adopts_the_newest_shipped_checkpoint_it_holds` from
  `crates/core/src/btc.rs`. Verify `cargo test -p rsk-core` passes and
  `rg -n "reanchor" crates/core/src` finds no reference.
- [x] 1.3 Delete `resume_from` in `crates/rsk/src/node.rs` and inline
  `tip + 1` at the `sync` call site. Remove the "Adopt the newest embedded
  checkpoint" sentence from the `Node::sync` doc. Verify `cargo test -p rsk`
  passes.
- [x] 1.4 Update the node tests: keep the tip assertion in
  `opening_a_fresh_mainnet_store_trusts_the_embedded_checkpoints` and
  `opening_a_fresh_non_mainnet_store_starts_from_genesis`; replace
  `reopening_an_initialized_store_keeps_its_tip` with a test that asserts
  the tip after a reopen. Verify `cargo test -p rsk` passes.

## 2. Move the chain tests

- [x] 2.1 Create `crates/core/specs/bitcoin_header_chain.rs` and declare it in
  `crates/core/specs/mod.rs`. Move `HeaderPrediction`, `HeaderModel`,
  `HeaderTransition`, `predict`, `HeaderMachine`, `HeaderSut` and
  `bitcoin_headers_match_the_spec` to the chain file. Rename the test to
  `bitcoin_header_chain_matches_the_spec`. Move `HeaderUniverse`, `HeaderKind`
  and `header_universe` to `support.rs`, and import them in the chain file and
  in `bitcoin_header_validation.rs`. Replace the `E*` clause comments with the
  matching `BTC-*` IDs. Verify `cargo test -p rsk-core --lib specs::` passes.
- [x] 2.2 Move `submit`, `expect_err` and `expect_accepted` from
  `bitcoin_header_validation.rs` to `support.rs`. Update both test files.
  Verify `cargo test -p rsk-core --lib specs::` passes.
- [x] 2.3 Update the module docs in `specs/mod.rs` and `bitcoin_headers.rs`:
  one file per capability; the chain file holds the chain machine;
  `bitcoin_headers.rs` holds the HDR and finality machines until their
  capabilities exist. Verify `rg -n "chain ingestion" crates/core/specs` finds
  no stale description and `cargo test -p rsk-core --lib specs::` passes.

## 3. Store identity (BTC-00, BTC-01)

- [x] 3.1 Add `btc_00_trusted_checkpoints` with a two-entry checkpoint set:
  the root fork holds the checkpoints, the root fork is best, and the highest
  checkpoint is the mark. Verify the test passes and asserts `Core::state()`.
- [x] 3.2 Add `btc_01_store_identity`: reopen with the same set; a longer set
  on the same chain; a different network; a different header at a checkpoint
  height. Assert `NetworkMismatch`, `CheckpointMismatch`, the store unchanged
  on a refusal, and no header added on a success. Use a file-backed store and
  the test-only `with_checkpoints` seam. Verify the test passes.
- [x] 3.3 Remove `store_identity_matches_the_spec` from
  `bitcoin_headers.rs`. Verify `cargo test -p rsk-core --lib specs::` passes.

## 4. Batch contract (BTC-02, BTC-03, BTC-04)

- [x] 4.1 Add `btc_02_atomicity`: a batch that starts a fork and fails on a
  later header leaves no header and no fork; a refused batch leaves the state
  equal. Verify both cases and the exact error variant.
- [x] 4.2 Add `btc_03_idempotency`: a repeated prefix, an all-stored batch, an
  empty batch, and a prefix stored on a losing fork. Verify the accepted counts
  and the no-op state equality.
- [x] 4.3 Add `btc_04_contiguity`: a gap in the new run, a bad prefix link, and
  a stored header after a new header. Verify each fails with `LinkBreak` at the
  expected height.

## 5. Tree (BTC-05, BTC-06)

- [x] 5.1 Add `btc_05_forks`: extend the tip, fork from a non-tip, fork of a
  fork, siblings, a start below the best tip, and a second submission at a fork
  tip. Verify `Core::state().forks` reports the parent and the forkpoint, and
  `Core::locate` finds each first header.
- [x] 5.2 Add `btc_06_stored_set`: a losing fork stays stored, a switch removes
  no header, and the tip work equals the parent work plus the run's work.
  Verify `Core::stored_headers` and `ForkState::tip_work`.

## 6. Best fork and outcome (BTC-07, BTC-08)

- [x] 6.1 Add `btc_07_best_fork`: heavier wins, lighter stays, equal keeps, a
  later win, the root wins back, and the canonical read. Verify
  `Core::state().best_fork` and the canonical tip.
- [x] 6.2 Add `btc_08_outcome`: extend, switch, no-op, and a losing-fork batch.
  Verify the accepted count, the previous best fork, the best fork and the
  canonical tip.
- [x] 6.3 Correct the `prev_best_fork` field doc in `crates/core/src/btc.rs`:
  the field always reports the previous best fork. Verify the comment matches
  the value the submit path sets.

## 7. Integration gate

- [x] 7.1 Run `cargo test -p rsk-core --lib specs::`, `cargo test -p rsk-core`
  and `cargo test -p rsk`. Verify all three pass.
- [x] 7.2 Run `cargo check --workspace`. Verify it passes.
- [x] 7.3 Run `openspec validate "specify-bitcoin-header-chain" --type change
  --strict`. Verify it passes.

## Deferred work

These items are outside this change. Create a separate change for an item when
you pick it up.

- **`bitcoin-finality`.** Own the mark's advance, the mainnet requirement, the
  clock (E6, E7), the creation gate and best-fork admittance. Move the finality
  machine out of `bitcoin_headers.rs`. Fill the two references in BTC-05 and
  BTC-07.
- **Finalized checkpoint row.** `docs/terminology.md` now has the `trusted
  checkpoint` and `checkpoint table` rows. The finality change adds the
  `finalized checkpoint` row.
- **Legacy documents.** `specs/bitcoin-headers.md` and
  `docs/core-verification.md` still describe the machine with `O1` and `E1` to
  `E7`. Delete them in a cleanup after the durable specs cover every clause.
- **Proptest regression file.** The renamed machine uses
  `crates/core/specs/bitcoin_header_chain.txt`. The old
  `bitcoin_headers.txt` is untracked and holds only comments.

## Workflow follow-up

- Archive the change after review. After archive, the durable spec is
  `openspec/specs/bitcoin-header-chain/spec.md`.
