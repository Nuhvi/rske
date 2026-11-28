# Design

## Context

The rules live in `crates/core/src/btc/validation.rs` and are interleaved in
`btc.rs::connect_headers_impl`. `Core::apply(Action::ConnectHeaders)` is the one
mutation entry point; the public wrappers only dispatch to it.

The executable spec is `crates/core/specs/bitcoin_headers.rs`. It holds four
`proptest-state-machine` machines and five plain tests. The plain tests are rule
tests that sit in the machine file. Shared fixtures (`mine`, `mine_bad`,
`synthetic_mainnet_core`) are private to it.

A fresh store seeds from the embedded spine on mainnet, or from genesis on the
other networks. The seed tip is a period boundary. The store holds no ancestors
below that height, so the median-time-past window is incomplete for the first 11
heights above the seed.

`Core::new` accepts every `Network`. `rsk.rs` and `rootstock.rs` map testnet4
and signet to the testnet Rootstock config, but no Rootstock network uses them.

`bitcoin` 0.32 supplies the compact-target arithmetic, including
`CompactTarget::from_next_work_required`. The current boundary test computes its
expected value with `from_header_difficulty_adjustment`, which calls the same
function. That test proves the inputs, not the formula.

A 2015-block boundary fixture takes about 8 s through an in-memory `Core` on
this machine.

## Goals / Non-Goals

**Goals:**

- Put every requirement of `bitcoin-header-validation` behind one scenario test
  that drives `Core::apply`.
- Separate rule scenarios, shared fixtures and the existing state machines
  into different files.
- Record every known difference from Bitcoin Core in one place.

**Non-Goals:**

- The chain state machine (`O*`, `E*`): prefix skip, forks, best-fork choice,
  finality mark. Those become their own capability.
- Strict Core parity for the sparse median-time-past window and the version
  floor. Both are declared decisions, not silent gaps.
- Signet support.
- A full differential harness against `bitcoind`.

## Decisions

### D1. The test surface is `Core::apply`

Every requirement scenario calls
`Core::apply(Action::ConnectHeaders { headers, now })` and asserts the `Outcome`
or the `Error`. A pure `validate_header(header, view)` is not extracted.

Reasons:

- The bug class we fear is a correct rule fed the wrong view. The view is built
  by the submit path (`connect_headers_impl` until the rename): prefix skip,
  target fork, `at(height)` walks to ancestor forks, period start, last normal
  bits, the 11 times. A unit test of the predicate cannot see that wiring.
- The spec talks about "the submission". The test should use the same language.
  A function-level test would force the spec to describe an internal API.
- A second surface is a second thing to keep in sync. The current interleaving
  already passes the same values that a view struct would hold.

Alternatives considered: extract a predicate and unit-test it; keep it as a
secondary test. Rejected. Helper tests for pure arithmetic
(`median_time_past`, compact decode) stay as ordinary unit tests.

The cost is fixture construction. D8 gives the fixture for each requirement.

### D2. Capability boundary: the predicate, not the store

This capability owns the rule and the content of the view. The chain capability
owns how the store supplies the view and what happens after acceptance.

The seam in prose is:

| View input | Rule that reads it |
|---|---|
| header fields | HDR-01 through HDR-08 |
| parent header | HDR-01, HDR-04, HDR-05, HDR-06, HDR-07 |
| period-start header | HDR-06 |
| last normal header in the period | HDR-05 |
| the 11 ancestor times | HDR-07 |
| caller clock | HDR-08 |
| network parameters | HDR-02, HDR-04, HDR-05, HDR-06 |

`HDR-00` reads only the network name, at open. `HDR-01` stays in validation
because Bitcoin Core rejects an unknown parent as a header failure
(`prev-blk-not-found`). Batch contiguity, the repeated-prefix skip and atomicity
stay with the chain capability.

### D3. Requirement granularity and IDs

