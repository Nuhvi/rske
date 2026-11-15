# Tasks

## 1. Requirement IDs

- [x] 1.1 Replace the `C*` and `V*` comments in `crates/core/src/btc.rs`,
  `crates/core/src/btc/validation.rs`, `crates/core/src/core.rs`, and
  `crates/core/specs/bitcoin_headers.rs` with the matching `HDR-*` IDs. Verify
  `rg -n '\b[CV][0-9]+\b' crates/core/src crates/core/specs` reports no clause
  reference, and `cargo test -p rsk-core --lib specs::` passes.

## 2. Test scaffolding

- [x] 2.1 Create `crates/core/specs/support.rs` and move the shared helpers
  there: `mine`, `mine_bad`, `mine_with_bits`, `mainnet_params`,
  `synthetic_mainnet_core`, `delivery_core`, `mainnet_core`, `work`,
  `canonical_chain`. Declare the module in `crates/core/specs/mod.rs`. Verify
  `cargo test -p rsk-core --lib specs::` passes and
  `crates/core/specs/bitcoin_headers.rs` no longer defines the moved helpers.
- [x] 2.2 Create `crates/core/specs/bitcoin_header_validation.rs` and declare it
  in `mod.rs`. Move `median_time_past_excludes_the_header_itself`,
  `the_target_range_check_runs_before_proof_of_work` and
  `a_header_exactly_two_hours_ahead_is_accepted` from `bitcoin_headers.rs`.
  Verify `cargo test -p rsk-core --lib specs::bitcoin_header_validation` runs
  the three tests and `cargo test -p rsk-core --lib specs::bitcoin_headers`
  still passes.
- [x] 2.3 Update the `crates/core/specs/mod.rs` header to describe one file per
  capability, with `bitcoin_header_validation.rs` for the capability and
  `bitcoin_headers.rs` for the machine. Verify the comment lists every module
  and `cargo test -p rsk-core --lib specs::` passes.

## 3. Network scope

- [x] 3.1 Add `Error::UnsupportedNetwork(Network)`. Refuse testnet4 and signet
  at the top of `Core::new`, before the store opens. Remove the
  `Network::Testnet4` branch from `expected_bits` and its unit test. Drop
  testnet4 from the finality unit-test loop. Make `rootstock.rs::for_network`
  return `None` for the refused networks, and comment the `rsk.rs` fallback as
  unreachable. Verify `cargo test -p rsk-core`, `cargo test -p rsk`, and
  `cargo check --workspace`.
- [x] 3.2 Add `hdr_00_supported_networks` to `bitcoin_header_validation.rs`: a
  file-backed open on testnet4 and signet fails with `UnsupportedNetwork` and
  creates no file; a mainnet, testnet3, and regtest open succeeds. Use a unique
  path under `std::env::temp_dir()` for the refused cases. Verify
  `cargo test -p rsk-core --lib specs::bitcoin_header_validation` passes.

## 4. Rule scenarios

- [x] 4.1 Add `hdr_01_parent_of_the_first_new_header_is_known`,
  `hdr_02_target_range` and `hdr_03_proof_of_work` to
  `bitcoin_header_validation.rs`, one test per requirement. Cover every scenario
  of those requirements, including the range-before-PoW case. Verify
  `cargo test -p rsk-core --lib specs::bitcoin_header_validation` passes and each
  test asserts the exact `Error` variant.
- [x] 4.2 Add `hdr_04_expected_bits_inside_a_period` and
  `hdr_05_minimum_difficulty_networks`. Use mainnet params for HDR-04 and
  testnet3 params with a relaxed limit for HDR-05. Verify the five HDR-05
  scenarios distinguish the limit from the last normal bits.
- [x] 4.3 Add `hdr_06_mainnet_and_regtest_boundaries`. Build one 2015-block
  chain and connect it to a retargeting core; assert the clamped retarget is
  accepted, the parent bits are refused, and the unclamped value is refused.
  Reuse the same store for the regtest cases with the retarget disabled; an
  anchor at 2015 is impossible because the seed must start at a period
  boundary. Verify the test passes and the chain is built once.
- [x] 4.4 Add `hdr_07_median_time_past`. Cover equality, median plus one
  second, exclusion of the new header from the window, and the sparse window.
  Verify all four scenarios pass through `Core::apply`.
- [x] 4.5 Add `hdr_08_future_time`. Cover exactly 7200 seconds, 7201 seconds,
  and no clock. Verify all three scenarios pass through `Core::apply`.

## 5. Integration gate

- [x] 5.1 Run `cargo test -p rsk-core --lib specs::`, `cargo test -p rsk-core`
  and `cargo test -p rsk`. Verify all three pass.
- [x] 5.2 Run `openspec validate "specify-bitcoin-header-validation" --type
  change --strict`. Verify it passes.
- [x] 5.3 Run `cargo check --workspace`. Verify the new error variant compiles
  at every caller.

## Deferred work

These items are outside this change. They are recorded here so the decision is
not lost. Create a separate change for an item when you pick it up.

- **Rename the API.** A separate `rename-header-submission` change with
  `skip_specs: true`: `connect_headers` to `submit_headers`, `ConnectHeaders`
  to `SubmitHeaders`, `ConnectOutcome` to `SubmitOutcome`,
  `Outcome::Connected` to `Outcome::Submitted`, and `connected` to `accepted`.
  That is 138 occurrences in 10 files.
- **Median-time-past option (a).** Seed, or request, 11 context headers so the
  full window exists above a boundary. This would MODIFY HDR-07 and remove the
  deviation.
- **Version floor.** Add the height-activated `version < 2/3/4` check from
  `Params::{bip34,bip66,bip65}_height`. This would add HDR-09 or MODIFY the
  capability.
- **Add a network.** If an RSK deployment ever uses testnet4 or signet, add a
  network requirement and the rules that network needs in a separate change.
- **Differential harness.** Compare accept/reject decisions with `bitcoind
  submitheader`. Regtest cannot exercise the retarget.
- **Proptest regression seeds.** Check in `crates/core/specs/bitcoin_headers.txt`
  when the state-machine changes are committed.

## Workflow follow-up

- Archive the change after review. After archive, the durable spec is
  `openspec/specs/bitcoin-header-validation/spec.md`.
