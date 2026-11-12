# Bitcoin header ingestion

Status: draft
Executable form: [`crates/core/specs/bitcoin_headers.rs`](../crates/core/specs/bitcoin_headers.rs)

This document describes the Bitcoin header state machine seeded at open and
exposed by `Core::apply(Action::ConnectHeaders { headers, now })`. The Rust
comments use the same clause IDs.

Opening rules use `O*`. Validation checks use `V*`. State transition rules use
`E*`. Some `V*` clauses contain local policy; those parts are marked
**Local**.

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

The machine is configured once, at open, with:

- **Network.** The Bitcoin consensus parameters.
- **Partition safety.** Whether the mainnet network-partition safety rule runs.
  The default is on; a test disables it (E6).
- **Seed.** Trusted headers at difficulty-period boundaries. The crate fixes it:
  the embedded spine on mainnet, genesis elsewhere, replaced in tests (O1).

## 2. State

The state machine tracks:

- **S1 — stored headers.** Every stored Bitcoin header, keyed by fork and
  height.
- **S2 — forks.** Fork `0` is the seed chain. Every other fork has a
  `forkpoint`, the height of its first header, and a contiguous run of headers
  from that height. A fork's cumulative work is the work stored on its chain.
- **S3 — best fork.** The fork that E5 selects. Its chain is the canonical
  chain.
- **S4 — finality mark.** The newest height treated as un-reorgable.
- **S5 — clock high-water mark.** The greatest finality clock seen.

## 3. Action

**A1 — connect a batch.** One action delivers:

- **Headers.** A run that the caller claims is contiguous.
- **`now`.** The caller's clock, or none. It lets the machine judge header
  timestamps and measure finality without an internal clock (V7, E7).

## 4. Requirements

### Opening

**O1 — store identity.** Opening an empty store plants the configured seed,
records the network, and sets the mark to the newest seed height. Opening an
existing store must agree with the configuration on the network and on every
checkpoint both hold, or the open fails. It adds no checkpoint.

### Validation

**V1 — parent.** After the skipped prefix (E2), the first new header must name
a stored parent. If its parent is not stored, the action fails with
`UnknownAncestor`. Bitcoin Core calls this `prev-blk-not-found`. Bitcoin Core
does not keep invalid previous headers, so `bad-prevblk` cannot arise.

**V2 — linkage.** From the first new header, each header must name the previous
header as its parent. A mismatch fails with `LinkBreak`.

**V3 — proof of work.** Each new header must meet its own target. Failure gives
`BadPow`. Bitcoin Core calls this `high-hash`.

**V4 — median-time-past.** Bitcoin requires the timestamp to be greater than the
median of the previous **up to** 11 block times. Failure gives `BadTimestamp`
(Bitcoin Core `time-too-old`).

**Local:** the check applies only when all 11 previous headers are stored. If one
is missing, or the header is in the first 11 heights, the check is skipped.

**V5 — expected difficulty.** The header bits must equal the expected target.
Inside a period they are the parent's. At a period boundary the retarget
recomputes them; testnet4 starts from the period-start bits (BIP94), every other
network from the parent's, and regtest keeps the parent's. On a network that
permits minimum-difficulty blocks, a header more than twice the target spacing
after its parent must carry the proof-of-work limit; any other header there
expects the last non-minimum-difficulty bits of the period. Failure gives
`BadDifficulty` (Bitcoin Core `bad-diffbits`).

**V6 — target range.** Bitcoin rejects a target that is negative, overflowing,
zero or above the network proof-of-work limit. The same range check runs before
proof of work. A failure gives `BadPow` with `target out of range`.

**V7 — future time.** Bitcoin rejects a header whose timestamp is more than two
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
| MTP when fewer than 11 headers are stored | Local skip. See V4. |

### State transition

**E1 — rejected action.** If the action fails, the state does not change.

**E2 — repeated prefix.** The action skips a leading run of stored headers and
starts at the first header not yet stored. If every header is stored, the action
succeeds, reports no connected header, and changes nothing.

**E3 — forks.** The target fork is the fork that holds the first new header's
parent. The action extends it when the parent is its tip, and otherwise starts a
new fork at the next height. A new fork's first height must be above the
finality mark, or the action fails.

**E4 — stored headers.** The action stores each header after the skipped prefix
at its height on the target fork. It stores nothing else and removes no header,
so a losing fork's headers stay stored.

**E5 — best fork.** The best fork is the one with the most cumulative work
among the forks that carry the mark's block. A batch changes it only to a
strictly heavier fork; equal work keeps the incumbent.

**E6 — finality mark.** The mark never regresses and never exceeds `tip - 100`.
With the seed chain best, it moves to `tip - 100` when that is greater and the
work above the mark clears the requirement. Mainnet takes the requirement from
the estimated work since the mark, floored at a coinbase-maturity window; every
other network takes it from the work of the tip block, so depth alone gates
those networks. With partition safety disabled, every network uses the tip-work
rule. This switch is test-only.

**E7 — clock.** The requirement's clock is the greatest of the supplied `now`
(if any), the stored high-water mark, and the chain time at the best tip: the
median of the last 11 header times, or the tip's own time when that window is
not stored. V7 uses only the supplied value. A batch that stores a new header
raises the high-water mark to this clock when greater; a batch that stores none
leaves it unchanged.
