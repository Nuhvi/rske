# Design

## Context

See `proposal.md` - Why. The current state that shapes the approach:

- `submit_bitcoin_headers_impl` (`crates/core/src/btc.rs`) runs one write
  transaction: skip the stored prefix, locate the parent, extend a fork or
  create one, validate each new header, store the rows and the cumulative
  work, choose the best fork, run the reorg-mark rule and commit.
- The store holds `btc_headers` (`(fork, height) -> header, work`),
  `btc_hash_loc`, `btc_forks`, `btc_checkpoints` and `btc_meta`. Fork 0 is
  the chain that opening creates.
- `crates/core/src/btc/checkpoints.rs` parses one lowercase-hex 80-byte
  header per line, and line `i` holds height `i * 2016`. The embedded file
  holds 483 lines, from genesis to 971,712.
- `Config::checkpoints` is crate-private. Mainnet resolves the embedded file;
  every other network resolves `(0, genesis)`. Only a test can set a list.
- `btc_checkpoints` grows at runtime: `put_headers` inserts a row when fork 0
  crosses a period start. Nothing removes or rewrites a row.
- The submit path has two reorg teeth today: `ForkBelowFinalized` for a new
  fork, and a mark-crossing test before a heavier fork becomes best.
- `crates/core/specs/bitcoin_header_chain.rs` holds the old BTC-00 to BTC-08
  scenarios and the chain state machine. `crates/core/specs/bitcoin_headers.rs`
  holds the legacy HDR-04 and HDR-08 machines.
- The legacy `specs/` and `docs/` files stay read-only.

## Goals / Non-Goals

**Goals:**

- One durable contract for the chain above the store: the checkpoint set, the
  floor, the batch rules, the tree, the stored set, the best fork and the
  outcome.
- An Electrum-shaped checkpoint set that the caller fixes at open.
- Keep atomicity and idempotency, and keep the per-header rules in
  `bitcoin-header-validation` unchanged.

**Non-Goals:**

- The reorg mark, its requirement, the best-fork admittance test and the two
  outcome fields that carry them. This capability assumes no such mark exists.
- A runtime checkpoint ladder.
- Prefix acceptance (Bitcoin Core) or chunk acceptance (Electrum).
- Coinbase storage and pruning, the Rootstock tree, and the checkpoint
  production policy (how many periods a release ships).

## Decisions

### D1. Period-start checkpoints

The set is a list of headers, one per difficulty period, from genesis:

```
headers[0]   0           genesis block
headers[k]   P * k       first header of period k
```

`P` is the difficulty period that HDR-10 gives for the network. This crate
fixes it at 2016 on all three networks; Bitcoin Core uses 144 on regtest, and
this crate does not. The heights are `0, P, 2P, ...`. The position fixes the
height, so a set with a gap or with two entries in one period cannot be
expressed. The content
stays trusted: the core cannot prove that line `k` really is the first header
of period `k` without the chain. Entry `k` carries the `bits` of period `k`,
and the bits stay constant inside the period. A period therefore counts one
full period of work at its entry's bits.

The set is a list of headers and nothing else. The first header above the
highest entry is at `floor + 1`, inside the period that entry starts, and its
expected bits are the parent's bits. No value is missing. At the next
boundary the retarget reads the period start, which is the highest entry or a
later stored header, and the parent, which the run stores. The end shape
would need one extra value for the first boundary above the floor; the start
shape needs none.

Each header is stored in full, as today. The store writes it to
`btc_checkpoints` and to the `btc_headers` row at its height, with the
cumulative work, and the canonical read still returns it. The change from
today is that the store stops extending the set: `put_headers` no longer adds
a boundary row.

Alternatives:

- **Period ends plus the next target** (Electrum). Rejected. The first header
  above an end is a period start, and its retarget reads the first header of
  that end's period, which the sparse store does not hold. Electrum ships the
  target for that reason; the start shape needs no such value.
- **Copy the Bitcoin Core list.** Rejected. Core v29 places checkpoints at
  arbitrary heights (11111, 33333, ...) and uses them only as an anti-reorg
  hash check. It has no period structure and no work ladder.

The work formula follows the shape. Entry 0 counts the genesis block. Entry
`k` counts one full period at every period below `k`, plus its own work. This
is the formula in use today.

### D2. The set is an open-time input, never extended

`Config` gains a public constructor for the set. The embedded mainnet file
stays the default, and every other network defaults to genesis. An open adopts
the configured entries up to the tip (D10); `put_headers` never writes
`btc_checkpoints`.

