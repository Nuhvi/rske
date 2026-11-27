# bitcoin-header-validation Specification

## Purpose
Defines which Bitcoin headers the core accepts. The core is a header-only
Rootstock client, and a submission is one batch of headers offered in a call.
The rules stay consistent with Bitcoin consensus and its network parameters,
and each deviation is declared where it occurs. Each requirement is verifiable
as a unit test, and the requirement ID names that test.

## ADDED Requirements

### Requirement: [HDR-00] Supported networks
The core SHALL open a store only for Bitcoin mainnet, testnet3, and regtest.
It SHALL refuse testnet4 and signet with `UnsupportedNetwork` before it creates
a store. This is a simplification to keep only networks that we need for a Rootstock integration.

#### Scenario: Supported network
- **WHEN** the core opens a store for mainnet, testnet3, or regtest
- **THEN** the open succeeds

#### Scenario: Unsupported network
- **WHEN** the core opens a store for testnet4 or signet
- **THEN** the open fails with `UnsupportedNetwork` and creates no store

### Requirement: [HDR-02] Target range
**Source**: Bitcoin Core v29.0 [`DeriveTarget`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/pow.cpp#L146-L159); reject reason `high-hash` is in [`CheckBlockHeader`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/validation.cpp#L3956-L3963).

The compact `bits` SHALL not set its sign bit and SHALL decode to a nonzero
target that fits in 256 bits. The target SHALL be at or below the network
proof-of-work limit, whose compact encoding is the limit bits. The range check
SHALL precede the proof-of-work check. The submission SHALL fail with `BadPow`
and the detail `target out of range` when a condition fails. The detail is
specific to this crate; Bitcoin Core reports `high-hash` for both the range and
the work failure.

#### Scenario: Negative target
- **WHEN** the compact encoding sets its sign bit
- **THEN** the submission fails with `BadPow` and detail `target out of range`

#### Scenario: Zero target
- **WHEN** the decoded target is zero, including a nonzero mantissa that
  truncates to zero (for example `0x01000001`)
- **THEN** the submission fails with `BadPow` and detail `target out of range`

#### Scenario: Overflowing target
- **WHEN** the decoded target overflows 256 bits
- **THEN** the submission fails with `BadPow` and detail `target out of range`

#### Scenario: Target above the network limit
- **WHEN** the decoded target is above the network proof-of-work limit
- **THEN** the submission fails with `BadPow` and detail `target out of range`

#### Scenario: Target equal to the network limit
- **WHEN** the decoded target equals the network proof-of-work limit and the
  header hash meets it
- **THEN** the submission accepts the header

#### Scenario: Range check before proof of work
- **WHEN** the target is above the network proof-of-work limit and the header
  hash is above the target
- **THEN** the submission fails with `BadPow` and detail `target out of range`

### Requirement: [HDR-03] Proof of work
**Source**: Bitcoin Core v29.0 [`CheckBlockHeader`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/validation.cpp#L3956-L3962), reject reason `high-hash`; [`CheckProofOfWorkImpl`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/pow.cpp#L161-L171).

The header hash, the double SHA-256 of the 80-byte header, SHALL be at or
below the target that the header's `bits` decodes to. The submission SHALL fail
with `BadPow` when the header hash is above the target.

#### Scenario: Hash above the target
- **WHEN** an otherwise valid header has a hash above the target in its `bits`
- **THEN** the submission fails with `BadPow`

#### Scenario: Hash at or below the target
- **WHEN** the header hash is at or below the target in its `bits`
- **THEN** the submission accepts the header

### Requirement: [HDR-04] Expected bits inside a period
**Source**: Bitcoin Core v29.0 [`GetNextWorkRequired`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/pow.cpp#L14-L38); the mismatch check and reason `bad-diffbits` are in [`ContextualCheckBlockHeader`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/validation.cpp#L4214-L4215).

The difficulty rules compare 32-bit `bits` values, not decoded targets. A
period boundary is a height that is a multiple of the difficulty period. The
period start of a height is the greatest boundary at or below it. On a network
that does not permit minimum-difficulty blocks, a header at a height that is
not a period boundary SHALL carry the parent's `bits`. The submission SHALL
fail with `BadDifficulty` when the `bits` differ.

#### Scenario: Parent bits
- **WHEN** a header on mainnet at a height that is not a period boundary
  carries the parent's `bits`
- **THEN** the submission accepts the header

#### Scenario: Different bits
- **WHEN** a header on mainnet at a height that is not a period boundary
  carries different `bits` and its hash meets that target
- **THEN** the submission fails with `BadDifficulty`

### Requirement: [HDR-05] Minimum-difficulty rule
**Source**: Bitcoin Core v29.0 [`GetNextWorkRequired`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/pow.cpp#L22-L36), reject reason `bad-diffbits`.

On testnet3 and regtest, a header at a height that is not a period boundary
SHALL carry the limit bits when its timestamp is more than twice the target
spacing after the parent timestamp. Otherwise it SHALL carry the `bits` of the
most recent normal ancestor at or above the period start. When no ancestor in
that range is normal, it SHALL carry the limit bits. Normal `bits` differ from
the limit bits. The submission SHALL fail with `BadDifficulty` when the `bits`
differ.

#### Scenario: Long gap with the limit
- **WHEN** the header timestamp is more than twice the target spacing after
  the parent timestamp, the parent carries normal `bits`, and the header
  carries the limit bits
- **THEN** the submission accepts the header

#### Scenario: Long gap with the parent bits
- **WHEN** the header timestamp is more than twice the target spacing after
  the parent timestamp, the parent carries normal `bits`, and the header
  carries the parent's `bits`
- **THEN** the submission fails with `BadDifficulty`

#### Scenario: Gap equal to twice the spacing
- **WHEN** the header timestamp is exactly twice the target spacing after the
  parent timestamp, the parent carries normal `bits`, and the header carries
  the limit bits
- **THEN** the submission fails with `BadDifficulty`

#### Scenario: Walk back to the last normal header
- **WHEN** the header timestamp is at most twice the target spacing after the
  parent timestamp, the parent carries the limit bits, an earlier ancestor at
  or above the period start carries normal `bits`, and the header carries those
  `bits`
- **THEN** the submission accepts the header

#### Scenario: Walk back skipped
- **WHEN** the header timestamp is at most twice the target spacing after the
  parent timestamp, an earlier ancestor at or above the period start carries
  normal `bits`, the parent carries the limit bits, and the header carries the
  limit bits
- **THEN** the submission fails with `BadDifficulty`

#### Scenario: Walk back through the submission
- **WHEN** the header timestamp is at most twice the target spacing after the
  parent timestamp, the parent carries the limit bits and arrived in the same
  submission after a header with normal `bits`, and the header carries the
  limit bits
- **THEN** the submission fails with `BadDifficulty`

#### Scenario: Walk back stops at the period start
- **WHEN** the header timestamp is at most twice the target spacing after the
  parent timestamp, the period start and every later ancestor carry the limit
  bits, and the header carries the limit bits
- **THEN** the submission accepts the header

### Requirement: [HDR-06] Period-boundary retarget
**Source**: Bitcoin Core v29.0 [`CalculateNextWorkRequired`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/pow.cpp#L50-L85), mismatch reason `bad-diffbits`.

At a period boundary the header SHALL carry the expected bits of the period
that ends at that height. On mainnet and testnet3 the bits encode the parent
target times the clamped timespan, integer-divided by the target timespan,
capped at the network proof-of-work limit. The timespan is the parent time
minus the period-start header time, clamped to one quarter and four times the
target timespan. On regtest the bits are the parent `bits`. A mismatch SHALL
fail with `BadDifficulty`.

#### Scenario: Mainnet quarter-timespan clamp
- **WHEN** a period takes less than one quarter of the target timespan and the
  boundary header carries the one-quarter clamped retarget
- **THEN** the submission accepts the header

#### Scenario: Mainnet exact quarter-timespan clamp
- **WHEN** a period takes exactly one quarter of the target timespan and the
  boundary header carries the one-quarter clamped retarget
- **THEN** the submission accepts the header

#### Scenario: Mainnet four-times-timespan clamp
- **WHEN** a period takes more than four times the target timespan and the
  boundary header carries the four-times clamped retarget
- **THEN** the submission accepts the header

#### Scenario: Mainnet exact four-times-timespan clamp
- **WHEN** a period takes exactly four times the target timespan and the
  boundary header carries the four-times clamped retarget
- **THEN** the submission accepts the header

#### Scenario: Mainnet boundary with the parent bits
- **WHEN** a period takes one quarter of the target timespan or less and the
  boundary header carries the parent's `bits`
- **THEN** the submission fails with `BadDifficulty`

#### Scenario: Unclamped retarget
- **WHEN** a period outside the clamp bounds ends, the clamped retarget is below
  the limit target, and the boundary header carries the unclamped retarget
- **THEN** the submission fails with `BadDifficulty`

#### Scenario: Retarget clamped to the proof-of-work limit
- **WHEN** the retarget is above the limit target and the boundary header
  carries the limit bits
- **THEN** the submission accepts the header

#### Scenario: Retarget equal to the proof-of-work limit
- **WHEN** the retarget equals the limit target and the boundary header carries
  the limit bits
- **THEN** the submission accepts the header

#### Scenario: Testnet3 boundary retarget
- **WHEN** a testnet3 period ends and the boundary header carries the retarget
  from the parent's `bits`, after the clamp
- **THEN** the submission accepts the header

#### Scenario: Testnet3 retarget from a minimum-difficulty parent
- **WHEN** a testnet3 parent at a period boundary carries the limit bits and
  the boundary header carries the retarget from the limit bits
- **THEN** the submission accepts the header

#### Scenario: Regtest boundary
- **WHEN** a regtest boundary header at height 2016 carries the parent's `bits`
- **THEN** the submission accepts the header

#### Scenario: Regtest retarget
- **WHEN** a regtest boundary header carries `bits` that differ from the
  parent's `bits`
- **THEN** the submission fails with `BadDifficulty`

### Requirement: [HDR-07] Median-time-past
**Source**: Bitcoin Core v29.0 [`ContextualCheckBlockHeader`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/validation.cpp#L4229-L4231), reject reason `time-too-old`; [`GetMedianTimePast`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/chain.h#L278-L290).

A header SHALL carry a timestamp above the median of the timestamps of the 11
headers before it. The core SHALL skip the check when it does not hold all 11.
The submission SHALL fail with `BadTimestamp` when the timestamp is at or below
the median. Bitcoin Core holds every header and medians the ancestors it has;
the skip is a declared deviation.

#### Scenario: Timestamp equal to the median
- **WHEN** 11 ancestors are known and the header timestamp is the median of
  those 11 ancestors
- **THEN** the submission fails with `BadTimestamp`

#### Scenario: One second above the median
- **WHEN** 11 ancestors are known and the header timestamp is one second above
  the median
- **THEN** the submission accepts the header

#### Scenario: Window excludes the new header
- **WHEN** the header timestamp is above the median of its previous 11
  ancestors but at or below the median of its previous 10 ancestors and itself
- **THEN** the submission accepts the header

#### Scenario: Sparse window
- **WHEN** 10 ancestors are known and the header timestamp is at or below the
  median of those 10 ancestors
- **THEN** the submission accepts the header

#### Scenario: Ancestors from the submission
- **WHEN** the submission carries the 11 ancestors before the new header in the
  same call and the new header timestamp is their median
- **THEN** the submission fails with `BadTimestamp`

#### Scenario: Older headers outside the window
- **WHEN** 12 ancestors are known and the header timestamp is above the median
  of its previous 11 ancestors but at or below the median of its previous 12
  ancestors
- **THEN** the submission accepts the header

### Requirement: [HDR-08] Future time
**Source**: Bitcoin Core v29.0 [`ContextualCheckBlockHeader`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/validation.cpp#L4245-L4247), reject reason `time-too-new`; [`MAX_FUTURE_BLOCK_TIME`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/chain.h#L29).

When the caller supplies a clock, a header SHALL carry a timestamp at most two
hours (7200 s) after the clock time. The submission SHALL fail with
`TimestampTooNew` when the timestamp is later. The core SHALL skip the check
when the caller supplies no clock. Bitcoin Core always compares against the
node clock; the skip is a declared deviation.

#### Scenario: Exactly two hours ahead
- **WHEN** the header timestamp equals the clock time plus 7200 seconds
- **THEN** the submission accepts the header

#### Scenario: More than two hours ahead
- **WHEN** the header timestamp equals the clock time plus 7201 seconds
- **THEN** the submission fails with `TimestampTooNew`

#### Scenario: No clock
- **WHEN** the caller supplies no clock and the header timestamp is far in the
  future
- **THEN** the submission accepts the header

### Requirement: [HDR-09] Version (declared deviation)
**Source**: Bitcoin Core v29.0 [`ContextualCheckBlockHeader`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/validation.cpp#L4250-L4256), reject reason `bad-version`.

The core SHALL not check the header version. The submission SHALL accept a
header of any version at any height that passes the other rules. This is a
declared deviation: Bitcoin Core requires version 2, 3 and 4 at the mainnet
heights 227931, 363725 and 388381. The version is part of the header hash, so
an old-version header gains no work.

#### Scenario: Version 1 at a high height
- **WHEN** a version-1 header on mainnet is at or above height 388381
- **THEN** the submission accepts the header

### Requirement: [HDR-10] Network parameters
**Source**: Bitcoin Core v29.0 [`kernel/chainparams.cpp`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/kernel/chainparams.cpp).

The difficulty period, target spacing, target timespan and limit bits SHALL be:

| Network | Period | Spacing | Timespan | Limit bits |
|---|---|---|---|---|
| mainnet, testnet3 | 2016 | 600 s | 1209600 s | 0x1d00ffff |
| regtest | 2016 | 600 s | 1209600 s | 0x207fffff |

Bitcoin Core uses a 144-block period and a one-day timespan on regtest; this
crate uses 2016 and 1209600 s. Regtest does not retarget.

#### Scenario: Regtest period
- **WHEN** a regtest parent at height 287 carries normal `bits` and the header
  at height 288 follows a gap of more than 1200 s and carries the limit bits
- **THEN** the submission accepts the header