One requirement per Core branch, plus the network scope: nine requirements,
`[HDR-00]` through `[HDR-08]`. The requirement name carries the ID, so a Rust
comment can cite it without a second registry.

IDs are append-only. A later change that adds a rule uses the next number. A
requirement that changes is MODIFIED under the same header; it is never
renumbered. The `C*` and `V*` letters disappear from validation code. `O*` and
`E*` stay until the chain capability defines `CHAIN-*` IDs.

Bitcoin Core's reject strings and function names go in this design, not in the
spec.

### D4. Median-time-past: keep the skip (option b)

`HDR-07` states that the check is skipped when fewer than 11 ancestors are
stored.

- Bitcoin Core computes the median over *up to* 11 blocks, including the parent
  (`src/chain.h`, `GetMedianTimePast`). A shorter chain uses a shorter window.
- This client seeds at a period boundary and stores nothing below it. Strict
  parity would need 11 context headers in the seed or in the first request.
- The skip widens the accepted set for heights `seed + 1 ..= seed + 11`, on any
  fork that branches in that range. It cannot create work. The affected heights
  are at most 11 above a trusted boundary.
- A synthetic window built from the seed time is rejected: it is neither parity
  nor clearly safe.

Alternative (a), seeding or fetching 11 context headers, is the follow-up that
turns this into a MODIFIED requirement. It belongs with the sync layer, not with
this change.

### D5. Supported networks: mainnet, testnet3, regtest

`Core::new` SHALL refuse `Network::Testnet4` and `Network::Signet` with
`Error::UnsupportedNetwork`, before it opens or seeds a store. Those are the
only two networks the client does not use. Rootstock maps to Bitcoin mainnet,
testnet3 and regtest; the fallbacks in `rsk.rs` and `rootstock.rs` that send
testnet4 and signet to the testnet Rootstock config do not make them supported.

Reasons:

- A header-only client cannot validate signet. `CheckSignetBlockSolution` reads
  the coinbase witness.
- No Rootstock network uses testnet4.
- Accepting a network is a support claim. An unused consensus branch is
  untested code.

Alternative: keep both for completeness. Rejected. This is an RSK edge client,
not a general SPV library. If an RSK deployment needs a network, add it in its
own change; the requirement IDs are append-only.

The refusal is the first check in `Core::new`, so a refused network creates no
store file. An existing store seeded for a different supported network still
fails with `NetworkMismatch`.

Remove the `Network::Testnet4` branch from `expected_bits` and its unit test.
Every supported retargeting network uses the parent's `bits` as the base. Drop
testnet4 from the finality unit-test loop. Make `rootstock.rs::for_network`
return `None` for the refused pair, and comment the `rsk.rs` fallback as
unreachable.

### D6. Version floor: declared, not fixed

Core rejects `version < 2/3/4` after the BIP34, BIP66 and BIP65 activations. On
mainnet the embedded spine is far above those heights, so only a header on an
old fork could carry an old version. On testnet3 and regtest the seed is
genesis, so the skip is reachable from low heights. This client does not check
versions. A version-1 header is accepted here and rejected by Core. It gains no
work, because the version is part of the header hash. Keep the skip and record
it in the ledger in D9. A follow-up can add the height-activated check from
`Params::{bip34,bip66,bip65}_height`.

### D7. Test layout

```text
crates/core/specs/
  mod.rs                       declares the test modules
  support.rs                   shared mining and Core builders (new)
  bitcoin_header_validation.rs rule scenarios, HDR-00..HDR-08 (new)
  bitcoin_headers.rs           the four state machines only (smaller)
  rootstock_checkpoints.rs     unchanged
```

- `support.rs` takes `mine`, `mine_bad`, `mine_with_bits`, `mainnet_params`,
  `synthetic_mainnet_core`, `delivery_core`, `mainnet_core`, `work` and
  `canonical_chain` from `bitcoin_headers.rs`. The universes, models and
  transitions stay where they are.
