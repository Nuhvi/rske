# Design

## Context

See `proposal.md` for the motivation. This section records the current state
that shapes the approach.

The machine lives in `submit_bitcoin_headers_impl`
(`crates/core/src/btc.rs:658-924`). One write transaction holds the whole
sequence: skip the stored prefix, locate the parent, extend or create a fork,
validate each new header, store the rows and the cumulative work, choose the
best fork, and run the finality rule. Any error drops the transaction.

The legacy clauses live in `specs/bitcoin-headers.md`, which stays read-only:
O1 and E1 to E7. `bitcoin-header-validation` owns the per-header rules in
`crates/core/src/btc/validation.rs`. The finality rule lives in
`crates/core/src/btc/finality.rs` and `finalize_in_txn`.

The store holds `btc_forks`, `btc_headers`, `btc_hash_loc` and
`btc_checkpoints`. Fork 0 is the root fork. The finality mark lives in
`btc_meta`.

The mark has two teeth in the submit path:

- `btc.rs:735-742`: a new fork fails with `ForkBelowFinalized` when
  `forkpoint <= mark`.
- `btc.rs:867-889`: a strictly heavier fork becomes best only when its block
  at the mark equals the root fork's block at the mark.

The chain machine is `bitcoin_headers_match_the_spec` in
`crates/core/specs/bitcoin_headers.rs`. O1's test is
`store_identity_matches_the_spec`. The legacy file names the capability after
the canonical chain, but the store is a fork tree.

## Goals / Non-Goals

**Goals:**

- Give O1 and E1 to E5, plus the batch outcome, one durable capability with
  scenario tests that drive `Core::apply`.
- Define the trusted checkpoints, the fork tree, the batch contract and the
  four chain outcome fields.
- Remove the shipped reanchor and its `resume_from` helper.
- Leave the `bitcoin-header-validation` requirements unchanged.

**Non-Goals:**

- The finality mark's advance, the mainnet requirement and the clock (E6, E7).
  `bitcoin-finality` owns them.
- The creation gate and best-fork admittance. `bitcoin-finality` owns both.
- A pure header validator, external vector files, signet and testnet4.
- The Rootstock tree and its checkpoint policy.
- Edits to the legacy `specs/` and `docs/`.

## Decisions

### D1. The mark keeps both teeth in `bitcoin-finality`

The creation gate and the promotion check are one predicate at two points. A
new fork with `forkpoint <= mark` cannot carry the mark's block: its own row at
the mark is a different block, or the row does not exist. The promotion check is
the general form, and it also catches a fork that parents on an overtaken fork.

This change puts both rules in `bitcoin-finality`. The chain spec states the
comparison and points at the finality rule in three places:

- BTC-00: the highest trusted checkpoint is the initial mark.
- BTC-05: the finality rule can refuse a new fork.
- BTC-07: a fork that the finality rule does not admit never becomes best.

Alternative: chain takes the mark as a stored input and owns both teeth. This
keeps one owner for "what the mark does to a submission", but it puts the
mark's meaning in chain. It also forces chain tests to advance the mark through
finality's rule, or to write the mark through a back door as
`fork_below_finalized_is_refused` does today. Reject it.

The cost: the chain spec has two forward references to a capability that does
not exist yet. The finality change will fill them. The chain tests keep the mark
at the checkpoint tip, so all forks are admitted there, and the references are
not exercised by the chain suite.

### D2. Nine requirements, named after their property

Legacy O1 splits into BTC-00 and BTC-01. The 500-character requirement limit
forces one behavior per requirement, and the split is natural: one rule
initializes a fresh store, the other compares an existing store.

Legacy E2 splits into BTC-03 and BTC-04. The stored prefix and the no-op are one
property. The link rule has its own error and does not depend on the prefix.

| Legacy | ID | Name |
|---|---|---|
| O1 initialize | BTC-00 | Trusted checkpoints |
| O1 compare | BTC-01 | Store identity |
| E1 | BTC-02 | Atomicity |
| E2 skip/no-op | BTC-03 | Idempotency |
| E2 link | BTC-04 | Contiguity |
| E3 | BTC-05 | Forks |
| E4 | BTC-06 | Stored set |
| E5 | BTC-07 | Best fork |
| new | BTC-08 | Outcome |

The ID prefix is `BTC-`, not `CHAIN-`. "Chain" is ambiguous in this workspace:
the Rootstock sidechain is also a chain, and the capability's subject is the
fork tree. `BTC-` names the Bitcoin side. The capability keeps the
`bitcoin-header-` prefix, so it stays a sibling of `bitcoin-header-validation`.

Requirement names are properties, not clause letters: Atomicity, Idempotency,
Contiguity, Forks, Stored set, Best fork. A test name carries the ID
(`btc_02_atomicity`), so a renamed test is a visible break.

### D3. Terminology: trusted checkpoints

The spec uses these terms:

