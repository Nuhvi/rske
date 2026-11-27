# Tasks

## 1. Checkpoint input and store shape

- [x] 1.1 Add `Config::with_trusted_checkpoints`: a public positional
  `&[Header]` input, resolved to heights `P * k` with the embedded mainnet set
  as the default and the existing test-only explicit-height switch kept.
  Verify: `cargo check -p rsk-core` and a config test that resolves the
  positional list.
- [x] 1.2 Stop the runtime ladder: remove the `btc_checkpoints` insert from
  `put_headers`; keep the `put_checkpoints` work formula and the `btc_headers`
  rows. Verify: a store test shows no checkpoint row appears when a synced
  header crosses a period start.
- [x] 1.3 Add `CoreState.checkpoint_floor` and `Error::ForkBelowCheckpoint`;
  expose the floor through `state()`. Verify: `cargo check -p rsk-core` and a
  state test reads the floor after open.
- [x] 1.4 Document `P` (HDR-10's 2016 on every network, Bitcoin Core's 144 on
  regtest) in the checkpoint and config docs. Verify: `cargo test --doc -p
  rsk-core`.

## 2. Open: compare, adopt and prune

- [x] 2.1 Compare each configured entry at or below the stored tip with the
  canonical header; a difference fails with `CheckpointMismatch`; an entry
  above the tip is ignored; a shorter set changes nothing. Verify: the BTC-02
  scenarios pass.
- [x] 2.2 Adopt the matching entries: write their checkpoint rows and raise
  the floor to the highest adopted entry. Verify: the BTC-02 `Longer set`
  scenario passes.
- [x] 2.3 Prune on open: remove the forks that do not carry the highest entry
  with their `btc_forks`, `btc_headers` and `btc_hash_loc` rows; remove the
  headers below the entry that are not entries; make the heaviest survivor
  best; reconcile the coinbase rows; run it all in one write transaction so a
  refused open changes nothing. Verify: the BTC-11 scenarios pass and a store
  test checks the removed rows.
- [x] 2.4 Clean a legacy store on the first open: a store whose runtime ladder
  added entries above a stored fork loses that fork and the headers below the
  highest entry. Verify: a fixture with an inserted legacy entry opens clean.

## 3. Submit path

- [x] 3.1 Gate a new fork with `forkpoint <= floor` and report
  `ForkBelowCheckpoint`; remove the `ForkBelowFinalized` gate from the chain
  path. Verify: the BTC-03 scenarios pass.
- [x] 3.2 Remove the mark-crossing admittance test. Keep the finality rule's
  side effects, but never let the mark refuse a fork above the floor. Verify:
  the BTC-09 `Deep reorg` and `Reorg back` scenarios pass, and the finality
  unit tests still pass.
- [x] 3.3 Preserve the strict-work switch, the equal-work tie, the canonical
  read, no removal on a switch, and the four outcome fields. Verify: the
  BTC-08, BTC-09 and BTC-10 scenarios pass.

## 4. Executable specification

- [x] 4.1 Rewrite `crates/core/specs/bitcoin_header_chain.rs` scenario tests
  for BTC-00 to BTC-11, each calling `Core::apply` with production validation
  flags and carrying a `// Scenario:` comment. Verify: `cargo test -p rsk-core
  --lib specs::bitcoin_header_chain`.
- [x] 4.2 Update the chain state machine: predict the full `CoreState`
  (including `checkpoint_floor`), every `StoredHeader` row and the four outcome
  fields; compute work from the target, not `header_work`; include the floor
  and prune cases. Verify: the machine test passes.
- [x] 4.3 Update `bitcoin_headers.rs`, `support.rs`, `rootstock_checkpoints.rs`
  and the `btc.rs` unit tests for the floor gate and the open prune; replace
  the mark-gate tests and keep the finality mark tests. Verify: `cargo test -p
  rsk-core`.
- [x] 4.4 Update `crates/rsk` (sync and node) and the CLI exporter for the
  floor gate and the unchanged finality side effects. Verify: `cargo test -p
  rsk` and the CLI tests.
- [x] 4.5 Update `docs/core-verification.md` and the crate docs. Verify: the
  coverage section matches the new scenario names.

## 5. Verification

- [x] 5.1 Run the gate: `openspec validate --all --strict`, `cargo test -p
  rsk-core --lib specs::`, `cargo test -p rsk-core`, `cargo test -p rsk`,
  `cargo check --workspace`. Verify: every command passes.
- [ ] 5.2 Run a scoped mutation audit, one file at a time, on
  `crates/core/src/btc.rs` and `crates/core/src/store/btc.rs`, and triage each
  missed mutant. Verify: `mutants.out/missed.txt` holds only named equivalents
  or exclusions.
- [x] 5.3 Confirm the verification layer record: scenario, state machine and
  mutation are used; exhaustive search is skipped (the fork tree is above the
  budget); Kani is skipped (no new arithmetic); fuzzing is skipped (no new
  parser). Verify: the design section states each layer.
- [x] 5.4 Run `scripts/verify.sh`. Verify: the script exits zero.

## Workflow follow-up

- Archive the change after the review requirements are satisfied.
- Verify the archived spec.