- `bitcoin_header_validation.rs` holds one `#[test]` per requirement, split
  where a fixture is expensive. It also takes
  `median_time_past_excludes_the_header_itself`,
  `the_target_range_check_runs_before_proof_of_work` and
  `a_header_exactly_two_hours_ahead_is_accepted` from the machine file. Those
  are rule tests, not sequence tests.
- `store_identity_matches_the_spec` (O1) and
  `the_finality_requirement_gates_the_mark` (E6) stay in the machine file until
  the chain and finality capabilities exist.
- The file name mirrors the capability path, so the ID-to-file mapping is
  obvious.

`mod.rs` documents one file per capability, not one file per Markdown document.

### D8. Fixture map

| Requirement | Fixture | Cost | Note |
|---|---|---|---|
| HDR-00 | open a file-backed core on each network | 0 blocks | assert `UnsupportedNetwork` and no file for the refused pair |
| HDR-01 | anchor + 1 header with an unknown parent | 1 block | parent lookup fails first |
| HDR-02 | anchor + 1 header for each malformed compact | 1 block each | range runs before PoW |
| HDR-03 | anchor + 2 headers (bad hash, good hash) | 2 | valid range |
| HDR-04 | mainnet anchor + parent + child | 2 | re-mine the wrong-bits child |
| HDR-05 | testnet3 params, relaxed limit, anchor + 3 | 3 | normal bits must differ from the relaxed limit |
| HDR-06 mainnet | boundary anchor + 2015 + boundary | ~16 s | quarter timespan; the hard boundary mine dominates |
| HDR-06 regtest | reuse the stored chain with regtest params | 0 extra blocks | no retarget; the seed must start at a boundary |
| HDR-07 full | boundary anchor + 11 + probe | 12 | equality and exclusion |
| HDR-07 sparse | anchor + 1 | 1 | deviation |
| HDR-08 | anchor + 1 per case | 1 | inclusive boundary |

Rules for negative tests:

- Start from a valid baseline. Change one field. Re-mine when the change alters
  the hash or the target.
- Assert the exact `Error` variant, and the detail where the spec names one.
- If an earlier rule necessarily fails first, either repair the fixture or
  assert only that the submission is refused. Do not assert a later rule's error
  through a shadowed fixture.
- Only HDR-06 needs the 2015-block chain. Build it once and reuse the store for
  the regtest cases; the seed must start at a period boundary, so an anchor at
  2015 is not possible.
- When the intended rule runs before proof of work, the fixture needs no valid
  hash. HDR-06 uses that for the unclamped value.

The new scenario file does not use the `without_difficulty_check` seam. The
delivery machine still uses it to keep `BadTarget` reachable.

### D9. Reported order and acceptance equivalence

The implementation order is: link, expected difficulty, median-time-past,
future time, target range, proof of work. Bitcoin Core checks proof of work and
the target range first, then the parent, then difficulty, MTP, future time and
version. It also checks the testnet4 timewarp; this capability excludes that
rule with testnet4 itself.

The contract is **the same accept/reject verdict for the same header, view and
clock**, not the same first error. The scenario tests pin each rule's error by
isolating it. The design records the order so a later change does not "fix" it
by accident.

Known differences are recorded in the ledger below. Everything else
should match Core for the same view.

| Difference | Scope | Direction | Follow-up |
|---|---|---|---|
| Testnet4 and signet | the whole network | refused at open | add a requirement if an RSK network needs one |
| Sparse median-time-past window | heights `seed+1` through `seed+11`, any fork | accepts more | option (a), context headers |
| Version floor | above the BIP34/66/65 heights | accepts more | a later change |
| Error order | all rules | the error differs, the verdict is equal | none |

### D10. Retarget arithmetic: accepted dependency risk

The retarget formula comes from `rust-bitcoin`. The synthetic boundary fixture
uses a base at the relaxed proof-of-work limit, so `parent_target * timespan`
overflows 256 bits. Its expected value is the library's wrapped result, not a
real retarget. No test checks the formula against Bitcoin Core.