The floor is the highest entry **the store holds**, not the highest entry the
configuration names. A shorter configured set on a reopen is a no-op: it can
never lower the floor below a checkpoint the store already trusted.
`CoreState` gains `checkpoint_floor: Option<u64>` so a test and the model can
read the floor without a fork attempt.

Alternatives:

- **A runtime ladder.** The user rejected it. The old code grew the table on
  fork 0 only, and a reorg left stale rows above the new forkpoint.
- **A table-free design.** The table stays: it is the only record of the
  entries below the sparse region, and `initialize_with_checkpoints` compares
  against it.

### D3. The fork gate is the floor

A new fork fails with `ForkBelowCheckpoint { fork, checkpoint }` when
`forkpoint <= floor`. This is a new variant. `ForkBelowFinalized` stays for
the finality work, and this capability does not rename or remove it. The
extension path does not test the floor.

This capability assumes that no reorg mark exists. The chain path SHALL NOT
let a mark refuse a fork that the floor admits. The finality rule and its
error variant are outside this capability; the finality change decides how
the two rules compose. A fork that starts just above the floor and becomes
heavier SHALL win, however much canonical history it replaces.

The exact input `forkpoint == floor` is unreachable while the store holds no
contiguous row below the floor: the parent would sit at `floor - 1`, and only
entries are stored there. Keep `<=` to match Electrum and to stay correct if
a later change stores that region. See the mutation note below.

### D4. Atomic and idempotent

One transaction holds the skip, the parent lookup, the fork decision, the
header checks, the writes and the best-fork choice. A refused batch drops the
transaction. The longest stored prefix is skipped, and an empty or fully
stored batch succeeds with no write.

This is a declared difference from both references. Bitcoin Core keeps the
valid prefix and returns the reason of the first bad header. Electrum rejects
a whole 2016-header chunk and saves nothing for it. The crate keeps the
stricter unit because the store is one transaction and the sync driver
retries from a peer prefix.

### D5. The first header above the floor

The floor is a period start. The first header above it is at `floor + 1`,
inside that period. Its expected bits are the parent's bits, so HDR-04 applies
as written and no extra value is needed.

At the next boundary the retarget reads the period start at `floor`, which is
stored, and the parent, which the run stores. The testnet minimum-difficulty
walk-back stops at the same stored entry, so it has no gap either.

### D6. Outcome

The outcome keeps four fields: `accepted`, `prev_best_fork`, `best_fork` and
`tip_height`. No field is added. A refused submission is `Err` and no outcome.
A no-op is `Ok` with zero accepted, the same best fork twice and the current
tip.

### D7. Names

| Term | Meaning |
|---|---|
| trusted checkpoint, entry | one element of the open-time set |
| period start | height `2016 * k` |
| checkpoint floor, floor | the height of the highest stored entry |
| checkpoint table | `btc_checkpoints`, the stored entries |

`checkpoint` never appears bare. The requirement IDs are `BTC-00` to
`BTC-10`. The capability is `bitcoin-header-chain`, a sibling of
`bitcoin-header-validation`.

### D8. Test layout and verification

The five layers from `docs/core-verification.md`:

- **Scenario.** One test per requirement: `btc_00_trusted_checkpoints` to
  `btc_11_prune_to_the_floor`. Each test calls `Core::apply`. Each spec
  scenario carries a `// Scenario:` comment at its assertion. The prune
  scenarios need a chain past the first period, about 2016 headers; share one
  fixture.
- **State machine.** One machine over the chain file. It predicts the full
  `CoreState` (including `checkpoint_floor`), every `StoredHeader` row and the
  four outcome fields. It computes a header's work from the target, not
  through `crate::header_work`, and it computes the entry work from the
  `bits` of the entries. The universe holds a trunk, a fork above the floor,
  a fork from an earlier entry, a bad-PoW header, a wrong-bits header,
  duplicate and empty batches, and cuts at the first new header, at the fork
  point and at a stored suffix.
- **Exhaustive search.** Skipped: the fork tree over real headers is far above
  the 10,000-state budget, and the machine already walks every reachable cut.
- **Mutation.** Run one file at a time on `crates/core/src/btc.rs` and
  `crates/core/src/store/btc.rs`. No new exclusion. If the floor comparison
  survives as an equivalent mutant, exclude `crates/core/src/btc.rs` at the
  gate line and name the unreachable input `forkpoint == floor` as the reason.
