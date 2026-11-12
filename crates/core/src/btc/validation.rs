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
//! - **Target range** — `bits` decodes without a negative, zero or overflowing
//!   target, and the target is at or below the network proof-of-work limit.
//! - **Expected difficulty** — `bits` is exactly the target the retarget
//!   schedule calls for at this height (Core's `GetNextWorkRequired`, Electrum's
//!   `get_target`). Inside a period that is the parent's bits; at a period
//!   boundary the target is recomputed. Testnet's minimum-difficulty rule and
//!   testnet4's BIP94 retarget are included; regtest never retargets. The
//!   retarget clamps to the network's proof-of-work limit.
//! - **Median-time-past** — the timestamp is strictly greater than the median
//!   of the previous [`MEDIAN_TIME_PAST`] blocks (Core's
//!   `ContextualCheckBlockHeader`).
//! - **Future time** — the timestamp is at most [`MAX_FUTURE_BLOCK_TIME`]
//!   seconds after the caller's clock.
//!
//! Deliberately **not** checked:
//!
//! - **Transaction / merkle validity** — not visible in a header; out of scope.
//! - **Version rules** (BIP34/65/66/9) — height-activated and of no SPV value.

use bitcoin::Network;
use bitcoin::block::Header as BitcoinHeader;
use bitcoin::consensus::Params;
use bitcoin::pow::{CompactTarget, Target};

use crate::store::DIFFICULTY_PERIOD;

/// Number of prior blocks whose timestamps form the median-time-past.
pub const MEDIAN_TIME_PAST: usize = 11;

/// Whether `bits` decodes to a valid target at or below the network limit.
///
/// This mirrors Bitcoin Core's `DeriveTarget`: reject a negative mantissa, a
/// zero target, an overflowing exponent and a target above `params.pow_limit`.
pub fn target_in_range(bits: CompactTarget, params: &Params) -> bool {
    let raw = bits.to_consensus();
    let size = raw >> 24;
    let mantissa = raw & 0x007f_ffff;
    if raw & 0x0080_0000 != 0 || mantissa == 0 {
        return false;
    }
    if size > 34 || (mantissa > 0xff && size > 33) || (mantissa > 0xffff && size > 32) {
        return false;
    }
    Target::from_compact(bits) <= params.max_attainable_target
}

/// The maximum time a header may lead the caller's clock, in seconds.
///
/// Bitcoin Core uses two hours. The header time is not checked against a
/// network-adjusted clock; the caller supplies `now` on every connect.
pub const MAX_FUTURE_BLOCK_TIME: u64 = 2 * 60 * 60;

