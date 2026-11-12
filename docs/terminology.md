# Terminology

Two chains live in this workspace, and the word "checkpoint" is used for four
different things. Never write it bare; say which one.

| Term to use | What it means | Code |
|---|---|---|
| **Bitcoin period boundary** | a difficulty-period start, every 2016 Bitcoin blocks | `btc::finality::is_checkpoint`, `btc_checkpoints` table, `finalized_checkpoint(s)` |
| **Bitcoin spine** | the trusted period-boundary headers the crate seeds as the anchor | `btc::checkpoints`, `ensure_anchor`, `CheckpointFormat` |
| **finality mark** | the newest Bitcoin height treated as un-reorgable (not a boundary) | `finalized_height`, `set_finalized` |
| **CPV sample** | one of the seven 64-block-spaced RSK heights a tag commits to | `CPV_INTERVAL` (was `CHECKPOINT`), `oldest_checkpoint`, `cpv_matches` |
| **RSK checkpoint** | a proposed RSK block whose merge-mining anchor sits in the finalized Bitcoin tail | `propose_checkpoint`, `attempt_checkpoint`, `EphemeralChainCheckpoint` |
| **difficulty anchor** | a lower bound on cumulative RSK difficulty at a height, used to reject inflated total-difficulty claims (vendored code) | `DifficultyCheckpoint`, `CheckpointVerdict` |

Rules:

- **"checkpoint" never appears bare.** Use one of the six terms above.
- **"header" never appears bare in a mixed document.** Say "Bitcoin header" or
  "RSK header"; in Bitcoin-only or RSK-only sections the bare word is fine.
- The RSK checkpoint (trust anchor) and the Bitcoin period boundary are
  unrelated concepts that happen to share a word; the CPV sample is a third,
  and the difficulty anchor a fourth.

`rsk-consensus` is vendored from `rustock-core`, so its public names
(`DifficultyCheckpoint`, `CheckpointVerdict`, module `checkpoint`) are kept as
upstream spells them. In our own prose they are "difficulty anchors".
