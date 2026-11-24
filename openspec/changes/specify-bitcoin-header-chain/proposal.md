# Proposal

## Why

The batch submission path stores Bitcoin headers in a fork tree and selects the
best fork. The behavior is tested, but its contract lives outside OpenSpec. The
repository-root `specs/bitcoin-headers.md` still uses the clause letters `O1`
and `E1` to `E7`, and the durable specification does not cover the tree. The
`bitcoin-header-` prefix separates this tree from the Rootstock sidechain.

The next capabilities, `bitcoin-finality` and the Rootstock checkpoint policy,
need one reviewable home for the tree. The finality mark also needs a clean
input: it must not move past the best fork.

The shipped reanchor is unspecified and it does not check the best fork. It
adopts a configured checkpoint the store already holds. It buys at most one
depth window, and the automatic advance gives the same movement with the next
stored header. Remove it.

## What Changes

- Create the `bitcoin-header-chain` capability. It has nine requirements,
  `BTC-00` to `BTC-08`. Each requirement has scenario tests that drive
  `Core::apply`.
- Define the **trusted checkpoints**: the configured headers that the crate
  assumes correct. The crate plants them at open. Each trusted checkpoint lies
  at a Bitcoin period boundary, and the heights are one period apart.
- Keep the open rule (legacy O1) in this capability as two requirements.
  `BTC-00 Trusted checkpoints` plants the checkpoints on an empty store.
  `BTC-01 Store identity` compares an existing store with the configuration.
- Name the batch rules after their property: `BTC-02 Atomicity`,
  `BTC-03 Idempotency` (the stored prefix and the no-op) and `BTC-04
  Contiguity` (the link rule and `LinkBreak`).
- Name the tree rules after their subject: `BTC-05 Forks`, `BTC-06 Stored
  set`, `BTC-07 Best fork`.
- Add `BTC-08 Outcome` for the four batch fields: the new header count, the
  previous best fork, the best fork, and the canonical tip height.
- Scope the finality mark out. `bitcoin-finality` will own the mark, the
  creation gate (`ForkBelowFinalized`) and best-fork admittance. This
  capability references those two rules and does not restate them.
- Remove the shipped reanchor: `Action::ReanchorShipped`,
  `Outcome::Reanchored`, `Core::reanchor_shipped`, `reanchor_shipped_impl` and
  the `resume_from` helper. **BREAKING**: an exhaustive match on `Action` or
  `Outcome` outside the crate needs an edit. No caller in this workspace
  matches either.
- Move the delivery machine and `store_identity_matches_the_spec` from
  `crates/core/specs/bitcoin_headers.rs` to
  `crates/core/specs/bitcoin_header_chain.rs`. Add the chain scenarios there.
  The HDR machines and the finality machine stay until their capabilities
  exist.
- Correct the `prev_best_fork` field doc. It says the field is `None` on the
  first connection or when the batch stays on the same fork. The code always
  reports the previous best fork.
- Keep the repository-root `specs/` and `docs/` read-only. Record one
  follow-up: `docs/terminology.md` needs the `trusted checkpoint` and
  `finalized checkpoint` terms.

## Capabilities

### New Capabilities

- `bitcoin-header-chain`: the trusted checkpoints, the fork tree, the batch
  contract and the best-fork choice for Bitcoin headers.

### Modified Capabilities

None. The `bitcoin-header-validation` requirements do not change.

## Impact

- `crates/core/specs/`: a new chain file, the machine move, the new scenarios
  and the shared submit helpers.
- `crates/core/src/btc.rs`: remove the reanchor implementation and its test.
  Correct one field doc.
- `crates/core/src/core.rs`: remove one `Action` variant and one `Outcome`
  variant.
- `crates/rsk/src/node.rs`: delete `resume_from`, inline the start height,
  update the `sync` doc and three tests.
- No store schema change. No new dependency. The crate is not published, so
  the breaking removal affects no external caller.
- No requirement change to `bitcoin-header-validation`.
