# Rootstock checkpoint trust

Status: draft
Executable form: [`crates/core/specs/rootstock_checkpoints.rs`](../crates/core/specs/rootstock_checkpoints.rs)

This document describes the Rootstock trust policy on top of a finalized Bitcoin
tail. It covers `ProposeCheckpoint`, the checkpoint attempt triggered by an empty
`ConnectRskHeaders`, and `SetRskSyncTip`.

## Scope

In scope:

- accepting a proposed RSK root only when its merge-mining anchor is canonical
  Bitcoin inside the finalized tail,
- the strict 2/3 work threshold and heaviest-root selection,
- GHOST subtree work over the staged forest,
- recording and resolving the sync tip.

Out of scope:

- `ConnectRskHeaders` with real entries and uncle validation,
- tag parsing, CPV scoring and `select_final`,
- re-anchoring, multiple Bitcoin tails, and the production mainnet parameters.

## 1. State

- **B1 — Bitcoin tail.** A fixed, finalized Bitcoin chain. `tail_start` is its
  newest finalized difficulty-period boundary; `mark` is the finality mark.
- **S1 — RSK forest.** Every accepted RSK header, keyed by hash, with parent
  edges added only when the parent was already stored at insert time.
- **S2 — roots.** The proposed checkpoint blocks. A root is just a staging
  boundary; its tree is the root plus every stored descendant.
- **S3 — sync tip.** The newest RSK header an IBD pass committed. The recorded
  hash may name a header that is not stored.

## 2. Actions

- **A1 — propose(i).** Validate entry `i`'s anchor and, if accepted, store it and
  mark it a root.
- **A2 — attempt.** Connect an empty batch; this runs the checkpoint attempt
  without changing the tree.
- **A3 — sync(i).** Record entry `i`'s hash as the sync tip.

## 3. Requirements

**R1 — anchor acceptance.** A proposal is accepted iff the entry's merge-mining
anchor resolves to a canonical Bitcoin block at a height in
`[tail_start, mark]`.

**R2 — refusal atomicity.** A refused proposal leaves the forest, roots and sync
tip unchanged.

**R3 — forest exactness.** The stored forest is exactly the set of accepted
entries; the root set is exactly the set of accepted proposals; a root is also a
stored header.

**R4 — strict threshold.** The checkpoint attempt reports a checkpoint iff at
least one root's GHOST subtree work satisfies
`work * 3 > tail_work * 2`. Equality does not clear the bar.

**R5 — heaviest clearing root.** When a checkpoint is reported, it is the
clearing root with the greatest subtree work. Its number, work, `tail_start` and
`btc_tail_work` match the store.

**R6 — GHOST work.** A root's subtree work is the sum of the cumulative
difficulty of the root and every stored descendant. A parent's subtree work is
strictly greater than each child's.

**R7 — sync tip.** The recorded sync-tip hash is exactly the last one written. It
resolves to `(hash, number)` iff that header is stored.

**R8 — attempt is read-only.** An attempt changes no forest, root or sync-tip
state.

## 4. How the executable spec is organised

`rsk_checkpoint_policy_matches_the_spec` walks `propose` (7 entries),
`attempt` and `sync` (7 entries) over a real finalized Bitcoin tail:

```text
A(0) anchored at 2016, work 2000
B(1) parent A, anchored at 2500, work 1000
C(2) parent B, anchored at 3000, work 500
D(0) anchored at the mark, work 3000
E(0) anchored above the mark      -> refused by R1
F(0) anchored below the tail      -> refused by R1
G(0) anchored at an unknown block -> refused by R1
```

The model tracks the child edges created at insert time, because a child
proposed before its parent does not become linked retroactively.

## 5. Not covered yet

- Valid `ConnectRskHeaders` batches and uncle rules.
- Tag-based finality (`Scoreboard`, CPV matching, `select_final`).
- Production mainnet parameters and a moving Bitcoin mark.
