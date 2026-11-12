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
- refusing a new fork that would start at or below the finality mark,
- Bitcoin headers consensus rules.

Out of scope:

- network I/O, peer selection and retries,
- block-body checks: transactions, merkle root and witness commitment,
- coinbase storage, tagging and pruning,
- Rootstock checkpoint policy.

## 1. Configuration

The machine is configured, once, with:

- **Network.** Selects the Bitcoin consensus parameters. The expected-difficulty
  rule applies on mainnet and signet. Testnet, testnet4 and regtest permit
  minimum-difficulty blocks.
- **Partition safety.** Whether the mainnet network-partition safety rule runs.
  The default is on. A test disables it to run the mainnet consensus rules over
  a low-work synthetic chain (R8).

The network selects the default requirement: mainnet prices elapsed time from
the anchor and floors the result at a coinbase-maturity window of work. Every
other network uses the tip block's own work, which the tip always carries above
the mark, so finality there is decided by the depth alone. With partition
safety disabled, every network uses the tip-work rule.

The machine also starts from a trust anchor:

- **Seed.** One or more trusted headers at difficulty-period boundaries. The seed
  may be sparse, and its first height may be above 0. The crate fixes the seed:
  the embedded spine on mainnet, genesis on every other network. A test may
  replace it; production code cannot. Seeding creates fork `0` and sets the
  first finality mark (S2, S4). The canonical chain starts at the first seed
  header.
- **Seed agreement.** A store that already holds a checkpoint at a configured
  height must hold the same header. Opening a store against a seed that
  disagrees fails with `AnchorMismatch`. Opening never extends an existing
  store, so a configured checkpoint the store does not hold is not planted.

## 2. State

The state machine tracks:

- **S1 — stored headers.** Every seed entry and every accepted header, keyed by
  fork and height. A header on a losing fork is stored.
- **S2 — forks.** Fork `0` is the seed chain. Every other fork has a
  `forkpoint`, the height of its first header, and a contiguous run of headers
  from that height. A fork's cumulative work is the work stored on its chain.
- **S3 — best fork.** The fork with the most cumulative work among the forks
  that R6 permits to win. A tie keeps the incumbent.
- **S4 — finality mark.** The newest height treated as un-reorgable. Seeding
  sets it to the newest seed height. The mark is kept at least one coinbase
  maturity window (100 blocks) below the tip: the candidate is `tip - 100`. The
  requirement can hold it lower. It never exceeds the candidate and never
  regresses.
- **S5 — clock high-water mark.** The greatest clock value used by a batch that
  stores new headers.

## 3. Action

**A1 — connect a batch.** One action delivers:

- **Headers.** A run that the caller claims is contiguous.
- **`now`.** The caller's clock, or none. C7 uses it when supplied and is
  skipped when it is not. Network-partition safety uses the clock floor (R10):
  the greatest of the supplied value, the stored high-water mark (S5) and the
  chain time at the best tip — the median of its last 11 header times, or the
  tip's own time when those are not all stored — so a missing clock measures
  finality from the chain's own time and a stale one cannot reduce the
  requirement. Within a batch, the finality mark moves only if that batch
  stores a new header; a later `now` alone never advances it.

The batch may extend a fork, start a new fork or repeat a leading run of stored
headers. A repeated prefix is skipped; the next header must name the last prefix
header as its parent. A repeat after a new header is not a no-op; it is a link
error.

## 4. Requirements

### Bitcoin header rules (external)

**C1 — parent.** A new header must name a stored parent. If its parent is not
stored, the action fails with `UnknownAncestor`. Bitcoin Core calls this
`prev-blk-not-found`. Bitcoin Core does not keep invalid previous headers, so
`bad-prevblk` cannot arise.

**C2 — linkage.** From the first new header, each header must name the previous
header as its parent. A mismatch fails with `LinkBreak`.

**C3 — proof of work.** Each new header must meet its own target. Failure gives
`BadPow`. Bitcoin Core calls this `high-hash`.

**C4 — median-time-past.** Bitcoin requires the timestamp to be greater than the
median of the previous **up to** 11 block times. Failure gives `BadTimestamp`
(Bitcoin Core `time-too-old`).

