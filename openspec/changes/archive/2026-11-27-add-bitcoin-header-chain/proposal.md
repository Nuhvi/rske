# Proposal

## Why

`openspec/specs/` holds only `bitcoin-header-validation`. It defines the rules
for one header. It does not define what a batch of headers does to the store:
the fork tree, the batch contract, the best-fork choice and the return value.
The earlier `bitcoin-header-chain` change stayed in draft and then left the
tree, so no durable spec covers those rules today.

The trusted checkpoint set is also a crate decision. The crate anchors one
header per difficulty period at the period's first height. That shape needs no
extra difficulty value: the first header above the anchor is inside the
period that the anchor starts, and its difficulty is the parent's. Electrum
anchors at the period end and ships the next target for that reason. The
current fork gate is a moving mark, not the checkpoint gate that an SPV
wallet uses. Electrum refuses a fork at or below its last trusted checkpoint
and allows a reorg of any depth above it. This capability should state that
same rule.

## What Changes

- Create the `bitcoin-header-chain` capability with twelve requirements,
  BTC-00 to BTC-11. Each requirement gets scenario tests that call
  `Core::apply`.
- The trusted checkpoint set holds the first header of each difficulty
  period, from genesis, as a plain list of headers. The period is 2016 on
  every supported network (HDR-10).
- The set is an input at open. The embedded mainnet set is the default. An
  open adopts a longer configured set up to the tip; a submission never
  extends it.
- On every open, the store prunes every fork that does not carry the highest
  entry and every header below it that is not an entry, then makes the
  heaviest surviving fork best.
- The trusted checkpoints are the only reorg gate: a new fork SHALL start
  above the highest checkpoint, and a reorg above it SHALL have no depth
  limit. A refused fork reports the new `ForkBelowCheckpoint` error.
- A batch is atomic and idempotent. A refused batch changes nothing. The
  store skips the stored prefix, and an empty or fully stored batch succeeds
  with no write.
- The capability defines the fork tree, the stored set, the strict-work
  best-fork rule and the canonical chain.
- The outcome reports the four chain fields of `SubmitBitcoinOutcome`:
  `accepted`, `prev_best_fork`, `best_fork` and `tip_height`.

## Capabilities

### New Capabilities

- `bitcoin-header-chain`: the trusted checkpoint set, the checkpoint floor,
  the batch contract, the fork tree, the stored set, the best fork and the
  submission outcome for Bitcoin headers.

### Modified Capabilities

None. `bitcoin-header-validation` keeps its requirements. The trusted entries
are full headers, so the per-header rules apply above the floor unchanged.

## Impact

- `crates/core/src/store/btc.rs`: the open adopts the configured entries and
  prunes the forks and headers below the floor in one transaction;
  `put_headers` never extends the checkpoint table; `put_checkpoints` keeps
  the work formula.
- `crates/core/src/core.rs`: the checkpoint set becomes a public open-time
  input.
- `crates/core/src/btc.rs`: the open runs the adopt and prune; the chain path
  gates on the checkpoint floor and reports the new error; the canonical
  iterator keeps its period jump.
- `crates/core/src/error.rs`: one added variant.
- `crates/core/specs/bitcoin_header_chain.rs`: the scenarios and the state
  machine.
- `docs/core-verification.md`: the coverage section.
- The legacy `specs/` and `docs/` files stay read-only.
