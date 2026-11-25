# bitcoin-header-validation Specification

## Purpose
Defines which Bitcoin headers a header-only Rootstock client accepts, and the
ancestors each rule reads. Ancestors are the headers on the target fork,
including earlier headers in the same submission. Only mainnet, testnet3 and
regtest are supported; a period boundary is a height that is a multiple of
2016.

## Requirements

### Requirement: [HDR-00] Supported networks
The core SHALL open on Bitcoin mainnet, testnet3, and regtest. It SHALL refuse
testnet4 and signet with `UnsupportedNetwork`, before it creates a store or
initializes one with the configured trusted checkpoints. This is local
policy, not a Bitcoin Core rule.

#### Scenario: Supported network
- **WHEN** the core opens with mainnet, testnet3, or regtest
- **THEN** the store opens

#### Scenario: Unsupported network
- **WHEN** the core opens with testnet4 or signet
- **THEN** the open fails with `UnsupportedNetwork` and creates no store

### Requirement: [HDR-01] Parent of the first new header is known
**Source**: Bitcoin Core v29.0 [`AcceptBlockHeader`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/validation.cpp#L4349-L4354),
reject reason `prev-blk-not-found`.

The first new header in a submission SHALL name a stored parent. When the
parent is not stored, the submission SHALL fail with `UnknownAncestor`.

#### Scenario: Unknown parent
- **WHEN** the first new header names a parent that is not stored
- **THEN** the submission fails with `UnknownAncestor`

#### Scenario: Stored parent
- **WHEN** an otherwise valid header names a stored parent
- **THEN** the submission accepts the header

### Requirement: [HDR-02] Target range
**Source**: Bitcoin Core v29.0 [`DeriveTarget`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/pow.cpp#L146-L159);
the range rejection surfaces as `high-hash`.

A header SHALL carry a target inside the network range. The target SHALL be
positive, non-zero, non-overflowing, and at or below the network proof-of-work
limit. A target outside the range SHALL fail with `BadPow` and the detail
`target out of range`.

#### Scenario: Negative target
- **WHEN** the target has the sign bit set
- **THEN** the submission fails with `BadPow` and the detail `target out of
  range`

#### Scenario: Zero target
- **WHEN** the target mantissa is zero
- **THEN** the submission fails with `BadPow` and the detail `target out of
  range`

#### Scenario: Overflowing target
- **WHEN** the target exponent overflows 256 bits
- **THEN** the submission fails with `BadPow` and the detail `target out of
  range`

#### Scenario: Target above the network limit
- **WHEN** the target is above the network proof-of-work limit
- **THEN** the submission fails with `BadPow` and the detail `target out of
  range`

#### Scenario: Range check before proof of work
- **WHEN** the target is above the network limit and the header hash is above
  the target
- **THEN** the failure reports `target out of range`

### Requirement: [HDR-03] Proof of work
**Source**: Bitcoin Core v29.0 [`CheckBlockHeader`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/validation.cpp#L3956-L3962),
reject reason `high-hash`; [`CheckProofOfWorkImpl`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/pow.cpp#L161-L170).

The double-SHA256 hash of a header SHALL be at or below the target encoded in
its `bits`. A header hash above its target SHALL fail with `BadPow`.

#### Scenario: Hash above the target
- **WHEN** an otherwise valid header has a hash above the target in its `bits`
- **THEN** the submission fails with `BadPow`

#### Scenario: Hash at or below the target
- **WHEN** the header hash is at or below the target in its `bits`
- **THEN** the submission accepts the header

### Requirement: [HDR-04] Expected bits inside a period
**Source**: Bitcoin Core v29.0 [`GetNextWorkRequired`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/pow.cpp#L14-L38);
the mismatch check and reason `bad-diffbits` are in [`ContextualCheckBlockHeader`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/validation.cpp#L4214-L4215).

On a network that does not permit minimum-difficulty blocks (mainnet), a
header outside a period boundary SHALL carry the parent's `bits`. Another value
SHALL fail with `BadDifficulty`.

#### Scenario: Parent bits
- **WHEN** a mainnet header inside a difficulty period carries the parent's
  `bits`
- **THEN** the submission accepts the header

#### Scenario: Different bits
- **WHEN** a mainnet header inside a difficulty period carries other `bits` and
  its hash meets that target
- **THEN** the submission fails with `BadDifficulty`

### Requirement: [HDR-05] Minimum-difficulty networks
**Source**: Bitcoin Core v29.0 [`GetNextWorkRequired`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/pow.cpp#L22-L38), reject reason `bad-diffbits`.

On testnet3 and regtest, a header outside a period boundary SHALL carry the
proof-of-work limit when its time is more than twice the target spacing after
its parent. Otherwise it SHALL carry the last normal `bits` at or above the
period start, or the limit when none is normal; normal means `bits` differ from
the limit. A mismatch SHALL fail with `BadDifficulty`.

#### Scenario: Long gap with the limit
- **WHEN** the gap after the parent exceeds twice the target spacing and the
  header carries the proof-of-work limit
- **THEN** the submission accepts the header

#### Scenario: Long gap with the parent bits
- **WHEN** the gap after the parent exceeds twice the target spacing and the
  header carries the parent's `bits`
- **THEN** the submission fails with `BadDifficulty`

#### Scenario: Gap equal to twice the spacing
- **WHEN** the gap equals twice the target spacing and the header carries the
  proof-of-work limit
- **THEN** the submission fails with `BadDifficulty`

#### Scenario: Walk back to the last normal header
- **WHEN** the parent carries the proof-of-work limit and the header carries the
  last normal `bits`
- **THEN** the submission accepts the header

#### Scenario: Walk back skipped
- **WHEN** the parent carries the proof-of-work limit and the header carries the
  proof-of-work limit
- **THEN** the submission fails with `BadDifficulty`

#### Scenario: Walk back through the submission
- **WHEN** the parent carries the proof-of-work limit and arrived earlier in the
  same submission, and the header carries the proof-of-work limit
- **THEN** the submission fails with `BadDifficulty`

#### Scenario: Walk back stops at the period start
- **WHEN** the period start and every later ancestor carry the proof-of-work
  limit, and the header carries the proof-of-work limit
- **THEN** the submission accepts the header

### Requirement: [HDR-06] Period-boundary retarget
**Source**: Bitcoin Core v29.0 [`CalculateNextWorkRequired`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/pow.cpp#L50-L85),
mismatch reason `bad-diffbits`.

At a period boundary the header SHALL carry the retarget of the previous
period. The period start is 2016 heights below the boundary. The base SHALL be
the parent's `bits` on mainnet and testnet3. The timespan from the period start
to the parent SHALL be clamped to one quarter and four times the target
timespan. Regtest SHALL keep the parent's `bits`. The result SHALL not exceed
the network proof-of-work limit. A mismatch SHALL fail with `BadDifficulty`.

#### Scenario: Mainnet quarter-timespan clamp
- **WHEN** a period takes one quarter of the target timespan or less and the
  boundary header carries the one-quarter clamped retarget
- **THEN** the submission accepts the header

#### Scenario: Mainnet four-times-timespan clamp
- **WHEN** a period takes more than four times the target timespan and the
  boundary header carries the four-times clamped retarget
- **THEN** the submission accepts the header

#### Scenario: Mainnet boundary with the parent bits
- **WHEN** a quarter-timespan period ends and the boundary header carries the
  parent's `bits`
- **THEN** the submission fails with `BadDifficulty`

#### Scenario: Unclamped retarget
- **WHEN** a period outside the clamp bounds ends and the boundary header
  carries the unclamped retarget
- **THEN** the submission fails with `BadDifficulty`

#### Scenario: Retarget clamped to the proof-of-work limit
- **WHEN** the retarget exceeds the network proof-of-work limit and the
  boundary header carries the proof-of-work limit
- **THEN** the submission accepts the header

#### Scenario: Testnet3 boundary retarget
- **WHEN** a testnet3 period ends and the boundary header carries the retarget
  from the parent's `bits`
- **THEN** the submission accepts the header

#### Scenario: Regtest boundary
- **WHEN** a regtest boundary header carries the parent's `bits`
- **THEN** the submission accepts the header

#### Scenario: Regtest retarget
- **WHEN** a regtest boundary header carries a recomputed retarget
- **THEN** the submission fails with `BadDifficulty`

### Requirement: [HDR-07] Median-time-past
**Source**: Bitcoin Core v29.0 [`ContextualCheckBlockHeader`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/validation.cpp#L4229-L4231), reject reason `time-too-old`; [`GetMedianTimePast`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/chain.h#L278-L290).

A new header SHALL carry a timestamp above the median of its previous 11 header
times. The median SHALL exclude the new header. When fewer than 11 ancestors
are available, the check SHALL be skipped. A timestamp at or below the median
SHALL fail with `BadTimestamp`.

#### Scenario: Timestamp equal to the median
- **WHEN** 11 ancestors are available and the new header carries the median of
  those 11 ancestors
- **THEN** the submission fails with `BadTimestamp`

#### Scenario: One second above the median
- **WHEN** 11 ancestors are available and the new header carries the median
  plus one second
- **THEN** the submission accepts the header

#### Scenario: Window excludes the new header
- **WHEN** the new header time is above the previous-11 median but equal to the
  median of a window that includes it
- **THEN** the submission accepts the header

#### Scenario: Sparse window
- **WHEN** 10 or fewer ancestors are available and the header is otherwise
  valid
- **THEN** the submission does not apply the median-time-past check

#### Scenario: Ancestors from the submission
- **WHEN** the submission carries the 11 ancestors before the new header in the
  same call and the new header carries their median
- **THEN** the submission fails with `BadTimestamp`

### Requirement: [HDR-08] Future time
**Source**: Bitcoin Core v29.0 [`ContextualCheckBlockHeader`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/validation.cpp#L4245-L4247), reject reason `time-too-new`; [`MAX_FUTURE_BLOCK_TIME`](https://github.com/bitcoin/bitcoin/blob/v29.0/src/chain.h#L29).

When the caller supplies a clock, a header SHALL carry a timestamp at most two
hours after it. A later timestamp SHALL fail with `TimestampTooNew`. Without a
clock, the check SHALL be skipped.

#### Scenario: Exactly two hours ahead
- **WHEN** the header time equals the clock plus 7200 seconds
- **THEN** the submission accepts the header

#### Scenario: More than two hours ahead
- **WHEN** the header time equals the clock plus 7201 seconds
- **THEN** the submission fails with `TimestampTooNew`

#### Scenario: No clock
- **WHEN** the caller supplies no clock and the header time is far in the future
- **THEN** the submission accepts the header