/// The `bits` the header at `height` must carry.
///
/// Inside a period the parent's bits are expected, except on a network that
/// permits minimum-difficulty blocks: there a header more than twice the target
/// spacing after its parent must carry the proof-of-work limit, and any other
/// header expects the bits of the last non-minimum-difficulty block in the
/// period (see [`last_normal_bits`](crate::btc)). At a period boundary the
/// retarget runs; testnet4 starts from the period-start bits (BIP94), every
/// other network from the parent's, and regtest keeps the parent's.
///
/// Returns `None` at a boundary without a period-start header. The connect path
/// treats that as an internal error: a seed at a period boundary makes it
/// unreachable.
pub fn expected_bits(
    height: u64,
    header_time: u32,
    parent: &BitcoinHeader,
    period_start: Option<&BitcoinHeader>,
    last_normal_bits: CompactTarget,
    params: &Params,
) -> Option<CompactTarget> {
    if !height.is_multiple_of(DIFFICULTY_PERIOD) {
        if params.allow_min_difficulty_blocks {
            let gap =
                u64::from(header_time) > u64::from(parent.time) + params.pow_target_spacing * 2;
            return Some(if gap {
                params.max_attainable_target.to_compact_lossy()
            } else {
                last_normal_bits
            });
        }
        return Some(parent.bits);
    }

    if params.no_pow_retargeting {
        return Some(parent.bits);
    }
    let first = period_start?;
    let last_bits = if params.network == Network::Testnet4 {
        first.bits
    } else {
        parent.bits
    };
    let timespan = u64::from(parent.time).saturating_sub(u64::from(first.time));
    Some(CompactTarget::from_next_work_required(
        last_bits, timespan, params,
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

    /// Mainnet and signet: inside a period the parent's bits are expected.
    #[test]
    fn off_boundary_bits_follow_the_parent() {
        let params = Params::new(Network::Bitcoin);
        let parent = header_with(1_600_000_000, 0x1d00_f3b3);
        assert_eq!(
            expected_bits(5, parent.time + 1, &parent, None, parent.bits, &params),
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
            expected_bits(
                DIFFICULTY_PERIOD,
                parent.time + 1,
                &parent,
                Some(&first),
                parent.bits,
                &params,
            ),
            Some(expected),
        );
        assert_eq!(
            expected_bits(
                DIFFICULTY_PERIOD,
                parent.time + 1,
                &parent,
                None,
                parent.bits,
                &params
            ),
            None,
        );
    }

    /// Testnet: a header more than twice the target spacing after its parent
    /// must carry the proof-of-work limit, whatever the last normal block had.
    #[test]
    fn a_minimum_difficulty_gap_requires_the_pow_limit() {
        let params = Params::new(Network::Testnet);
        let parent = header_with(1_600_000_000, 0x1d00_ffff);
        let expected = params.max_attainable_target.to_compact_lossy();
        assert_eq!(
            expected_bits(
                5,
                parent.time + 2 * 600 + 1,
                &parent,
                None,
                CompactTarget::from_consensus(0x1d00_ffff),
                &params,
            ),
            Some(expected),
        );
    }

    /// Testnet: without a gap, a normal block after a minimum-difficulty block
    /// carries the last normal bits, not the parent's minimum-difficulty bits.
    #[test]
    fn a_minimum_difficulty_parent_uses_the_last_normal_bits() {
        let params = Params::new(Network::Testnet);
        let parent = header_with(1_600_000_000, 0x1d00_ffff);
        let last_normal = CompactTarget::from_consensus(0x1c00_ffff);
        assert_eq!(
            expected_bits(5, parent.time + 1, &parent, None, last_normal, &params),
            Some(last_normal),
        );
    }

    /// Testnet4 (BIP94) starts the retarget from the period-start bits; testnet3
    /// starts from the parent's.
    #[test]
    fn testnet4_takes_the_retarget_from_the_period_start() {
        let first = header_with(1_600_000_000, 0x1d00_ffff);
        let parent = header_with(1_600_000_000 + 1_209_600, 0x1c00_ffff);
        let testnet3 = Params::new(Network::Testnet);
        let testnet4 = Params::new(Network::Testnet4);
        let e3 = expected_bits(
            DIFFICULTY_PERIOD,
            parent.time + 1,
            &parent,
            Some(&first),
            parent.bits,
            &testnet3,
        );
        let e4 = expected_bits(
            DIFFICULTY_PERIOD,
            parent.time + 1,
            &parent,
            Some(&first),
            parent.bits,
            &testnet4,
        );
        // A full timespan makes the retarget a no-op: the starting bits survive.
        assert_eq!(e3, Some(parent.bits));
        assert_eq!(e4, Some(first.bits));
        assert_ne!(e3, e4);
    }

    /// Regtest never retargets, so a boundary keeps the parent's bits.
    #[test]
    fn regtest_keeps_the_parent_bits() {
        let params = Params::new(Network::Regtest);
        let first = header_with(1_600_000_000, 0x207f_ffff);
        let parent = header_with(1_600_000_100, 0x207f_ffff);
        assert_eq!(
            expected_bits(
                DIFFICULTY_PERIOD,
                parent.time + 1,
                &parent,
                Some(&first),
                parent.bits,
                &params,
            ),
            Some(parent.bits),
        );
    }

    #[test]
    fn the_mainnet_pow_limit_rejects_easier_targets() {
        let mainnet = Params::new(Network::Bitcoin);
        // Difficulty 1 is exactly the mainnet limit.
        assert!(target_in_range(
            CompactTarget::from_consensus(0x1d00_ffff),
            &mainnet
        ));
        // The next representable step up is above it.
        assert!(!target_in_range(
            CompactTarget::from_consensus(0x1d01_0000),
            &mainnet
        ));
        // A target regtest accepts is refused on mainnet.
        assert!(!target_in_range(
            CompactTarget::from_consensus(0x207f_ffff),
            &mainnet
        ));
        assert!(target_in_range(
            CompactTarget::from_consensus(0x207f_ffff),
            &Params::new(Network::Regtest)
        ));
    }

    #[test]
    fn malformed_targets_are_refused() {
        let params = Params::new(Network::Regtest);
        // The sign bit is set.
        assert!(!target_in_range(
            CompactTarget::from_consensus(0x1d80_0000),
            &params
        ));
        // A zero mantissa is not a target.
        assert!(!target_in_range(
            CompactTarget::from_consensus(0x1d00_0000),
            &params
        ));
        // An exponent past the 256-bit range overflows.
        assert!(!target_in_range(
            CompactTarget::from_consensus(0x2300_0001),
            &params
        ));
    }

    #[test]
    fn median_of_eleven_is_the_sixth_smallest() {
        assert_eq!(median_time_past([5, 1, 3, 2, 4, 9, 7, 8, 6, 0, 10]), 5);
        assert_eq!(median_time_past([10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0]), 5);
    }
}
