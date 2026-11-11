# Bitcoin header ingestion

Status: draft
Executable form: [`crates/core/specs/bitcoin_headers.rs`](../crates/core/specs/bitcoin_headers.rs)

This document describes the Bitcoin header state machine exposed by
`Core::apply(Action::ConnectHeaders { headers, now })`. The Rust comments use
the same clause IDs.

Design rules use `R*`. Bitcoin header checks use `C*`. Some `C*` clauses contain
local policy; those parts are marked **Local**.

## Scope

In scope:

- accepting or refusing a contiguous run of Bitcoin headers,
- storing accepted headers on the right fork and height,
- canonical-chain selection by cumulative work,
- the finality mark's safety and advancement,
- the `ForkBelowFinalized` gate,
- the Bitcoin header checks that this Core implements.

Out of scope:

- network I/O, peer selection and retries,
- block-body checks: transactions, merkle root and witness commitment,
- coinbase storage, tagging and pruning,
- Rootstock checkpoint policy.

## 1. Inputs

The state machine depends on these inputs:

- **Network.** Selects the Bitcoin consensus parameters. The expected-difficulty
  rule applies on mainnet and signet. Testnet, testnet4 and regtest permit
  minimum-difficulty blocks.
- **Margin.** The candidate mark depth. Production uses
  `FINALITY_MARGIN_BLOCKS` (100). Tests use a small value, often 2.
- **Bar.** A function from the anchor work, anchor time, genesis time and `now`
  to the work the best chain must carry above the mark. The mark advances only
  when `have >= need`.
- **`now`.** The caller's clock. It sets the future-time limit for C7. The
  finality rule also stores the greatest value as a high-water mark. A later
  smaller value cannot reduce the bar. The mark only moves inside a batch that
  stores new headers.
- **Seed.** One or more trusted headers at difficulty-period boundaries. The seed
  may be sparse. A test seed is often just genesis.

## 2. State

The store holds:

- **S1 — stored headers.** Every seed entry and every accepted header, keyed by
  `(fork, height)`. A header on a losing fork is stored.
- **S2 — forks.** Fork `0` is the seed chain. Every other fork has a
  `forkpoint`, a `parent` fork and a contiguous run of headers from the
  forkpoint. A fork's cumulative work is the work stored on its chain.
- **S3 — best fork.** The fork with the most cumulative work among the forks
  that R6 permits to win. A tie keeps the incumbent.
- **S4 — finality mark.** The newest height treated as un-reorgable. Seeding
  sets it to the newest seed height. The candidate is `tip - margin`. The bar
  can hold it lower. It never exceeds the candidate and never regresses.
- **S5 — clock high-water mark.** The greatest `now` seen on a batch that
  stores new headers.

## 3. Action

**A1 — connect a batch.** `connect(headers, now)` delivers a run of headers that
the caller claims are contiguous. The batch may extend a fork, start a new fork
or repeat a leading run of stored headers.

A leading stored prefix is skipped. The next header must name the last prefix
header as its parent. A repeat after a new header is not a no-op; it is a link
error.

## 4. Requirements

### Bitcoin header rules (external)

**C1 — parent.** A new header must name a stored parent. If its parent is not
stored, the action fails with `UnknownAncestor`. Bitcoin Core calls this
`prev-blk-not-found`. Core does not keep invalid previous headers, so
`bad-prevblk` cannot occur here.

**C2 — linkage.** From the first new header, each header must name the previous
header as its parent. A mismatch fails with `LinkBreak`.

**C3 — proof of work.** Each new header must meet its own target. Failure gives
`BadPow`. Bitcoin Core calls this `high-hash`.

**C4 — median-time-past.** Bitcoin requires the timestamp to be greater than the
median of the previous **up to** 11 block times. Failure gives `BadTimestamp`
(Bitcoin Core `time-too-old`).

**Local:** this Core checks C4 only when all 11 previous rows are stored. If a
row is missing, or the header is in the first 11 heights, the check is skipped.

**C5 — expected difficulty.** On a network that enforces the retarget schedule,
the header bits must equal the expected target. Inside a period, the expected
bits are the parent bits. At a period boundary, the retarget calculation gives
the expected bits. Failure gives `BadDifficulty` (Bitcoin Core `bad-diffbits`).

**Local:** this Core skips C5 when the period-start header is not stored. It also
skips C5 on testnet, testnet4 and regtest. Those networks permit
minimum-difficulty blocks.