| Term | Meaning | Code |
|---|---|---|
| trusted checkpoint | a configured header that the crate assumes correct, one per Bitcoin period boundary | `Config::checkpoints`, `btc::checkpoints`, `put_checkpoints` |
| checkpoint height | the height of a trusted checkpoint | `btc_checkpoints` |
| highest checkpoint | the last trusted checkpoint; its height is the initial finality mark | the last trusted checkpoint |
| root fork | the fork that starts at the first checkpoint height | fork 0 |
| fork | one chain in the tree, starting at a forkpoint | `ForkMeta` |
| forkpoint | the height of a fork's first header | `ForkMeta::forkpoint` |
| parent fork | the fork that stores the block a new fork extends | `create_fork` |
| best fork | the fork the store treats as canonical | `best_fork` |
| canonical chain | the best fork's headers, resolved through its ancestor forks | `header_at` |

Rules:

- `trusted checkpoint` always stays qualified. `finalized checkpoint` is the
  finality side's term: a Bitcoin period boundary at or below the mark.
  Trusted means assumed; finalized means derived.
- The verb for the open action is **initialize**. The code initializes a
  fresh store with the configured trusted checkpoints.
- `seed`, `anchor` and `spine` do not name the trusted checkpoints. `seed`
  stays for DNS peer seeds. `anchor` stays for the finality mark and the RSK
  merge-mining anchor. `checkpoint_at` keeps its name.
- `checkpoint` never appears bare. The existing repository rule still applies.

`docs/terminology.md` now carries the `trusted checkpoint` and
`checkpoint table` rows. The finality capability adds the
`finalized checkpoint` row.

HDR-00 now says "before it creates a store or initializes one with the
configured trusted checkpoints".

### D4. Prefix skip: the stored set is global

The skip is the longest leading run of hashes the store holds on any fork. A
batch can start below the best tip; the parent, not the tip, chooses the fork.
The link check runs only from the first new header. An all-stored or empty
batch does not run it, so a stored sequence need not link. A stored header
after a new header is not skipped and fails `LinkBreak`, because its parent is
not the new header.

### D5. Fork identity

A fork is `(parent fork, forkpoint, first hash)`. The parent fork is the fork
that stores the parent row, not the fork that was canonical at that height. A
fork from an ancestor row below another fork's forkpoint attaches to the
ancestor fork. The fork tree can flatten; header ancestry does not change.

Rows of one fork are contiguous from its forkpoint. A batch extends a fork only
at its tip. A fork of a fork points at the fork that stores the parent row.
Sibling forks share the parent and the forkpoint, and differ by the first hash.
A first hash is never stored twice. Fork ids are monotone and never reused.

### D6. Best fork: strict work, incumbent on a tie

The switch is `target_work > best_work`. Equal work keeps the incumbent. The
invariant is: no admitted fork is strictly heavier than the best fork. It holds
because work never shrinks and every submission compares its target fork with
the incumbent.

The root fork is exempt from admittance and can become best again. A fork that
crosses the mark stays stored and never becomes best.

The invariant depends on the mark's movement. The shipped reanchor is the only
path that can move the mark past a non-root best fork. D8 removes it.

### D7. Outcome: four chain fields, two finality fields

| Field | Owner | Value |
|---|---|---|
| `accepted` | chain | new headers stored on any fork; zero for a no-op |
| `prev_best_fork` | chain | the best fork before the submission |
| `best_fork` | chain | the best fork after the submission |
| `tip_height` | chain | canonical tip after the submission; the current tip for a no-op; not the batch fork's tip |
| `finalized` | finality | the new mark, when this batch moved it |
| `finality` | finality | the requirement measurement |

The two chain fields `prev_best_fork` and `best_fork` are `Option` in the code.
A supported store always opens with the root fork, so the value is present. Keep
the type for API stability. Correct the `prev_best_fork` field doc: it claims
`None` on the first connection or when the batch stays on the same fork. The
code always reports the previous best fork.

A refused submission returns an error, not an outcome.

### D8. Remove the shipped reanchor

`Core::reanchor_shipped` (`btc.rs:339-372`) adopts the newest configured
checkpoint the store already holds. The only production caller is `resume_from`
(`crates/rsk/src/node.rs:173-180`), at the start of `Node::sync`. The `from`
value feeds one log line; `sync::run` computes its own start height.

The adoption needs `B <= fork-0 tip` and `B > mark`. The automatic rule sets the
mark to `tip - 100`. The gain is therefore at most 100 blocks, and only when the
store's tip sits just above a newly shipped checkpoint. The automatic rule
gives the same movement with the next stored header.

The reanchor does not require the root fork to be best. On a resumed store with
a non-root best fork, it can move the mark above that fork's forkpoint, or above
its tip. Two invariants then fail: the best fork carries the mark's block, and
the mark stays below `tip - 100` of the best chain. The automatic rule requires
`best_fork == 0` (`btc.rs:1117-1119`); the reanchor does not.

Remove the action, the outcome variant, the public method, the implementation
and the test. Remove `resume_from` and inline `tip + 1`. The `btc_checkpoints`
table stays: `initialize_with_checkpoints` uses it for the comparison.