**Local:** the check applies only when all 11 previous headers are stored. If one
is missing, or the header is in the first 11 heights, the check is skipped.

**C5 — expected difficulty.** On a network that enforces the retarget schedule,
the header bits must equal the expected target. Inside a period, the expected
bits are the parent bits. At a period boundary, the retarget calculation gives
the expected bits. Failure gives `BadDifficulty` (Bitcoin Core `bad-diffbits`).

**Local:** the check is skipped when the period-start header is not stored. It is
also skipped on testnet, testnet4 and regtest. Those networks permit
minimum-difficulty blocks.

**C6 — target range.** Bitcoin rejects a target that is negative, overflowing,
zero or above the network proof-of-work limit. The same range check runs before
proof of work. A failure gives `BadPow` with `target out of range`.

**C7 — future time.** Bitcoin rejects a header whose timestamp is more than two
hours after the current time. The failure is `TimestampTooNew` (Bitcoin Core
`time-too-new`).

**Local:** the check applies only when the caller supplies a clock. Without one
there is no frame of reference for it, so it is skipped.

When more than one check would fail, the reported order is: link, expected
difficulty (when enforced), median-time-past, future time, then proof of work.
This is not Bitcoin Core's order; Bitcoin Core checks proof of work first.

Known Bitcoin header checks that are not performed:

| Check | Reason |
|---|---|
| Version rules | Height-activated and no SPV value. |
| Bitcoin Core checkpoint mismatch | Trust comes from the seeded spine. The name refers to older Bitcoin Core. |
| Block-body checks | Not header fields. |
| MTP when fewer than 11 headers are stored | Local skip. See C4. |
| Boundary difficulty when the period start is missing | Local skip. See C5. |

### Interface and storage (designed here)

**R1 — atomicity.** If the action returns an error, the state is unchanged.

**R2 — idempotence.** If every header in the batch is already stored, the action
succeeds, reports no connected header, and changes nothing.

**R3 — storage exactness.** The stored set is exactly the seed entries plus every
header from every accepted batch. A losing header remains stored and readable.

**R4 — canonical chain.** For a chain with no gaps in stored headers, the
canonical chain starts at the seed header, links continuously and ends at the
best fork tip. A read of a canonical height with no stored header reports an
error; a sparse production seed has such gaps.

**R5 — fork choice by work.** The best fork carries the maximum cumulative work
among the forks that R6 permits to win. Height alone does not decide.

**R6 — switching.** A fork becomes best only if two conditions hold:

1. it is strictly heavier than the old best, and
2. it carries the same block as fork `0` at the mark.

The second condition keeps a heavier fork stored when that fork diverges below
the mark. Equal work keeps the incumbent.

### Finality (designed here)

**R7 — mark safety.** After seeding, the mark never regresses, never exceeds
`tip - 100`, and every height at or below it keeps the block it had when it was
finalized. Seeding itself sets the mark to the newest seed height.

**R8 — mark advancement.** With the seed chain best, if the work above the mark
clears the requirement (`have >= need`), the mark advances to `tip - 100` when
that is greater than the current mark. Mainnet takes `need` from the estimated
work since the mark, floored at a coinbase-maturity window; every other network
takes it from the work of the tip block, which the tip always carries, so on
those networks the depth is the only gate. With partition safety disabled,
every network uses the tip-work rule. This switch is test-only; a production
mainnet node keeps it on.

**R9 — fork gate.** A fork is created only when its forkpoint is above the
finality mark. At or below the mark the action fails with
`ForkBelowFinalized` and changes nothing. Above the mark, R3 and R6 apply.

**R10 — clock floor.** The requirement's clock is the greatest of three values:
the supplied `now`, if any; the stored high-water mark; and the chain time at
the best tip. The chain time is the median of the last 11 header times,
including the tip (Bitcoin Core's `GetMedianTimePast`), so one miner's
timestamp cannot move it. It falls back to the tip's own header time when the
11-header window is not stored: a sparse seed, or a chain shorter than the
window. C7 is not affected: it runs on the supplied value alone. Every batch
that stores a new header raises the high-water mark to the requirement's clock
when that is greater, whether or not the finality mark moves. A batch that
stores no new header leaves it unchanged, so a poll cannot raise the floor.
