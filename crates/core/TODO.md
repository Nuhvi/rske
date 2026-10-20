# rsk-core — TODO

Next steps, roughly in order. The crate is the side-effect-free core; network
I/O and orchestration live outside it (p2p / sync layers).

## Rootstock tail sync

- [ ] **`UncleGuard` window.** `Core::connect_rsk_headers` currently requires a
  batch in ascending order with each parent already stored, so the uncle rule's
  `AncestorSource` can walk the ancestry. Port rustock's `UncleGuard` (a bounded
  window of the uncle lists a header-only client has seen) so out-of-order or
  partial batches still validate, and credit no uncle work when the ancestry is
  incomplete. Possibly unnecessary for the first IBD if the walk feeds ascending
  runs with each parent already stored; revisit once real syncing shows the
  access pattern.
- [x] **Checkpoint attempt on connect.** `connect_rsk_headers` measures every
  proposed checkpoint's GHOST work against `2/3` of the Bitcoin tail and reports
  the one that clears ([`RskConnectOutcome::checkpoint`]). `propose_checkpoint`
  validates the proposed anchor (canonical + in tail) rather than trusting it.
- [ ] **Persist the established checkpoint.** Today the attempt recomputes and
  returns it each connect; store it (RSK meta) so pruning and status can read it.
- [ ] **Pruning.** Once a checkpoint is established, drop the other proposed
  trees and the Bitcoin headers below the tail start (keep the previous
  finalized boundary so the condition stays re-derivable). Opt-in.
- [ ] **Merge-mining proof pruning.** Stored RSK headers carry the merged-mining
  merkle proof and coinbase (~600 B each). After validation, decide whether to
  keep them (to re-serve) or drop them.

## Bitcoin finality (the mark)

- [x] **The mark and its bar.** Finalization is a monotonic pointer
  (`finalized_height`) to the newest header treated as un-reorgable, kept
  `FINALITY_MARGIN_BLOCKS` (100) below the tip; a **checkpoint** is the newest
  difficulty-period start at or below it. The mark advances when the work above
  it (the anchor) clears `max(100·w0, W_est(t0, now))` — the coinbase window and the estimated
  work the network did since the mark from the bottom price/dbtc law. `now` is
  supplied by the caller (`connect_headers`), and the store keeps the greatest
  clock seen so winding it back cannot shrink the bar. See `btc::finality`.
- [x] **Re-anchor on the mark.** The anchor is the mark, not the compiled-in
  spine tip: a returning node measures from its own finalized chain (a shorter
  age, so a smaller bar), and each advance re-anchors so `T` restarts at the
  newest trusted point. Only a first-ever sync (or a rolled-back store) starts
  from the shipped checkpoint.
- [x] **Adopt a newer shipped spine on resume.** The compiled-in spine was only
  read when seeding, so a store seeded by an older/shorter spine never used a
  newer one and stayed stuck on its old mark (a 2014 anchor kept refusing even
  after the binary shipped 2026 checkpoints). `Core::reanchor_shipped` now moves
  the mark forward to the newest trusted checkpoint the store can prove it
  holds (exact hash at that height), forward-only, logged. The ceiling is the
  invocation's own `--cut` prefix, so resume ≡ fresh under the same flags.
- [x] **Estimated-work bar (price power law).** `bar = max(100·w0, W_est)`, where
  `W_est` is the cumulative work implied by the bottom Bitcoin price power law
  (converted to hashrate with the frozen dbtc constants) over the anchor's age,
  and `w0` is the anchor block's work. Absolute, not fitted to the spine: the
  bottom price law is the low end of the estimate and sits below the realised
  work on the sampled 2016+ era, so the sampled guarantee is a **5% network
  share** (`Φ = bar/honest` above `s/(1−s)` and at most 1). Before 2014 the law
  is above the realised work, so an ancient anchor simply waits. The 100-block
  floor keeps `T = 0` non-zero. No sample window, no era/horizon guard: if the
  bar is above the honest work the mark does not advance (delayed finalization),
  which is the designed liveness/safety trade.
- [ ] **Non-mainnet finality policy.** Mainnet uses the price-law estimated-work
  bar; testnet/regtest use the linear reference rule because the price law is a
  mainnet calibration. Decide whether a light client on testnet should instead
  fail closed or adopt a network-specific law.
