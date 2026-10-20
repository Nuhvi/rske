//! The embedded Bitcoin checkpoint spine.
//!
//! One trusted header per difficulty period — heights `0, 2016, 4032, …` — one
//! lowercase-hex 80-byte header per line, line `i` at height
//! `i * DIFFICULTY_PERIOD`. It is the trust anchor for a fast sync: seeding
//! these lets a node skip the whole-history download (the Electrum model) while
//! still PoW-, continuity-, difficulty- and timestamp-validating every header
//! above the spine.
//!
//! The file holds only **finalized** checkpoints — each one met the finality
//! rule in force when the file was generated (see [`crate::btc::finality`]), so
//! none of them is subject to reorg. The tip's nearest boundary is deliberately
//! not included.
//!
//! The file lives next to this crate (`data/checkpoints.txt`), is compiled in,
//! and is regenerated from a synced store with `rsk-cli export-checkpoints`.

use bitcoin::block::Header as BitcoinHeader;
use bitcoin::consensus::Decodable;

use crate::error::Error;
use crate::store::DIFFICULTY_PERIOD;

/// The embedded checkpoint spine, one hex header per line in height order.
const CHECKPOINTS: &str = include_str!("../../data/checkpoints.txt");

/// The raw spine file contents (one hex header per line).
pub fn text() -> &'static str {
    CHECKPOINTS
}

/// The spine as `(height, header)`, line `i` at height `i * DIFFICULTY_PERIOD`.
pub fn parse() -> Result<Vec<(u64, BitcoinHeader)>, Error> {
    let mut out = Vec::new();
    for (i, line) in CHECKPOINTS
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
    {
        let bytes = hex::decode(line.trim()).map_err(|e| Error::CheckpointFormat {
            line: i,
            detail: e.to_string(),
        })?;
        if bytes.len() != 80 {
            return Err(Error::CheckpointFormat {
                line: i,
                detail: format!("{} bytes, want 80", bytes.len()),
            });
        }
        let mut reader = bytes.as_slice();
        let header =
            BitcoinHeader::consensus_decode(&mut reader).map_err(|e| Error::CheckpointFormat {
                line: i,
                detail: e.to_string(),
            })?;
        out.push((i as u64 * DIFFICULTY_PERIOD, header));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_spine_parses_contiguously_from_genesis() {
        let spine = parse().expect("embedded checkpoints parse");
        // The file may be trimmed for experiments, so only the shape
        // is asserted: contiguous whole periods from the real genesis. The age
        // of the tip is visible at runtime (`finality: mark #… (age …)`).
        assert!(
            spine.len() >= 2,
            "the spine needs at least two checkpoints, got {}",
            spine.len()
        );
        assert_eq!(spine[0].0, 0);
        // Line i is at height i * 2016, contiguous.
        for window in spine.windows(2) {
            assert_eq!(window[1].0, window[0].0 + DIFFICULTY_PERIOD);
        }
        // Line 0 is the real mainnet genesis.
        assert_eq!(
            spine[0].1.block_hash().to_string(),
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"
        );
    }
}
