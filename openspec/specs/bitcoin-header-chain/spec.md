# bitcoin-header-chain Specification

## Purpose
Defines how a batch of Bitcoin headers grows the store's fork tree and which
fork is best. This capability owns the trusted checkpoints and the batch
contract; `bitcoin-header-validation` owns the per-header rules and
`bitcoin-finality` owns the finality mark.

## Requirements

### Requirement: [BTC-00] Trusted checkpoints
Opening an empty store SHALL initialize it with the configured trusted
checkpoints. Each checkpoint SHALL lie at a Bitcoin period boundary, and the
heights SHALL be one period apart; a malformed set SHALL be refused. The root
fork SHALL hold the checkpoints from the first height to the highest. Each
period between adjacent checkpoints SHALL count one period at the lower
checkpoint's `bits`. The highest checkpoint SHALL be the initial finality mark.

#### Scenario: Fresh store
- **WHEN** the core opens an empty store with the configured trusted checkpoints
- **THEN** the root fork holds the checkpoints from the first height to the
  highest
- **AND** the root fork is the best fork

#### Scenario: Initial cumulative work
- **WHEN** the core opens an empty store with two adjacent trusted checkpoints
- **THEN** the root fork's tip work counts one full period at the first
  checkpoint's `bits` plus the second checkpoint's own work

#### Scenario: Malformed checkpoint set
- **WHEN** a configured checkpoint is not at a period boundary, or two
  checkpoints are not one period apart
- **THEN** the open fails and no store is initialized

#### Scenario: Initial finality mark
- **WHEN** the core opens an empty store with the configured trusted checkpoints
- **THEN** the highest checkpoint is the finality mark

### Requirement: [BTC-01] Store identity
Opening a store that already holds a fork tree SHALL compare the configured
network with the stored network. A different network SHALL fail with
`NetworkMismatch`. The open SHALL compare each configured checkpoint with the
stored header at the same height. A different header SHALL fail with
`CheckpointMismatch`. A refused open SHALL change nothing. A successful open
SHALL add no header.

#### Scenario: Reopen
- **WHEN** the core opens the store again with the same network and the same
  checkpoints
- **THEN** the open succeeds and adds no header

#### Scenario: Longer checkpoint set
- **WHEN** the core opens the store with a longer checkpoint set on the same
  chain
- **THEN** the open succeeds and the store keeps its tip

#### Scenario: Different network
- **WHEN** the core opens the store with a different network
- **THEN** the open fails with `NetworkMismatch`
- **AND** the store is unchanged

#### Scenario: Different checkpoint
- **WHEN** a configured checkpoint differs from the stored header at its height
- **THEN** the open fails with `CheckpointMismatch`
- **AND** the store is unchanged

### Requirement: [BTC-02] Atomicity
A submission SHALL run as one transaction. A refused submission SHALL store no
header, create no fork, change no fork metadata and change no best fork.

#### Scenario: Fork then failure
- **WHEN** a submission starts a new fork and a later header fails a rule
- **THEN** the submission fails and stores no header and no fork

#### Scenario: Refused batch
- **WHEN** a submission fails
- **THEN** the stored tree is the same before and after

### Requirement: [BTC-03] Idempotency
A submission SHALL skip the longest leading run of headers that the store
already holds. A stored header is one that the store holds on any fork. A
submission that adds no header SHALL succeed and change nothing.

#### Scenario: Repeated prefix
- **WHEN** a submission repeats a stored run and adds one header
- **THEN** the submission stores the new header and reports one accepted header

#### Scenario: All stored
- **WHEN** a submission carries only stored headers
- **THEN** the submission succeeds, stores nothing and reports zero accepted
  headers

#### Scenario: Empty
- **WHEN** the submission carries no header
- **THEN** the submission succeeds, stores nothing and reports zero accepted
  headers

#### Scenario: Prefix on a losing fork
- **WHEN** a submission starts with a header stored on a fork that is not best
- **THEN** the store skips that header and the new headers extend that fork

### Requirement: [BTC-04] Contiguity
When the submission skips a prefix, the first new header SHALL name the last
skipped header. Every later new header SHALL name the header before it. A link
that does not hold SHALL fail with `LinkBreak` at that height.

#### Scenario: Gap in the new run
- **WHEN** a new header does not name the header before it
- **THEN** the submission fails with `LinkBreak` at that height

#### Scenario: Prefix link
- **WHEN** a submission skips a prefix and the first new header does not name
  the last skipped header
- **THEN** the submission fails with `LinkBreak`

#### Scenario: Stored header after a new header
- **WHEN** the submission carries a new header and then a stored header
- **THEN** the submission fails with `LinkBreak` at the stored header's height

