# Terminology

Two chains live in this workspace, and the word "checkpoint" names several
different things. Never write it bare; say which one.

| Term to use | What it means | Code |
|---|---|---|
| **Bitcoin period boundary** | a difficulty-period start, every 2016 Bitcoin blocks | `btc::finality::is_checkpoint`, `finalized_checkpoint(s)` |
| **trusted checkpoint** | a configured header the crate assumes correct, one per Bitcoin period boundary; written when the store is initialized | `Config::checkpoints`, `btc::checkpoints`, `put_checkpoints` |
| **checkpoint table** | `btc_checkpoints`: one header per period boundary; starts with the trusted checkpoints and grows with validated root-chain boundaries; the whole-chain work ladder | `btc_checkpoints`, `checkpoint_at` |
| **finality mark** | the newest Bitcoin height treated as un-reorgable (not a boundary) | `finalized_height`, `set_finalized` |
| **CPV sample** | one of the seven 64-block-spaced RSK heights a tag commits to | `CPV_INTERVAL` (was `CHECKPOINT`), `oldest_checkpoint`, `cpv_matches` |
| **RSK checkpoint** | a proposed RSK block whose merge-mining anchor sits in the finalized Bitcoin tail | `propose_checkpoint`, `attempt_checkpoint`, `EphemeralChainCheckpoint` |
| **difficulty anchor** | a lower bound on cumulative RSK difficulty at a height, used to reject inflated total-difficulty claims (vendored code) | `DifficultyCheckpoint`, `CheckpointVerdict` |

Rules:

- **"checkpoint" never appears bare.** Use one of the terms above.
- **"header" never appears bare in a mixed document.** Say "Bitcoin header" or
  "RSK header"; in Bitcoin-only or RSK-only sections the bare word is fine.
- **"initialize" means writing the trusted checkpoints into an empty store.**
  An existing store must agree with them. Do not say "seed", "seeding" or
  "anchor" for this act or for the store state. `checkpoint_at` keeps its name.
- **"anchor" is not a trusted-checkpoint term.** It stays in two places: the
  finality mark (the mark is the anchor of the measurement) and the RSK
  merge-mining anchor. Qualify it in both.
- **"seed" only names DNS peer seeds.** It does not name the checkpoints, the
  store state or the test fixture.
- The RSK checkpoint and the Bitcoin period boundary are unrelated concepts
  that share a word. The CPV sample and the difficulty anchor are two more.

`rsk-consensus` is vendored from `rustock-core`, so its public names
(`DifficultyCheckpoint`, `CheckpointVerdict`, module `checkpoint`) are kept as
upstream spells them. In our own prose they are "difficulty anchors".
