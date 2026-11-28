# Spec Delta

## Purpose

Defines the local Bitcoin finality mark: the block the node will not reorg, the
policy that advances it, and the way an external caller sets it.

## ADDED Requirements

### Requirement: [FIN-00] Mark identity
The finalized read SHALL return the mark as a stored Bitcoin block: its height,
its hash and its header time. The mark SHALL be monotone in height and SHALL
only move through the advance method. The automatic policy SHALL run on the best
fork. Fork 0 SHALL have no special role.

#### Scenario: Fresh store
- **WHEN** the Core opens an empty store with a spine
- **THEN** the mark is the highest spine entry
- **AND** that block is stored at its height

#### Scenario: Reopen
- **WHEN** the Core reopens the store with the same spine
- **THEN** the mark is the same block

#### Scenario: Normal reorg above the mark
- **WHEN** a heavier fork replaces the best tip above the mark
- **THEN** the mark keeps advancing on the new best fork
- **AND** the mark is not frozen by the fork change

#### Scenario: Every mark is a stored block
- **WHEN** the mark moves
- **THEN** its block is stored at its height on the best fork

### Requirement: [FIN-01] Spine and initialization
`Config` SHALL take a spine: the embedded period starts, or one caller header.
`Core::new` SHALL store the spine as the trusted prefix and SHALL set the mark
to its highest entry. An existing store SHALL be checked against the network and
every spine entry at or below the tip. A higher entry SHALL be adopted and SHALL
raise the mark. A mismatch SHALL fail with `CheckpointMismatch`.

#### Scenario: One caller header
- **WHEN** the Core opens an empty store with one trusted header at height H
- **THEN** the store holds that header as the trusted prefix
- **AND** the mark is that header

#### Scenario: Caller header at an arbitrary height
- **WHEN** the trusted header is not a period start and is not genesis
- **THEN** the open accepts it
- **AND** the mark is that header

#### Scenario: Embedded spine
- **WHEN** the Core opens mainnet with the embedded spine
- **THEN** the mark is the last embedded entry

#### Scenario: Reopen unchanged
- **WHEN** the Core reopens with the same spine
- **THEN** the open adds no header
- **AND** the mark is unchanged

#### Scenario: Higher spine
- **WHEN** the Core reopens with a spine entry above the stored mark
- **THEN** the open adopts the entry
- **AND** the mark rises to that entry

#### Scenario: Conflicting spine
- **WHEN** a spine entry differs from the stored header at its height
- **THEN** the open fails with `CheckpointMismatch`
- **AND** the store is unchanged

#### Scenario: Different network
- **WHEN** the store was initialized for another network
- **THEN** the open fails with `NetworkMismatch`

### Requirement: [FIN-02] The bar
**Source**: Bitcoin Core v29.0 coinbase maturity; a coinbase is spendable 100 blocks after its block.
The automatic policy SHALL require `need = max(100, est_blocks) * w` work above
the anchor, where `w` is the work of the block at the anchor and `est_blocks`
is the estimate in blocks at that difficulty. `COINBASE_MATURITY` SHALL be 100.

#### Scenario: Estimate below the floor
- **WHEN** `est_blocks` is 50
- **THEN** `need` is 100 blocks' worth of work

#### Scenario: Estimate at the floor
- **WHEN** `est_blocks` is 100
- **THEN** `need` is 100 blocks' worth of work

#### Scenario: Estimate above the floor
- **WHEN** `est_blocks` is 101
- **THEN** `need` is 101 blocks' worth of work

#### Scenario: Work units
- **WHEN** the anchor block's work is `w` and `need` is 100 blocks
- **THEN** the reported requirement is `100 * w` in work
- **AND** the status reports the same value in blocks