- **Kani and fuzzing.** Skipped: the change adds no pure arithmetic and no
  byte parser. `work_scaled` and the checkpoint text parser stay as they are.

Gate: `openspec validate --all --strict` and
`cargo test -p rsk-core --lib specs::`.

### D9. File and store layout

The text file keeps one lowercase-hex 80-byte header per line. Line `i` holds
height `i * 2016`, as today. The parser and the embedded file do not change.

The `btc_checkpoints` value stays the 80-byte header, and the `btc_headers`
rows keep the header and the cumulative work. `put_headers` never adds a
boundary row. The open adopts the configured entries and prunes the rows that
D10 names, in one transaction.

`export_checkpoints` writes the start headers, as today. Its cutoff is a
driver decision; this capability states none.

### D10. Adopt and prune on open

The open adopts each matching configured entry at or below the tip and writes
its checkpoint row. It then prunes against the highest stored entry:

- every fork whose block at that entry differs from the entry or is absent,
  with its `btc_forks` row and every `btc_headers` and `btc_hash_loc` row on
  it;
- every `btc_headers` and `btc_hash_loc` row below the entry that is not an
  entry.

The prune leaves fork 0 and every fork that branches above the entry; those
carry the entry through their ancestors. It makes the heaviest surviving fork
best. A purge can remove the best fork, so its coinbase rows no longer match
the canonical chain. The coinbase retention rule must reconcile them; this
capability states no coinbase rule.

The prune runs on every open against the highest stored entry. A legacy store
that the old runtime ladder filled is cleaned the first time the new code
opens it, even when the configured set does not reach the old floor. A refused
comparison (D2) aborts before any prune. The adopt and prune run in one write
transaction, so a failed open changes nothing.

## Deviation table

| Behavior | Bitcoin Core v29 | Electrum | This crate |
|---|---|---|---|
| Checkpoint shape | arbitrary heights, hash only | period end, next target | period start, header only |
| Trusted region | full chain from genesis, checked against checkpoints | chunks below `max_checkpoint` | sparse entries, nothing between them |
| Fork gate | header height < last known checkpoint | forkpoint <= `max_checkpoint` | forkpoint <= stored floor |
| Fork cleanup | none | delete a chain at or below `max_checkpoint` at startup | prune it and its rows on open |
| Difficulty period | 2016 mainnet and testnet3, 144 regtest | 2016 on every network | 2016 on every network, HDR-10 |
| Batch failure | valid prefix kept | whole chunk rejected | whole run rejected |
| Best chain from headers | needs block bodies to activate a tip | swaps chain files when heavier | selects the best fork at once |
| Work tie | earliest received wins | parent wins | incumbent wins |
| Invalid header memory | cached, descendants refused | none | none, the next batch re-validates |
| Difficulty above the anchor | computed from genesis | shipped next target | computed from the stored period start |
| MTP and future time | both checked | neither checked | both checked, in `bitcoin-header-validation` |

Each row is intentional. The first two rows carry the Electrum model, the
batch row carries atomicity, and the last row keeps the stricter header
rules.

## Risks / Trade-offs

- [The open prunes rows] -> The adopt and prune run in one transaction after
  the comparison, so a failed open changes nothing. A successful open removes
  the forks that do not carry the floor and the headers that the sparse region
  does not need. A rollback cannot restore them; the store is test data.
- [A legacy store holds runtime-added boundary rows] -> The prune runs against
  the highest stored entry, so it removes the forks below that entry and the
  headers below it that are not entries. The entries stay.
- [A shorter configured set could weaken the floor] -> Read the floor from
  the store, not from the configuration.
- [The existing mark rule refuses a fork above the floor] -> The chain
  contract admits that fork. The `Deep reorg above the floor` scenario pins
  it, and the finality change reconciles the two rules.
- [The prune leaves stale coinbase rows] -> The coinbase retention rule must
  reconcile the table after a purge; this capability excludes coinbases.
- [The floor boundary mutant is equivalent] -> Name the unreachable input in
  the mutation note. Delete the guard only if a later change makes the input
  reachable.

## Migration plan

Ship one change: the public configuration, the open-time adopt and prune, the
floor gate, the canonical read, the stopped ladder growth and the tests. A
store that the old binary wrote opens and the prune cleans it. Roll back by
reverting the change; pruned rows do not return.

## Open Questions

None that change the spec, the approach or the tasks. The exporter's default
cutoff height is a driver decision for a later change.
