# bitcoin-header-chain Specification

## Purpose

Defines the Bitcoin header contract above the store: the trusted checkpoint
set adopted at open, the checkpoint floor and its prune, the batch contract,
the fork tree, the stored set, the best fork and the submission outcome.
Per-header rules belong to `bitcoin-header-validation`. The difficulty period
`P` is the value HDR-10 gives for the network: 2016 on this crate's mainnet,
testnet3 and regtest. Bitcoin Core uses 144 on regtest; HDR-10 records that
deviation.

## Requirements

### Requirement: [BTC-00] Trusted checkpoints
**Source**: Bitcoin Core v29.0 [`GetNextWorkRequired`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/pow.cpp#L14-L38); the difficulty stays constant inside a difficulty period.

The checkpoint set SHALL be fixed at each open, and a submission SHALL never
extend it. The set SHALL hold exactly one header for each difficulty period,
from genesis: the period's first header. Entry zero SHALL be the genesis
block, at height 0. Entry `k` SHALL be at height `P * k`. A period SHALL
count one full period of work at its entry's bits. Opening an empty store
SHALL store the headers with their work and make that fork best.

#### Scenario: Set shape
- **WHEN** the core opens with a well-formed set of three headers on a network
  with `P` = 2016
- **THEN** the store holds them at heights 0, 2016 and 4032
- **AND** the fork that holds them is the best fork

#### Scenario: Contiguous periods
- **WHEN** the core opens with a set of five headers on a network with
  `P` = 2016
- **THEN** the store holds exactly five headers, at heights 0, 2016, 4032,
  6048 and 8064

#### Scenario: Entry work
- **WHEN** the set holds genesis and the first header of period one
- **THEN** the genesis entry counts one block of work
- **AND** the second entry counts one full period at the genesis bits plus its
  own work

#### Scenario: Period work
- **WHEN** the set holds four entries
- **THEN** the fourth entry counts one full period at the bits of period zero,
  period one and period two, plus its own work

#### Scenario: Sparse region
- **WHEN** the store holds a set with two entries or more
- **THEN** a height between two entries holds no stored header

### Requirement: [BTC-01] Checkpoint validity
The set SHALL fail with `CheckpointFormat` before any write when entry zero is
not the genesis block, or when the set holds no header. The entries are
trusted: the core SHALL NOT apply the per-header rules to them.

#### Scenario: First entry is not genesis
- **WHEN** the first entry holds a header that is not the network genesis
- **THEN** the open fails with `CheckpointFormat` and the store holds no chain

#### Scenario: Empty set
- **WHEN** the set holds no header
- **THEN** the open fails with `CheckpointFormat` and the store holds no chain

### Requirement: [BTC-02] Store identity
Opening a store that already holds a chain SHALL compare the network with the
configuration and SHALL fail with `NetworkMismatch` on a difference. It SHALL
compare each configured entry at or below the stored tip with the stored
header at its height and SHALL fail with `CheckpointMismatch` on a difference.
It SHALL adopt the matching entries; an entry above the tip SHALL be ignored.
A refused open SHALL change nothing, and a successful open SHALL add no
header.

#### Scenario: Reopen
- **WHEN** the core opens the store again with the same network and the same set
- **THEN** the open succeeds and adds no header

#### Scenario: Longer set
- **WHEN** the core opens the store with a longer set on the same chain
- **THEN** the open adopts the entries, the floor rises to the highest entry,
  the tip stays, and it adds no header

#### Scenario: Shorter set
- **WHEN** the core opens the store with a shorter set on the same chain
- **THEN** the open succeeds, the store keeps its floor and tip, and it adds
  and removes nothing

#### Scenario: Entry above the tip
- **WHEN** a configured entry sits above the stored tip
- **THEN** the open succeeds, the entry is ignored, and the floor stays

#### Scenario: Different network
- **WHEN** the core opens the store with a different network
- **THEN** the open fails with `NetworkMismatch`
- **AND** the store is unchanged

#### Scenario: Different checkpoint
- **WHEN** a configured entry differs from the stored header at its height
- **THEN** the open fails with `CheckpointMismatch` and names the height, the
  stored hash and the configured hash
- **AND** the store is unchanged

### Requirement: [BTC-03] Checkpoint floor
The checkpoint floor SHALL be the height of the highest trusted entry that the
store holds. A submission SHALL refuse a new fork whose forkpoint is at or
below the floor with `ForkBelowCheckpoint`. A submission SHALL extend a stored
fork without this test. A fork above the floor SHALL pass this rule, and no
other depth rule SHALL refuse a fork.

#### Scenario: Fork below the floor
- **WHEN** a submission names a non-tip block on an entry below the highest
  entry, and the new header starts at that entry's height plus one
- **THEN** the submission fails with `ForkBelowCheckpoint` and names the
  forkpoint and the floor
- **AND** the store is unchanged

#### Scenario: First fork above the floor
- **WHEN** the highest entry is not a tip and a submission names it with a
  different first header, so the forkpoint is the floor plus one
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
- **WHEN** the first header above a testnet floor comes more than twice the
  target spacing after the floor entry and carries the limit bits
- **THEN** the submission accepts the header

### Requirement: [BTC-04] Atomicity
A submission SHALL run as one transaction. A refused submission SHALL store no
header, create no fork, change no fork metadata and change no best fork. A
submission on a store initialized for another network SHALL fail with
`NetworkMismatch` and change nothing. A submission SHALL apply the per-header
rules to each new header; a header that fails one SHALL stop it. The caller's
clock SHALL feed only the per-header future-time rule.

#### Scenario: First header fails
- **WHEN** the first new header fails a per-header rule
- **THEN** the submission fails and stores nothing

#### Scenario: Fork then failure
- **WHEN** a submission starts a new fork and a later header fails a rule
- **THEN** the submission fails and stores no header and no fork

#### Scenario: Refused batch
- **WHEN** a submission fails
- **THEN** the stored tree, the best fork and the canonical tip are the same
  before and after

#### Scenario: Store of another network
- **WHEN** a submission runs on a store that the configuration disagrees with
- **THEN** it fails with `NetworkMismatch` and stores no header

### Requirement: [BTC-05] Idempotency
A submission SHALL skip the longest leading run of headers that the store
holds on any fork. A submission that adds no header SHALL succeed, change
nothing and report zero accepted headers.

#### Scenario: Repeated prefix
- **WHEN** a submission repeats a stored run and adds one header
- **THEN** the submission stores the new header and reports one accepted
  header

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
- **THEN** the store skips the stored prefix and the new header attaches to the
  last skipped header

### Requirement: [BTC-06] Contiguity
The new headers SHALL form one contiguous run. The first new header SHALL name
the last skipped header, or the parent of the run when the submission skips
nothing. Each later new header SHALL name the header before it. A failed link
SHALL stop the submission with `LinkBreak` at that height.

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

### Requirement: [BTC-07] Forks
The first new header SHALL name a stored parent. A parent that no fork holds
SHALL stop the submission with `UnknownAncestor`. A submission SHALL extend a
fork when the parent is its tip. Otherwise it SHALL create a fork at the
parent's height plus one, with the fork that stores the parent as its parent.
A fork's rows SHALL fill contiguous heights from its forkpoint. Two forks
SHALL not share a first hash.

#### Scenario: Unknown parent
- **WHEN** the first new header names a block that no fork holds
- **THEN** the submission fails with `UnknownAncestor` and names the hash
- **AND** the store is unchanged

#### Scenario: Extend the tip
- **WHEN** the first new header names the tip of its fork
- **THEN** the submission extends that fork and creates no fork

#### Scenario: Fork from a non-tip
- **WHEN** the first new header names a stored block that is not its fork's
  tip
- **THEN** the submission creates a fork at the parent's height plus one
- **AND** the new fork's parent is the fork that stores the parent block

#### Scenario: Fork of a fork
- **WHEN** the first new header names a non-tip block on a fork that an
  earlier submission created
- **THEN** the new fork's parent is that fork

#### Scenario: Siblings
- **WHEN** two submissions name the same non-tip block with different first
  headers
- **THEN** the store holds two forks with the same first height and different
  first hashes

#### Scenario: Distinct first hash
- **WHEN** a submission starts with a fork's first header and diverges at the
  next height
- **THEN** the store skips the first header and starts a new fork at the next
  height
- **AND** no two forks share a first hash

#### Scenario: Below the best tip
- **WHEN** a submission starts at a stored block below the best tip
- **THEN** the submission attaches the new run to the fork that stores the
  parent block

#### Scenario: Contiguous fork
- **WHEN** a fork receives a second submission at its tip
- **THEN** its headers fill contiguous heights from its first height

### Requirement: [BTC-08] Stored set
The submission SHALL store each new header at its height on the target fork,
with its cumulative work. The work SHALL be the parent's work plus the work of
the new headers. A submission SHALL remove no header. A fork that does not
become best SHALL keep its rows and its metadata.

#### Scenario: Cumulative work
- **WHEN** a submission stores a run above a parent
- **THEN** the fork tip's cumulative work is the parent's work plus the work
  of the run

#### Scenario: Losing fork
- **WHEN** a submission stores headers on a fork that does not become best
- **THEN** the store keeps those headers and the fork metadata

#### Scenario: No removal
- **WHEN** a fork becomes best
- **THEN** the headers of the old best fork stay stored

### Requirement: [BTC-09] Best fork
After it stores the rows, a submission SHALL make the target fork best only
when it has strictly more cumulative work than the best fork. Equal work SHALL
keep the best fork. No stored fork SHALL have more work than the best fork.
The best fork's chain SHALL be the canonical chain.

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
- **WHEN** a fork starts at the first height above the floor and becomes
  strictly heavier than the best fork
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

### Requirement: [BTC-10] Outcome
A successful submission SHALL report the number of new headers, the best fork
before it, the best fork after it and the canonical tip height after it. A
submission that adds no header SHALL report zero accepted headers, the same
best fork twice and the current tip. A refused submission SHALL return an
error and no outcome.

#### Scenario: Extend
- **WHEN** a submission extends the best fork
- **THEN** the outcome reports one accepted header, the same best fork before
  and after, and the new tip height

#### Scenario: Switch
- **WHEN** a submission makes a new fork best
- **THEN** the outcome reports the previous best fork, the new best fork and
  the new fork's tip height

#### Scenario: No-op
- **WHEN** a submission adds no header
- **THEN** the outcome reports zero accepted headers, the same best fork
  before and after, and the current tip height

#### Scenario: Losing fork
- **WHEN** a submission stores headers on a fork that does not become best
- **THEN** the outcome reports the accepted header count, the same best fork
  before and after, and the canonical tip height

### Requirement: [BTC-11] Prune to the floor
**Source**: Electrum [`BlockchainManager._instantiate_chain`](https://github.com/spesmilo/electrum/blob/master/electrum/blockchain.py#L152-L185); a chain at or below the last checkpoint is deleted.

After the comparison, an open SHALL remove every fork whose chain does not
carry the highest entry, every header row on such a fork, and every header of
the initial chain below that entry that is not an entry. It SHALL make the
heaviest surviving fork best. A matching header of the initial chain below the
entry SHALL be removed as well. The adopt and prune SHALL run in one
transaction.

#### Scenario: Higher checkpoint
- **WHEN** the open adopts an entry above the tip of the best fork
- **THEN** the store removes that fork and its rows
- **AND** the heaviest surviving fork is best

#### Scenario: Fork above the entry stays
- **WHEN** a stored fork carries the highest adopted entry
- **THEN** the store keeps the fork and its rows

#### Scenario: Matching header pruned
- **WHEN** the open adopts a higher entry
- **THEN** a stored header below it that is not an entry is removed, even when
  it matches the trusted chain

#### Scenario: Tip stays
- **WHEN** the open adopts a higher entry below the tip
- **THEN** every canonical header above the entry stays stored

#### Scenario: Entry at the tip
- **WHEN** the open adopts an entry at the stored tip
- **THEN** the headers below it are pruned and the entry is the canonical tip