Accept that risk for this change. The dependency is widely used, and the normal
mainnet path does not overflow. If a future change needs independent proof of
the formula, add a real mainnet vector then.

A differential harness against `bitcoind submitheader` is out of scope. It
cannot exercise the retarget on regtest (`no_pow_retargeting`), and it needs its
own fixture set. Keep it as deferred work in `tasks.md`.

### D11. Traceability and the legacy documents

The OpenSpec capability is the only normative source. Traceability is direct:
the requirement header carries `[HDR-xx]`, the scenario test name carries the
same number (`hdr_07_median_time_past`), and a `**Source**` metadata line under
each requirement links the Bitcoin Core function and reject reason. OpenSpec
skips metadata lines for the 500-character requirement text, and archive keeps
the raw block, so the link survives into the main spec. `openspec show` prints
the parsed text and omits the metadata; the Markdown and archived spec keep it.
There is no separate verification map. A renamed test is a visible break, and
the archived change keeps the decision record.

The repository-root `specs/` and `docs/` stay read-only. No task edits them, and
no task reads them as an input. `crates/core/specs/` is the executable test
directory and stays. If the OpenSpec workflow proves out, delete the legacy
files in a separate cleanup.

Deferred work is recorded as plain bullets in `tasks.md`. After archive, the
durable spec is `openspec/specs/bitcoin-header-validation/spec.md`.

### D12. Terminology: submission

The capability calls the act of handing a header batch to the core a
**submission**. Bitcoin Core's `submitheader` RPC uses "submit" and describes a
header "as a candidate chain tip if valid". Its internal header check is
`AcceptBlockHeader`. "Connect" is Core's block and chainstate verb
(`ConnectBlock`, `ConnectTip`) and Electrum's header verb (`can_connect`), but
"connection" also names the P2P connection. Use "submission" for the act and
"accepted" or "rejected" for the verdict.

The public API keeps `connect_headers` in this change. A separate
`rename-header-submission` change with `skip_specs: true` renames
`ConnectHeaders` to `SubmitHeaders`, `ConnectOutcome` to `SubmitOutcome`,
`Outcome::Connected` to `Outcome::Submitted`, and the `connected` count to
`accepted`. That is 138 occurrences in 10 files. The capability spec does not
name the API, so the two changes are independent.

## Risks / Trade-offs

- **[MTP skip accepts non-Core headers near the seed]** → bounded to 11 heights
  above a trusted boundary; no work advantage; stated in HDR-07 and the ledger
  in D9; option (a) remains a follow-up.
- **[Retarget arithmetic is not independently verified]** → the formula comes
  from `rust-bitcoin`, and the synthetic fixture's base overflows the 256-bit
  multiply. Accepted for this change; add a vector later if needed.
- **[A new `Error` variant breaks exhaustive matches]** → check in-workspace
  callers; the crate is not published.
- **[The mainnet boundary fixture costs ~17 s]** → one grouped test; the
  regtest cases reuse the store.
- **[A future RSK network uses testnet4 or signet]** → both are refused today;
  add a requirement and its tests in a new change. The IDs are append-only.
- **[Version floor is not checked]** → declared deviation; no work gain; a
  follow-up can add it from `Params` heights.
- **[The legacy `specs/` and `docs/` go stale]** → they are read-only, no task
  depends on them, and a separate cleanup can delete them.
- **[The spec says submission while the API says `connect_headers` until the
  rename]** → the spec does not name the API; a separate `skip_specs` change
  renames it.

## Migration Plan

No data migration. The change adds one error variant (`UnsupportedNetwork`)
and moves test code between modules. A store seeded for testnet4 or signet is
no longer openable; no shipped deployment uses them. Rollback is a revert of the
change; the delta spec leaves no trace in `openspec/specs/` until archive.

## Open Questions

None. The version-floor decision is recorded above with a follow-up; it does
not change this capability's requirements or task order.