- [ ] **Rootstock bar.** `attempt_checkpoint` measures RSK GHOST work against a
  *relative* `2/3` of the Bitcoin tail — the same fixed-bar shape the Bitcoin
  rule just moved away from, and the tail it measures against is not yet
  settled. Add an absolute estimated-work term anchored on the RSK side
  (`rsk-consensus::checkpoint::MAINNET_CHECKPOINT` is a shipped `(t0, D0)`
  pair), calibrated for RSK's block time and difficulty divisor.


## Bitcoin P2P

 - [ ] P2P best practices, research and implement
    - [ ] remove peers 

## Observability / ergonomics

- [ ] **`export-checkpoints` should be read-only.** It opens the store with
  `Database::create`, so exporting from a missing directory creates an empty
  store and then fails `store is not seeded`; with a sync running it fails on
  the lock and writes nothing. Refuse to create and say "no store at PATH".
- [ ] **Spine-length-independent tests.** Several tests assume the embedded
  spine is long and modern: the shape check, the shipped-bar tests, and the
  `rsk` tests that slice `checkpoints::parse()`. Build synthetic fixtures (or
  gate on the spine's length and era) so trimming `checkpoints.txt` for
  experiments does not turn the suite red.

## Rootstock finality (reorg safety)

Two candidate definitions to pin down and implement:

- [ ] **Bitcoin-style finality.** Same shape as Bitcoin finality: 100 BTC blocks
  worth of difficulty. Measured by counting 100 BTC blocks merge-mined with the
  Rootstock trunk chain.
- [ ] **Checkpointing with strong-subjectivity guarantees.** Prove that no miner
  with `≥1%` of hash rate is actively creating a fork. Measured by watching the
  most recent `N` `rskblock` tags and requiring `99%` of those `N` points to the
  same trunk. Requires Armadillo's CPV and cross links (with the 20-byte block
  id). `N` must be large enough for statistical significance: to detect a 1% fork
  with 99% confidence, roughly `N ≥ 461` `rskblock` tags — however far back that
  reaches, depending on what fraction of BTC miners are mining Rootstock.
  (Confirm the 461 figure and the confidence math.)
- [ ] **Tag agreement rule.** Call two `rskblock` tags in agreement on any block's
  CPV when 3 consecutive bytes including the byte in question agree, since faking
  some 3 bytes in the CPV costs `(1/256)^3`. They should also agree on all blocks
  before it; the 3 bytes are the minimal overlap needed (I guess).
- [ ] **Armadillo (RSKIP-110) bounds the merge-mining ratio from below.** The CPV
  is 7 bytes at 64-RSK-block spacing = a 448-RSK-block window ≈ 11.2 BTC blocks; a
  tag's CPV only shares bytes with the previous tag's if the next tag lands inside
  that window. So if the ratio of merge mining is too small the tags are too far
  between and the CPV fields lose their overlap, which breaks the 3-byte agreement
  rule (and cross-tag comparison generally). Roughly, `q ≳ 1/11.2 ≈ 9%` for
  overlap. Consequence for the count above: the very-low-`q` regime is infeasible,
  so the detection window stays ~460 BTC blocks but the tag count is `~460·q`
  (≥ ~41 at `q = 9%`), not the 461-at-`q=1` only.

## Validation

- [ ] **Vetiver.** Every block we validate is newer than `VETIVER-731b9d8`
  (`F` is always newer given >67% participation), so pre-Vetiver rules and the
  activation-height branches that only apply to older blocks can be dropped.
- [ ] **Real-header tests.** `connect_rsk_headers` is exercised only on synthetic
  headers today; add a fixture with real mainnet Rootstock headers (with uncles)
  to cover the full validation path.

## p2p (outside rsk-core)

- [ ] **Fetch with uncles.** `rsk/63` `BlockHeadersWithUncles` (27/28), else
  headers + bodies for uncles.
- [ ] **Bisect for `F`.** Per peer/fork, bisect canonical heights for the deepest
  in-tail anchor, then download `[F..tip]`.
- [ ] **Status work-at-F.** Advertise the cumulative work at `F` (a sixth status
  field) so a peer's `[F..tip]` work can be previewed before downloading.
