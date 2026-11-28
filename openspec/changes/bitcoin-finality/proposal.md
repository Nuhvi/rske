# Proposal

## Why

The node must choose a Bitcoin prefix that it will not reorg after it spends
work on the Rootstock tail and on coinbase scans. A reorg below that prefix
wastes the work. The current code advances a finality mark inside the submit
path, but no capability owns the rule: the mark is a bare height, it moves only
while fork 0 is the best fork, a conflicting fork is accepted, and an external
caller with its own finality evidence has no way to set it.

## What Changes

- Create the `bitcoin-finality` capability with twelve requirements, FIN-00 to
  FIN-11.
- The mark becomes a stored block identity: a height and a hash. The store
  keeps it. One write path moves it.
- One method writes the mark: `advance_final_block(Option<u64>)`. `None` means
  the coinbase-maturity depth (100). `Some(n)` means the block `n` below the
  best tip. The method does not check the bar, the clock or continuity.
- Moving the mark SHALL NOT prune headers or coinbases. The mark is a decision,
  not a retention trigger. The coinbase prune stays a separate action.
- The automatic policy keeps the partition-safe rule: a 100-block floor, a
  price-law work estimate, the competing-fork cap and the conflict check. It
  computes a depth and calls the same method.
- The manual path lets an embedder set the mark after its own check. It is the
  seam for external finality systems. No Binohash, watchtower or certificate
  type is added.
- A fork at or below the mark SHALL fail with `ForkBelowFinalized`. A fork at or
  below the mark that would become best, or a best chain that excludes the mark,
  SHALL fail with `FinalityConflict`. The Core returns the error. The caller
  halts and reacts. The Core does not recover automatically.
- The trusted spine becomes a first-class input at open: the embedded period
  starts, or one caller header at a height. A higher spine raises the mark.
- The work estimate becomes a policy input. A test injects a fixed block count
  or modified formula constants. The old boolean goes.
- **BREAKING**: `Config::partition_safety` and
  `Config::without_partition_safety` are removed.
- **BREAKING**: `Core::finalized` returns a mark block, not a height. The
  finalized checkpoints, the coinbase retention floor, the Bitcoin tail work
  and the Rootstock anchor check read the mark's fork.
- **BREAKING**: the automatic advance no longer deletes coinbases below the new
  finalized checkpoint.
- Fork 0 stops being special. The automatic rule runs on the best fork. A
  normal reorg above the mark no longer freezes finality.
- Non-mainnet networks keep depth-only finality. The estimate does not run and
  the test injection is refused.
- Out of scope: Binohash, watchtowers, certificate types, pruning below the
  mark, and a public policy surface.

## Capabilities

### New Capabilities

- `bitcoin-finality`: the mark, the trusted spine, the work estimate, the bar,
  the advance, the clock, the competing forks, the conflict and the finalized
  consumers.

### Modified Capabilities

- `bitcoin-header-chain`: BTC-03 ends the privilege of the floor to refuse
  alone, so the finality mark rule can refuse a fork above the floor. BTC-09
  now makes the best-fork choice subject to the finality mark, so a chain that
  excludes a finalized block cannot become best.

## Impact

- `crates/core/src/btc/finality.rs`: the estimate policy, the bar and the mark
  rule.
- `crates/core/src/btc.rs`: the advance on the best fork, the conflict, the
  manual writer and the finalized readers. The advance stops deleting coinbases.
- `crates/core/src/core.rs`: the spine and the policy inputs; the boolean goes.
- `crates/core/src/store/btc.rs`: the mark identity and the mark-fork reads.
- `crates/core/src/error.rs`: the conflict variant; `ForkBelowFinalized` gains
  its use.
- `crates/core/specs/bitcoin_finality.rs`: the scenarios and the state machine.
  `support.rs` gains the easy mainnet fixture and the estimate injection.
- `crates/core/specs/bitcoin_header_chain.rs`: the BTC-03 scenario.
- `crates/rsk/src/sync.rs`: the fork-0 assertion and the finality log.
- `crates/rsk/src/ibd.rs`, `crates/rsk/src/coinbase.rs`: the mark reads.
- `docs/core-verification.md`: the coverage and the known gaps.
- `README.md`: the finality section.

The legacy `specs/` and `docs/` files stay read-only.