### Requirement: [FIN-03] The estimate
**Source**: [Partition-safe IBD](https://github.com/Nuhvi/partition-safe-ibd); the bottom price law and the frozen dbtc relation.
The estimate SHALL be the bottom price-law work the honest network should have
produced between the anchor time and the caller's clock, in blocks at the
anchor's difficulty. It SHALL saturate at `u64::MAX`. A genesis anchor SHALL
fail closed. A clock at or before the anchor time SHALL give zero.

#### Scenario: Closed form
- **WHEN** the estimate is compared with a numeric integration of the same law
- **THEN** the difference is below one part in ten thousand

#### Scenario: Short window
- **WHEN** the window is short against the chain's age at the anchor
- **THEN** the estimate is about `r0 * T / 600` blocks
- **AND** `r0` is the ratio of the price-law hashrate to the real hashrate at the anchor

#### Scenario: Zero window
- **WHEN** the clock equals the anchor time
- **THEN** the estimate is zero
- **AND** the 100-block floor is the whole requirement

#### Scenario: Clock before the anchor
- **WHEN** the clock is before the anchor time
- **THEN** the estimate is zero

#### Scenario: Genesis anchor
- **WHEN** the anchor time is at or before the genesis time
- **THEN** the requirement fails closed at the maximum value

#### Scenario: Zero anchor work
- **WHEN** the anchor block's work is zero
- **THEN** the requirement fails closed at the maximum value

#### Scenario: Saturation
- **WHEN** the window is absurdly large
- **THEN** the estimate saturates at `u64::MAX` blocks

### Requirement: [FIN-04] The estimate policy
The estimate SHALL be a policy input. Production SHALL use the bottom price
law. A test SHALL inject a fixed block count or modified price-law constants.
The policy SHALL NOT be persisted: a reopen SHALL keep the mark and SHALL apply
the new policy only to future advances.

#### Scenario: Production policy
- **WHEN** the Core opens with the default policy
- **THEN** the estimate uses the bottom price law

#### Scenario: Fixed estimate
- **WHEN** a test injects an estimate of 250 blocks
- **THEN** the bar is 250 blocks

#### Scenario: Zero estimate
- **WHEN** a test injects an estimate of zero
- **THEN** the 100-block floor binds

#### Scenario: Modified constants
- **WHEN** a test changes one price-law constant
- **THEN** the estimate changes as the formula states
- **AND** the rest of the rule is unchanged

#### Scenario: Reopen with a different policy
- **WHEN** the store reopens with another estimate
- **THEN** the mark is unchanged
- **AND** the new estimate applies only to later advances

### Requirement: [FIN-05] The automatic advance
The automatic policy SHALL move the mark to `tip - 100` when the best chain
contains the mark, the candidate is above the mark, and the work above the mark
is at least the bar. It SHALL call the same write method as the manual path. It
SHALL NOT move the mark backwards.

#### Scenario: Depth boundary, too shallow
- **WHEN** the tip is 100 blocks above the anchor
- **THEN** the candidate equals the anchor
- **AND** the mark does not move

#### Scenario: Depth boundary, one past
- **WHEN** the tip is 101 blocks above the anchor
- **THEN** the mark moves to the tip minus 100

#### Scenario: Work boundary, one short
- **WHEN** the estimate is 250 blocks and the best chain carries 249 blocks above the anchor
- **THEN** the mark does not move

#### Scenario: Work boundary, exact
- **WHEN** the best chain carries 250 blocks above the anchor
- **THEN** the mark moves

#### Scenario: One write path
- **WHEN** the automatic policy advances to a depth
- **THEN** the mark is the same as a manual advance to that depth

#### Scenario: No backward move
- **WHEN** the computed candidate is at or below the mark
- **THEN** the mark does not move

#### Scenario: No pruning
- **WHEN** the mark moves
- **THEN** every stored header stays
- **AND** every stored coinbase stays

#### Scenario: Normal reorg
- **WHEN** a heavier fork replaces the best tip above the mark
- **THEN** the mark advances on the new best fork
- **AND** the mark is not frozen

### Requirement: [FIN-06] The clock
The automatic policy SHALL use the caller's clock, raised by the stored
high-water mark and the chain time at the best tip. A submission with no clock
SHALL NOT advance the mark. A backward clock SHALL NOT lower the bar.

#### Scenario: Clock forward
- **WHEN** the caller's clock moves forward
- **THEN** the requirement grows

#### Scenario: Backward clock
- **WHEN** the caller's clock moves backward after a later clock was seen
- **THEN** the requirement does not shrink

#### Scenario: No clock
- **WHEN** the submission passes no clock
- **THEN** the mark does not move

#### Scenario: Chain time floor
- **WHEN** the caller's clock is behind the chain time at the best tip
- **THEN** the rule uses the chain time

#### Scenario: High-water mark
- **WHEN** a submission with a later clock does not clear the bar
- **THEN** the high-water mark still records the later clock

### Requirement: [FIN-07] Competing forks
The automatic policy SHALL cap the candidate at the deepest block shared by the
best chain and every stored fork whose work plus the bar reaches the best
chain. A fork below that threshold SHALL be ignored.

#### Scenario: Fork at the threshold
- **WHEN** a stored fork's work plus the bar equals the best chain's work
- **THEN** the mark is capped at the shared prefix

#### Scenario: Fork one below the threshold
- **WHEN** a stored fork's work plus the bar is one below the best chain's work
- **THEN** the fork does not cap the mark

#### Scenario: Fork above the candidate
- **WHEN** a competing fork shares the candidate block
- **THEN** the mark is not lowered

#### Scenario: Fork below the candidate
- **WHEN** a competing fork diverges below the candidate
- **THEN** the mark is capped at that fork's forkpoint

#### Scenario: Order independence
- **WHEN** the same forks are stored in a different order
- **THEN** the mark is the same

### Requirement: [FIN-08] Conflict
A new fork at or below the mark SHALL fail with `ForkBelowFinalized`. A stored
fork at or below the mark that would become best, or a best chain that excludes
the mark, SHALL fail with `FinalityConflict`. Each failure SHALL change nothing,
and the mark SHALL NOT move.

#### Scenario: New fork at the mark
- **WHEN** a submission starts a fork at the mark's height
- **THEN** the submission fails with `ForkBelowFinalized`
- **AND** the store is unchanged

#### Scenario: New fork one above the mark
- **WHEN** a submission starts a fork at the mark's height plus one
- **THEN** the submission accepts the fork
- **AND** the store holds it

#### Scenario: Stored fork below the mark becomes best
- **WHEN** a heavier stored fork below the mark would become best
- **THEN** the submission fails with `FinalityConflict`
- **AND** the store is unchanged

#### Scenario: Best chain excludes the mark
- **WHEN** the best chain no longer contains the mark
- **THEN** the submission fails with `FinalityConflict`
- **AND** the mark is unchanged

#### Scenario: Checkpoint floor precedence
- **WHEN** a fork is at or below the checkpoint floor
- **THEN** the submission fails with `ForkBelowCheckpoint`
- **AND** not with `ForkBelowFinalized`

### Requirement: [FIN-09] The manual advance
The advance method SHALL set the mark to the block `n` below the best tip for
`Some(n)`, and to the coinbase-maturity depth for `None`. It SHALL skip the bar,
the clock and the competing-fork cap. It SHALL return the previous mark. It
SHALL NOT prune.

#### Scenario: Tip
- **WHEN** the caller passes `Some(0)`
- **THEN** the mark is the best tip

#### Scenario: One block below
- **WHEN** the caller passes `Some(1)`
- **THEN** the mark is the parent of the best tip

#### Scenario: Default depth
- **WHEN** the caller passes `None`
- **THEN** the mark is 100 blocks below the best tip

#### Scenario: Deep manual mark
- **WHEN** the caller passes `Some(1000)` and the best tip is at height 3000
- **THEN** the mark is the block at height 2000

#### Scenario: Depth beyond the tip
- **WHEN** the caller passes a depth greater than the best tip's height
- **THEN** the call fails
- **AND** the mark is unchanged

#### Scenario: Backward move
- **WHEN** the depth puts the mark below its current height
- **THEN** the mark moves backward
- **AND** the call returns the previous mark

#### Scenario: No bar
- **WHEN** the caller passes `Some(0)` and the estimate would reject that block
- **THEN** the mark still moves

#### Scenario: No clock
- **WHEN** the caller passes no clock
- **THEN** the mark still moves

#### Scenario: No pruning
- **WHEN** the mark moves
- **THEN** every stored header stays
- **AND** every stored coinbase stays

### Requirement: [FIN-10] Non-mainnet
On testnet and regtest the automatic policy SHALL require one tip block of work,
so the 100-block depth SHALL decide. The estimate SHALL NOT run and the test
injection SHALL be refused.

#### Scenario: Testnet at the depth
- **WHEN** the tip is 100 blocks above the anchor
- **THEN** the mark does not move

#### Scenario: Testnet one past the depth
- **WHEN** the tip is 101 blocks above the anchor
- **THEN** the mark moves

#### Scenario: Regtest at the depth
- **WHEN** the tip is 100 blocks above the anchor
- **THEN** the mark does not move

#### Scenario: Regtest one past the depth
- **WHEN** the tip is 101 blocks above the anchor
- **THEN** the mark moves

#### Scenario: Injection refused
- **WHEN** a test injects an estimate on testnet or regtest
- **THEN** the configuration is refused

### Requirement: [FIN-11] Consumers
The finalized checkpoints, the coinbase retention floor, the Bitcoin tail work
and the Rootstock anchor check SHALL read the mark's fork and hash. A reorg
above the mark SHALL NOT change them. A reorg below the mark SHALL be a
conflict.

#### Scenario: Finalized checkpoints
- **WHEN** the mark is at height H
- **THEN** the finalized checkpoints are the period starts at or below H on the mark's fork

#### Scenario: Reorg above the mark
- **WHEN** a fork replaces the best tip above the mark
- **THEN** the finalized checkpoints are unchanged

#### Scenario: Non-best mark
- **WHEN** a manual mark sits on a stored non-best fork
- **THEN** the consumers read that fork

#### Scenario: Rootstock range
- **WHEN** the Rootstock anchor check runs
- **THEN** the trusted range is the last finalized checkpoint through the mark

#### Scenario: Retention floor
- **WHEN** the coinbase retention floor is read
- **THEN** it is the last finalized checkpoint at or below the mark

#### Scenario: No prune on move
- **WHEN** the mark moves
- **THEN** the coinbases below the new checkpoint are not removed
- **AND** the separate prune action removes them
