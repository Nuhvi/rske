//! Bitcoin header consensus rules for the SPV canonical chain.
//!
//! These are the checks a header-only client can and should make: Electrum's
//! `Blockchain.verify_header` / `get_target` plus the Bitcoin Core contextual
//! rules that make sense without full blocks.
//!
//! Checked (in [`crate::btc`], which supplies the surrounding headers):
//!
//! - **Proof of work** — the block hash meets the target encoded in `bits`
//!   (`Header::validate_pow`).
//! - **Expected difficulty** — `bits` is exactly the target the retarget
//!   schedule calls for at this height. Within a difficulty period every block
//!   carries its period's target; at a period boundary the target is recomputed
//!   from the previous period (Core's `GetNextWorkRequired`, Electrum's
//!   `get_target`). At a period boundary the retarget clamps to the network's
//!   proof-of-work limit.
//! - **Median-time-past** — the timestamp is strictly greater than the median
//!   of the previous [`MEDIAN_TIME_PAST`] blocks (Core's
//!   `ContextualCheckBlockHeader`).
//! - **Future time** — the timestamp is at most [`MAX_FUTURE_BLOCK_TIME`]
//!   seconds after the caller's clock.
//!
//! Deliberately **not** checked:
//!
//! - **Target range and proof-of-work limit** — `validate_pow` compares only the
//!   hash with the target. See C6 in `specs/bitcoin-headers.md`.
//! - **Transaction / merkle validity** — not visible in a header; out of scope.
//! - **Version rules** (BIP34/65/66/9) — height-activated and of no SPV value.
//! - **Retargeting on testnet/regtest** — those networks permit
//!   minimum-difficulty blocks, so "`bits` equals the schedule" does not hold.
//!   Electrum skips them entirely; so do we (see [`enforces_difficulty`]).

use bitcoin::block::Header as BitcoinHeader;
use bitcoin::consensus::Params;
use bitcoin::pow::CompactTarget;

use crate::store::DIFFICULTY_PERIOD;

/// Number of prior blocks whose timestamps form the median-time-past.
pub const MEDIAN_TIME_PAST: usize = 11;

/// The maximum time a header may lead the caller's clock, in seconds.
///
/// Bitcoin Core uses two hours. The header time is not checked against a
/// network-adjusted clock; the caller supplies `now` on every connect.
pub const MAX_FUTURE_BLOCK_TIME: u64 = 2 * 60 * 60;

/// Whether the retarget rule (expected `bits`) is enforced on this network.
///
/// Testnet and regtest allow minimum-difficulty blocks, so their `bits` do not
/// follow the schedule; like Electrum we skip the difficulty rule there. The
/// production network (mainnet) and signet are enforced.
pub fn enforces_difficulty(params: &Params) -> bool {
    !params.no_pow_retargeting && !params.allow_min_difficulty_blocks
}

/// The `bits` the header at `height` must carry, given its parent.
///
/// Off a difficulty-period boundary, every block in the period carries the
/// same target as its parent. On a boundary (`height % DIFFICULTY_PERIOD == 0`)
/// the target is recalculated from the previous period: its first block
/// (`height - DIFFICULTY_PERIOD`, `period_start`) and its last block (the
/// parent). Returns `None` when `period_start` is not available — e.g. it lies
/// below a trusted checkpoint — in which case the caller skips the check.
pub fn expected_bits(
    height: u64,
    parent: &BitcoinHeader,
    period_start: Option<&BitcoinHeader>,
    params: &Params,
) -> Option<CompactTarget> {
    if !height.is_multiple_of(DIFFICULTY_PERIOD) {
        return Some(parent.bits);
    }
    let first = period_start?;
    Some(CompactTarget::from_header_difficulty_adjustment(
        *first, *parent, params,
    ))
}

/// The median-time-past of exactly [`MEDIAN_TIME_PAST`] timestamps: the sixth
/// smallest (Core's `GetMedianTimePast` uses `nMedian = 11 / 2`). No
/// allocation — the hot path calls this once per connected header.
pub fn median_time_past(mut times: [u32; MEDIAN_TIME_PAST]) -> u32 {
    times.sort_unstable();
    times[MEDIAN_TIME_PAST / 2]
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::Network;
    use bitcoin::block::Version;
    use bitcoin::hashes::Hash;

    fn header_with(time: u32, bits: u32) -> BitcoinHeader {
        BitcoinHeader {
            version: Version::ONE,
            prev_blockhash: bitcoin::block::BlockHash::all_zeros(),
            merkle_root: Hash::all_zeros(),
            time,
            bits: CompactTarget::from_consensus(bits),
            nonce: 0,
        }
    }

    #[test]
    fn difficulty_is_enforced_only_where_the_schedule_holds() {
        assert!(!enforces_difficulty(&Params::new(Network::Regtest)));
        assert!(!enforces_difficulty(&Params::new(Network::Testnet)));
        assert!(enforces_difficulty(&Params::new(Network::Bitcoin)));
        assert!(enforces_difficulty(&Params::new(Network::Signet)));
    }

    #[test]
    fn off_boundary_bits_follow_the_parent() {
        let params = Params::new(Network::Bitcoin);
        let parent = header_with(1_600_000_000, 0x1d00_f3b3);
        assert_eq!(
            expected_bits(5, &parent, None, &params),
            Some(CompactTarget::from_consensus(0x1d00_f3b3)),
        );
    }

    #[test]
    fn boundary_bits_are_recomputed_from_the_previous_period() {
        let params = Params::new(Network::Bitcoin);
        let first = header_with(1_600_000_000, 0x1d00_ffff);
        // A full target timespan later, so the retarget is a no-op — but the
        // point is which headers feed the calculation.
        let parent = header_with(1_600_000_000 + 1_209_600, 0x1d00_ffff);
        let expected = CompactTarget::from_header_difficulty_adjustment(first, parent, &params);
        assert_eq!(
            expected_bits(DIFFICULTY_PERIOD, &parent, Some(&first), &params),
            Some(expected),
        );
        assert_eq!(
            expected_bits(DIFFICULTY_PERIOD, &parent, None, &params),
            None,
        );
    }

    #[test]
    fn median_of_eleven_is_the_sixth_smallest() {
        assert_eq!(median_time_past([5, 1, 3, 2, 4, 9, 7, 8, 6, 0, 10]), 5);
        assert_eq!(median_time_past([10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0]), 5);
    }
}
