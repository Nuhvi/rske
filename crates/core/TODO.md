# rsk-core — TODO

Open work only. Completed behaviour is described by the tests and the
specifications in `specs/`; add future consensus rules there before implementing
them.

## Rootstock ingestion

- [ ] **`UncleGuard` window.** `Core::connect_rsk_headers` requires an ascending
  batch with each parent already stored so the uncle rule can walk the ancestry.
  Port rustock's bounded `UncleGuard` so out-of-order or partial batches still
  validate, and credit no uncle work when the ancestry is incomplete. The IBD
  path does not need it today: `rsk::ibd` stages header-only batches with
  `connect_rsk_headers_without_uncles`.
- [ ] **Persist the established checkpoint.** The attempt recomputes and returns
  the clearing checkpoint on every connect; store it in RSK meta so pruning and
  status can read it.
- [ ] **RSK pruning.** Once a checkpoint is established, drop the other proposed
  trees and the Bitcoin headers below the tail start (keep the previous
  finalized boundary so the condition stays re-derivable). Opt-in.
- [ ] **Merged-mining proof retention.** Stored RSK headers carry the
  merged-mining proof and coinbase (~600 B each). Decide whether to keep them to
  re-serve or drop them after validation.

## Finality

- [ ] **Non-mainnet finality policy.** Mainnet uses the price-law estimated-work
  bar; testnet/regtest use the linear reference rule because the price law is a
  mainnet calibration. Decide whether a light client on testnet should fail
  closed or use a network-specific law.
- [ ] **Absolute Rootstock bar.** `attempt_checkpoint` measures RSK GHOST work
  against a relative `2/3` of the Bitcoin tail, and that tail is not yet
  settled. Add an absolute estimated-work term anchored on the RSK side
  (`rsk-consensus::checkpoint::MAINNET_CHECKPOINT` is a shipped `(t0, D0)`
  pair), calibrated for RSK's block time and difficulty divisor.

## IBD policy

- [ ] **Confirm `honest_share` semantics.** The same `H` sizes the sample
  (`N` from `(1 − H)^N`) and sets the endorsement bar (`(1 − H)·N`). Decide
  whether those are one parameter or two; the current code treats them as one.
- [ ] **Multiple peer chains.** IBD stages one peer's header chain per pass;
  forks discovered from other peers are not merged. Either merge them or make
  the single-peer assumption explicit in the driver.
- [ ] **Forward mode.** Continuous forward sync is not built. The current
  fetch/scoring split is ready for it.

## Observability / ergonomics

- [ ] **`export-checkpoints` should be read-only.** It opens the store with
  `Database::create`, so exporting from a missing directory creates an empty
  store and then fails with `NoChain`; with a sync running it fails on
  the lock. Refuse to create and report "no store at PATH".
- [ ] **Checkpoint-file-independent tests.** Some tests assume the embedded
  trusted checkpoints are long and modern. Build synthetic fixtures or gate on
  the file's length and era so trimming `checkpoints.txt` for experiments does
  not turn the suite red.

## Validation

- [ ] **Vetiver.** Every block we validate is newer than `VETIVER-731b9d8`
  (`F` is always newer given >67% participation), so pre-Vetiver rules and the
  activation-height branches that only apply to older blocks can be dropped.
- [ ] **Real-header tests.** `connect_rsk_headers_without_uncles` exercises real
  header-only batches, but the full `connect_rsk_headers` path (uncle-aware) is
  still tested only on synthetic headers. Add a fixture with real mainnet
  Rootstock headers and uncles.

## p2p (outside rsk-core)

- [ ] **Fetch with uncles.** `rsk/63` `BlockHeadersWithUncles` (27/28), else
  headers + bodies for uncles.
- [ ] **Bisect for `F`.** Per peer/fork, bisect canonical heights for the
  deepest in-tail anchor, then download `[F..tip]`.
- [ ] **Status work-at-F.** Advertise the cumulative work at `F` (a sixth status
  field) so a peer's `[F..tip]` work can be previewed before downloading.
- [ ] **P2P peer handling.** Research current best practices; start with peer
  removal/eviction.