Alternative: keep the reanchor and add the `best_fork == 0` guard. It preserves
a narrow shortcut and adds a finality requirement. Reject it. The shortcut has
no consumer, and the finality capability is simpler with one movement rule.

The finality change will state that one rule: the mark advances only while the
root fork is best, to the root tip minus 100, when the requirement clears.

The removal has no BTC requirement. It is a non-normative refactor in this
change. The proposal lists it, and this section gives the reason and the
invariant it restores.

### D9. Test layout

```text
crates/core/specs/
  bitcoin_header_chain.rs   O1/BTC-00..BTC-08 scenarios and the delivery machine
  bitcoin_header_validation.rs   unchanged
  bitcoin_headers.rs        HDR-04 machine, HDR-08 machine, finality machine
  support.rs                shared fixtures, the delivery universe and the submit helpers
  mod.rs                    one file per capability
```

- Move `predict`, `HeaderMachine`, `HeaderSut` and the
  `bitcoin_headers_match_the_spec` test to the chain file. Rename the test to
  `bitcoin_header_chain_matches_the_spec`.
- Move `HeaderUniverse`, `HeaderKind` and `header_universe` to `support.rs`, so
  the chain machine and the HDR-07 scenario share one fixture without one
  capability file importing another.
- Move `store_identity_matches_the_spec` to the chain file and rewrite it as
  the BTC-00 and BTC-01 scenarios.
- Move `submit`, `expect_err` and `expect_accepted` from
  `bitcoin_header_validation.rs` to `support.rs`, so both files share them.
- Keep the HDR-04, HDR-08 and finality machines in `bitcoin_headers.rs` until
  their capabilities exist. Update the module docs.

Fixtures:

- Chain tests use a one-entry checkpoint set and stay below 100 heights above
  it. The mark then never moves, so finality stays out of the chain suite.
- BTC-01 uses a two-entry checkpoint set and a file-backed store for the
  reopen scenarios.
- Every scenario calls `Core::apply`. The test name carries the ID.

Scenario map:

| Requirement | Scenarios |
|---|---|
| BTC-00 | fresh store, initial mark |
| BTC-01 | reopen, longer checkpoint set, different network, different checkpoint |
| BTC-02 | fork then failure, refused batch |
| BTC-03 | repeated prefix, all stored, empty, prefix on a losing fork |
| BTC-04 | gap, prefix link, stored header after a new header |
| BTC-05 | extend, fork from a non-tip, fork of a fork, siblings, distinct first hash, below the best tip, contiguous fork |
| BTC-06 | losing fork, no removal, cumulative work |
| BTC-07 | heavier wins, shorter but heavier fork wins, lighter stays, equal keeps, later win, root wins back, canonical |
| BTC-08 | extend, switch, no-op, losing fork |

### D10. Order and traceability

The chain does not change the rule order. `bitcoin-header-validation` owns it:
link, expected difficulty, median-time-past, future time, target range, proof of
work. The chain adds the batch steps around it: skip, parent, fork, validate,
store, best fork, finality. The transaction makes the order invisible on
failure.

Traceability is direct: the requirement header carries `[BTC-xx]`, the test name
carries the same number, and the archived change keeps the decision record.
There is no separate verification map.

The legacy `specs/bitcoin-headers.md` and `docs/` stay read-only. No task edits
them or reads them as an input. `crates/core/specs/` is the executable test
directory and stays.

## Risks / Trade-offs

- **[The chain spec points at a capability that does not exist yet]** → the two
  references name exact rules. The finality change fills them next. The chain
  suite does not exercise them, and the design records that.
- **[The reanchor removal has no BTC requirement]** → the proposal lists it and
  this design gives the reason. The finality change states the single movement
  rule it leaves behind.
- **[`trusted checkpoint` adds a fifth checkpoint meaning]** → the term always
  stays qualified, and `finalized checkpoint` is finality's. A later docs change
  updates `docs/terminology.md`.
- **[The chain tests freeze the mark at the checkpoint tip]** → the creation
  gate and admittance stay with finality. The chain suite cannot hide a
  regression there, and the finality suite must cover it.
- **[The machine move changes the proptest regression file name]** → the
  untracked `crates/core/specs/bitcoin_headers.txt` holds only comments. No
  case is lost.
- **[`prev_best_fork` is `Option` but always present]** → the spec states the
  value, and the code keeps the type. Correct the field doc.
- **[Removing `Action` and `Outcome` variants breaks exhaustive matches]** → no
  matcher in this workspace uses them, and the crate is not published.
- **[An offline, fully synced store keeps its old mark]** → the loss is
  conservative: no consumer needs the jump, and the next stored header moves
  the mark.
- **[The legacy documents go stale]** → they are read-only and no task depends
  on them. A separate cleanup can delete them.

## Migration Plan

No store schema change and no data migration. A store initialized before the
change
opens the same way. The `btc_checkpoints` table stays.

The reanchor removal is a code change. Rollback is a revert of the change; a
store that the old binary reanchored keeps its mark and opens the same way.

## Open Questions

None. The finality split and the reanchor removal are decided.
