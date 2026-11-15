# Proposal

## Why

`rsk-core` already validates Bitcoin headers, but the contract lives outside
OpenSpec. The repository-root Markdown, the Rust comments and the executable
spec disagree on clause IDs. The OpenSpec capability replaces the Markdown as
the source; the legacy files stay read-only. The supported networks are
implicit, the version floor is a silent skip, and the median-time-past deviation
is a footnote. The chain and finality capabilities will build on this contract,
so it needs one reviewable home first.

## What Changes

- Create the `bitcoin-header-validation` capability. It has nine requirements.
  Each requirement has boundary scenarios that become Core-level tests.
- Define the supported networks: mainnet, testnet3 and regtest. Refuse testnet4
  and signet with `UnsupportedNetwork`. **BREAKING**: an exhaustive match on
  `Error` outside the crate needs a new arm.
- Specify the median-time-past rule with its sparse-window deviation (HDR-07).
  A fresh store seeds at a period boundary, so the first 11 heights above that
  seed have no full window. The rule does not apply there.
- Replace the `C*` and `V*` clause letters with stable `HDR-*` requirement IDs
  in Rust comments. The legacy Markdown stays read-only. The `O*` and `E*`
  clauses stay until the chain capability exists.
- Use `submission` for the act of handing a header batch to the core. Each
  Bitcoin Core rule links the function and reject reason it comes from.
  The API rename to `submit_headers` is a separate refactor change with
  `skip_specs`.
- Add a Core-level scenario test file for the rules. Move the plain boundary
  tests out of the state-machine file. Add a shared fixture module.
- Declare the version floor as an out-of-scope deviation for this capability.
  The design records the decision and the follow-up.

## Capabilities

### New Capabilities

- `bitcoin-header-validation`: the rules that decide whether one Bitcoin header
  is valid against its stored ancestors and the caller's clock.

### Modified Capabilities

None.

## Impact

- `crates/core/src/core.rs`, `crates/core/src/error.rs`: the supported-network
  refusal and one error variant.
- `crates/core/src/btc.rs`, `crates/core/src/btc/validation.rs`: the requirement
  IDs in comments and the testnet4 base-branch removal.
- `crates/core/src/rsk.rs`, `crates/rsk/src/rootstock.rs`: the network mapping
  cleanup.
- `crates/core/specs/`: a new scenario file, a shared fixture module, and a
  smaller state-machine file.
- The repository-root `specs/` and `docs/` stay read-only. The change does not
  edit them and does not depend on them.
- The public API keeps `connect_headers` until the separate rename change.
- No store schema change. No new dependency.