### Requirement: [BTC-05] Forks
Let the parent be the stored block that the first new header names. When the
parent is the tip of its fork, the submission SHALL extend that fork. Otherwise
the submission SHALL start a new fork at the parent's height plus one. The
parent fork SHALL be the fork that stores the parent block. A new fork SHALL
fill contiguous heights from its first height; two forks SHALL not share a
first hash. `bitcoin-finality` can refuse a new fork with `ForkBelowFinalized`.

#### Scenario: Extend the tip
- **WHEN** the first new header names the tip of its fork
- **THEN** the submission extends that fork and creates no fork

#### Scenario: Fork from a non-tip
- **WHEN** the first new header names a stored block that is not its fork's tip
- **THEN** the submission creates a new fork at the parent's height plus one
- **AND** the new fork's parent is the fork that stores the parent block

#### Scenario: Fork of a fork
- **WHEN** the first new header names a non-tip block on a fork other than the
  root fork
- **THEN** the new fork's parent is that fork

#### Scenario: Siblings
- **WHEN** two submissions name the same parent block with different first
  headers
- **THEN** the store holds two forks with the same first height and different
  first hashes

#### Scenario: Distinct first hash
- **WHEN** a submission starts with a fork's first header and diverges at the
  next height
- **THEN** the store skips the first header and starts the new fork at the next
  height
- **AND** no two forks share a first hash

#### Scenario: Below the best tip
- **WHEN** a submission starts at a stored block below the best tip
- **THEN** the submission attaches the new run to the fork that stores the
  parent block

#### Scenario: Contiguous fork
- **WHEN** a fork receives a second submission at its tip
- **THEN** its headers fill contiguous heights from its first height

### Requirement: [BTC-06] Stored set
The submission SHALL store each new header at its height on the target fork. It
SHALL store the cumulative work with each header and at the fork tip. The
cumulative work SHALL be the parent block's cumulative work plus the work of the
new headers. The submission SHALL remove no header. A fork that does not become
best SHALL keep its headers and its metadata.

#### Scenario: Losing fork
- **WHEN** a submission stores headers on a fork that does not become best
- **THEN** the store keeps those headers and the fork metadata

#### Scenario: No removal
- **WHEN** a fork becomes best
- **THEN** the headers of the old best fork stay stored

#### Scenario: Cumulative work
- **WHEN** a submission stores a run above a parent
- **THEN** the fork tip's cumulative work is the parent's cumulative work plus
  the run's work

### Requirement: [BTC-07] Best fork
After the submission stores the new headers, it SHALL compare the target fork
with the best fork. The target fork SHALL become best only when it has more
cumulative work. Equal work SHALL keep the best fork. A fork that the finality
rule does not admit SHALL not become best, and `bitcoin-finality` owns that
rule. The best fork's chain SHALL be the canonical chain. The root fork SHALL be
able to become best again.

#### Scenario: Heavier wins
- **WHEN** a submission stores headers on a fork with more cumulative work than
  the best fork
- **THEN** the store makes that fork best

#### Scenario: Shorter but heavier fork wins
- **WHEN** a submission stores headers on a fork with fewer blocks than the
  best fork but more cumulative work
- **THEN** the store makes that shorter fork best

#### Scenario: Lighter stays
- **WHEN** a submission stores headers on a fork with less cumulative work than
  the best fork
- **THEN** the best fork stays

#### Scenario: Equal keeps
- **WHEN** a submission stores headers on a fork with equal cumulative work
- **THEN** the best fork stays

#### Scenario: Later win
- **WHEN** a stored fork receives more headers and becomes strictly heavier
- **THEN** the store makes it best

#### Scenario: Root wins back
- **WHEN** the root fork becomes strictly heavier than the best fork
- **THEN** the store makes the root fork best

#### Scenario: Canonical
- **WHEN** a fork is best
- **THEN** its chain from the first checkpoint height to its tip is the
  canonical chain

### Requirement: [BTC-08] Outcome
A successful submission SHALL report the number of new headers, the best fork
before the submission, the best fork after the submission, and the canonical tip
height after the submission. A no-op submission SHALL report zero new headers,
the same best fork before and after, and the current canonical tip. A refused
submission SHALL report no outcome. The finality fields belong to
`bitcoin-finality`.

#### Scenario: Extend
- **WHEN** a submission extends the best fork
- **THEN** the outcome reports one accepted header, the same best fork before
  and after, and the new tip height

#### Scenario: Switch
- **WHEN** a submission makes a new fork best
- **THEN** the outcome reports the previous best fork, the new best fork, and
  the new fork's tip height

#### Scenario: No-op
- **WHEN** a submission adds no header
- **THEN** the outcome reports zero accepted headers, the same best fork before
  and after, and the current tip height

#### Scenario: Losing fork
- **WHEN** a submission stores headers on a fork that does not become best
- **THEN** the outcome reports the accepted header count, the same best fork
  before and after, and the canonical tip height
