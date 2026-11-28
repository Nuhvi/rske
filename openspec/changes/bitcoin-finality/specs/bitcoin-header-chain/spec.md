# Spec Delta

## MODIFIED Requirements

### Requirement: [BTC-03] Checkpoint floor
The checkpoint floor SHALL be the height of the highest trusted entry that the
store holds. A submission SHALL refuse a new fork whose forkpoint is at or
below the floor with `ForkBelowCheckpoint`. A submission SHALL extend a stored
fork without this test. A fork above the floor SHALL pass this rule. Only the
finality mark rule in `bitcoin-finality` SHALL refuse a fork above the floor.

#### Scenario: Fork below the floor
- **WHEN** a submission names a non-tip block on an entry below the highest
  entry, and the new header starts at that entry's height plus one
- **THEN** the submission fails with `ForkBelowCheckpoint` and names the
  forkpoint and the floor
- **AND** the store is unchanged

#### Scenario: First fork above the floor
- **WHEN** the highest entry is not a tip, the finality mark is at the floor,
  and a submission names the entry with a different first header, so the
  forkpoint is the floor plus one
- **THEN** the submission creates the fork and accepts the header

#### Scenario: Extension skips the floor
- **WHEN** a submission names the tip of a stored fork
- **THEN** the submission extends that fork and creates no fork, whatever the
  floor is

#### Scenario: Floor at genesis
- **WHEN** the set holds genesis only and a submission starts at height 1
- **THEN** the submission extends the stored fork and accepts the header

#### Scenario: First header above the floor
- **WHEN** the header at the first height above the floor carries the floor
  entry's bits on a network without minimum-difficulty blocks and valid proof
  of work
- **THEN** the submission accepts the header

#### Scenario: Testnet first header after a long gap
- **WHEN** the finality mark is at the floor, and the first header above a
  testnet floor comes more than twice the target spacing after the floor entry
  and carries the limit bits
- **THEN** the submission accepts the header

### Requirement: [BTC-09] Best fork
After it stores the rows, a submission SHALL make the target fork best only
when it has strictly more cumulative work than the best fork and the finality
mark does not forbid the change. Equal work SHALL keep the best fork. No stored
fork SHALL have more work than the best fork. The best fork's chain SHALL be the
canonical chain.

#### Scenario: Heavier wins
- **WHEN** a submission stores headers on a fork with more cumulative work
  than the best fork
- **THEN** the store makes that fork best

#### Scenario: Shorter but heavier
- **WHEN** a submission stores headers on a fork with fewer blocks than the
  best fork but more cumulative work
- **THEN** the store makes that shorter fork best

#### Scenario: Lighter stays
- **WHEN** a submission stores headers on a fork with less cumulative work
  than the best fork
- **THEN** the best fork stays

#### Scenario: Equal keeps
- **WHEN** a submission stores headers on a fork with equal cumulative work
- **THEN** the best fork stays

#### Scenario: Later win
- **WHEN** a stored fork receives more headers and becomes strictly heavier
- **THEN** the store makes it best

#### Scenario: Reorg back
- **WHEN** the fork that lost best status becomes strictly heavier again
- **THEN** the store makes it best and its chain is canonical again

#### Scenario: Deep reorg above the floor
- **WHEN** a fork starts at the first height above the floor, the fork is above
  the finality mark, and it becomes strictly heavier than the best fork
- **THEN** the store makes it best and its chain becomes canonical, although
  every canonical height above the floor moved

#### Scenario: Canonical
- **WHEN** a fork is best
- **THEN** its rows from the first entry height to its tip are the canonical
  chain

#### Scenario: Two ancestor forks
- **WHEN** a fork of a fork becomes best
- **THEN** the canonical chain resolves through both ancestor forks from the
  first entry to its tip