**C6 — target range.** Bitcoin rejects a target that is negative, overflowing,
zero or above the network proof-of-work limit.

**Local:** this Core does not check the target range. It only checks
`hash <= target`. C5 hides this gap on mainnet and signet while the seed is
valid. The gap remains on testnet, testnet4 and regtest.

**C7 — future time.** Bitcoin rejects a header whose timestamp is more than two
hours after the current time. The failure is `TimestampTooNew` (Bitcoin Core
`time-too-new`).

Validation order in this Core is: link, expected difficulty (when enforced),
median-time-past, future time, then proof of work. This is not Bitcoin Core's
order; Bitcoin Core checks proof of work first.

Known Bitcoin header checks that this Core does not implement:

| Check | Reason |
|---|---|
| Target range (`C6`) | The header target is only compared with the hash. |
| Version rules | Height-activated and no SPV value. |
| Bitcoin Core checkpoint mismatch | Trust comes from the seeded spine. The name refers to older Bitcoin Core. |
| Block-body checks | Not header fields. |
| MTP when fewer than 11 rows are stored | Local skip. See C4. |
| Boundary difficulty when the period start is missing | Local skip. See C5. |

### API and storage (designed here)

**R1 — atomicity.** If the action returns an error, the store is unchanged.

**R2 — idempotence.** If every header in the batch is already stored, the action
succeeds with `connected == 0` and changes nothing.

**R3 — storage exactness.** The stored set is exactly the seed entries plus every
header from every accepted batch. A losing header remains readable through
`stored_headers` and `locate`.

**R4 — canonical chain.** For a chain with no gaps in stored rows, the canonical
chain starts at the seed header, links continuously and ends at the best fork
tip. `canonical_headers` reports an error if a canonical height has no row; a
sparse production seed has such gaps.

**R5 — fork choice by work.** The best fork carries the maximum cumulative work
among the forks that R6 permits to win. Height alone does not decide.

**R6 — switching.** A fork becomes best only if two conditions hold:

1. it is strictly heavier than the old best, and
2. it carries the same block as fork `0` at the mark.

The second condition keeps a heavier fork stored when that fork diverges below
the mark. Equal work keeps the incumbent.

### Finality (designed here)

**R7 — mark safety.** After seeding, the mark never regresses, never exceeds
`tip - margin`, and every height at or below it keeps the block it had when it
was finalized. Seeding itself sets the mark to the newest seed height.

**R8 — mark advancement.** With the seed chain best, if the work above the mark
clears the bar (`have >= need`), the mark advances to `tip - margin` when that is
greater than the current mark.

**R9 — fork gate.** A fork is created only when its forkpoint is above the
finality mark. At or below the mark the action fails with
`ForkBelowFinalized` and changes nothing. Above the mark, R3 and R6 apply.

## 5. How the executable spec is organised

Three reference machines mirror this document:

- `bitcoin_headers_match_the_spec` checks R1–R2, C1–C4 and R3–R6 over a
  delivery universe with a 12-block trunk, a competing fork, a fork of a fork, a
  bad-PoW header, a stale-timestamp header, duplicates and empty batches. The
  mark stays out of reach here, so R7 is only trivial and the R6 mark condition
  cannot fire.
- `finality_and_fork_gate_matches_the_spec` checks R7–R9 on a 120-block trunk
  with a three-work-unit bar and a two-block margin. It also delivers light
  forks below and above the advancing mark. It does not create a heavier fork
  that crosses the mark.
- `difficulty_matches_the_spec` checks C5 at height 1 on signet. It uses a
  synthetic seed and an easy target. It does not test C6 or the boundary
  retarget.
- `future_time_matches_the_spec` checks C7. It delivers a valid child and a
  child one second beyond the two-hour window.

The reference models predict acceptance and the stored set. The test, not
`Core`, checks R5 and R6 as invariants.

## 6. Not covered yet

- A heavier fork that crosses the mark (the R6 condition and the corresponding
  election path).
- MTP with fewer than 11 stored rows, and MTP at a height where an ancestor row
  is missing.
- The target-range and proof-of-work-limit check (C6).
- A valid signet target, and the boundary retarget through `Core`.
- Coinbase and Rootstock actions.
- Fork trees deeper than the fixed universe.

Adding a requirement here should come with either a clause in the Rust file or
an explicit note under “Not covered yet”.
