//! When a difficulty-period boundary is settled — the finality mark.
//!
//! ## The mark
//!
//! Finalization is a **mark**: a pointer to the newest header we treat as
//! un-reorgable. Nothing is pruned when it advances, and nothing outside the
//! crate reads it as consensus — it exists so a later Rootstock sync has a
//! trusted Bitcoin prefix to cross-link against. Its only teeth are the
//! [`crate::Error::ForkBelowFinalized`] refusal.
//!
//! A **checkpoint** is a boundary header (a difficulty-period start) at or
//! below that mark: [`is_checkpoint`]. The mark itself may be *any* height — it
//! is a reorg depth, not a boundary — and the newest checkpoint is simply the
//! last period start the mark has reached. The store's `btc_checkpoints` table
//! is the broader *boundary ladder* — every period start, settled or not —
//! which is what makes the whole chain's work computable from the spine; only
//! the part of it at or below the mark is a checkpoint.
//!
//! ## The bar
//!
//! The mark advances only once the work above it (the **anchor**) reaches
//!
//! ```text
//! need = max( REVERSAL_BLOCKS · w0 , W_est(t0, now) )
//! ```
//!
//! where `w0` is the work of one block at the anchor's difficulty,
//! [`REVERSAL_BLOCKS`] is the coinbase-maturity floor, and `W_est` is the
//! **estimated work the network did since t0**, in hashes, from the Bitcoin
//! price power law converted to hashrate with the dbtc relation
//! ([`estimated_work`]).
//!
//! The bar is *absolute*: it does not depend on how much work the candidate
//! happens to carry, only on the trusted anchor's age and the power law. The
//! price law is the **bottom** variant, the low end of the estimate; over the
//! sampled 2016+ history it sits below the realised work, so the honest chain
//! clears it while a fork with a small network share does not (before 2014 it is
//! above the realised work, so an ancient anchor simply waits). The sampled
//! guarantee is about a **5% network share**: a fork holds `s/(1-s)` of the
//! honest work, and the bar sits above that line while staying at or below the
//! honest work. Over-estimating only delays finalization.
//!
//! The floor keeps `T = 0` from being a zero-work hole. The mark sits
//! `FINALITY_MARGIN_BLOCKS` below the tip, so in steady state `T` is short and
//! the floor is usually the binding term; the estimated work takes over when the
//! mark has been stagnant. Re-anchoring resets `T` but not the floor.
//!
//! ## The anchor is the mark
//!
//! The anchor is the finality mark itself, not the compiled-in spine tip. Any
//! history the node already trusts anchors the next measurement, so a node that
//! has synced before measures from its own finalized chain, and each advance
//! re-anchors. The compiled-in spine is the anchor only for a first-ever sync:
//! it seeds the mark.
//!
//! ## The clock
//!
//! `T = now − t0` comes from the node's clock, which is therefore a trust input:
//! a backward clock skew lowers the bar. The store remembers the greatest
//! `now` it has seen, so winding it back cannot shrink the bar. The work of a
//! chain is `Σ D · 2³²` over its blocks, so the mark tracks exactly the work the
//! network had to hash.

use alloy_primitives::U256;
use bitcoin::pow::Work;

use crate::store::DIFFICULTY_PERIOD;

/// The coinbase-maturity floor: a boundary never settles for less than the work
/// of this many blocks at the anchor's difficulty.
pub const REVERSAL_BLOCKS: u64 = 100;

/// Bitcoin's target block interval, in seconds.
pub const TARGET_BLOCK_SECONDS: u64 = 600;

/// A Julian year, in seconds.
pub const SECONDS_PER_YEAR: f64 = 31_557_600.0;

/// Bottom price law `P(d) = 10^(A + N log10 d)`, USD/BTC, `d` days since
/// genesis. The low end of the price estimate; the study's security bound is
/// taken at this level.
const PRICE_A: f64 = -17.432;
const PRICE_N: f64 = 5.834;

/// The frozen dbtc price -> difficulty constants.
const DBTC_D0: f64 = 54_621_616_002.157_09;
const DBTC_P0: f64 = 455.910_174_203_227_37;
const DBTC_B: f64 = 0.673_473_301_558_825_5;

/// `p = n / b`: the power-law exponent of hashrate in chain age. The estimated
/// hashrate is `H(t) ∝ age(t)^p`, so its cumulative integral is closed-form.
const PRICE_P: f64 = PRICE_N / DBTC_B;

/// The finality policy: the work a chain must carry above the anchor, given the
/// anchor's work per block, its header time, the network genesis time and the
/// caller's clock.
///
/// Injected into [`crate::Core`] so mainnet can use the price-law
/// [`estimated_work_bar`], while non-mainnet networks and the store-mechanics
/// tests use the linear [`linear_reference_bar`].
pub type Bar = fn(anchor_work: Work, anchor_time: u64, genesis_time: u64, now: u64) -> U256;

/// Whether `height` is a checkpoint: it starts a difficulty period.
///
/// Whether it is *finalized* is a separate question — see
/// [`crate::Core::finalized_checkpoints`], which intersects this with the mark.
pub fn is_checkpoint(height: u64) -> bool {
    height.is_multiple_of(DIFFICULTY_PERIOD)
}

/// The shipped bar: the greater of the coinbase window and the estimated work
/// the network did since the anchor.
pub fn estimated_work_bar(
    anchor_work: Work,
    anchor_time: u64,
    genesis_time: u64,
    now: u64,
) -> U256 {
    let floor = work_to_u256(anchor_work).saturating_mul(U256::from(REVERSAL_BLOCKS));
    floor.max(estimated_work(anchor_time, genesis_time, now))
}

/// Cumulative work the bottom price law says the network did over
/// `[anchor_time, now]`, in hashes.
///
/// The power law gives a hashrate `H(t) = C · age(t)^p`; integrating that from
/// `t0` to `now` is closed-form:
///
/// ```text
/// W_est = H(t0) · a0 · YEAR / (p + 1) · ( (1 + T_y / a0)^(p + 1) − 1 )
/// ```
///
/// with `a0` the anchor's age in years and `T_y` the elapsed years. The result
/// saturates at [`U256::MAX`] for absurd extrapolations, which fails closed.
pub fn estimated_work(anchor_time: u64, genesis_time: u64, now: u64) -> U256 {
    f64_to_u256(estimated_work_f(anchor_time, genesis_time, now))
}

fn estimated_work_f(anchor_time: u64, genesis_time: u64, now: u64) -> f64 {
    let age_years = anchor_time.saturating_sub(genesis_time) as f64 / SECONDS_PER_YEAR;
    if age_years <= 0.0 || now <= anchor_time {
        return 0.0;
    }
    let h0 = estimated_hashrate(anchor_time, genesis_time);
    let elapsed_years = (now - anchor_time) as f64 / SECONDS_PER_YEAR;
    let exponent = PRICE_P + 1.0;
    h0 * age_years * SECONDS_PER_YEAR / exponent
        * ((1.0 + elapsed_years / age_years).powf(exponent) - 1.0)
}

/// Estimated hashrate at `t`, in hashes per second, from the bottom price law
/// and the frozen dbtc conversion.
fn estimated_hashrate(t: u64, genesis_time: u64) -> f64 {
    let days = t.saturating_sub(genesis_time) as f64 / 86_400.0;
    if days <= 0.0 {
        return 0.0;
    }
    let price = 10f64.powf(PRICE_A + PRICE_N * days.log10());
    let difficulty = DBTC_D0 * (price / DBTC_P0).powf(1.0 / DBTC_B);
    difficulty * (1u64 << 32) as f64 / TARGET_BLOCK_SECONDS as f64
}

/// The linear reference rule, `w0 · max(100, T/600)`.
///
/// Used for non-mainnet networks — the price law is a mainnet calibration — and
/// by the store-mechanics tests, whose synthetic chains at trivial difficulty
/// could never clear an absolute real-world bar. It is a development policy, not
/// the 5% guarantee; the mechanics (mark monotonicity, re-anchoring, fork
/// refusal, adoption) are policy-independent.
pub(crate) fn linear_reference_bar(
    anchor_work: Work,
    anchor_time: u64,
    _genesis_time: u64,
    now: u64,
) -> U256 {
    let blocks = (now.saturating_sub(anchor_time) / TARGET_BLOCK_SECONDS).max(REVERSAL_BLOCKS);
    work_to_u256(anchor_work).saturating_mul(U256::from(blocks))
}

/// The bar as seen from one connect: what the best chain carries above the
/// anchor and what the rule demands. Reported on [`crate::ConnectOutcome`] so a
/// driver can say *why* a boundary has not settled yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalityStatus {
    /// The anchor height the bar is measured from.
    pub anchor: u64,
    /// The anchor's header time.
    pub anchor_time: u64,
    /// Work of one block at the anchor's difficulty — the unit of the bar.
    pub anchor_work: Work,
    /// Work of the best chain above the anchor.
    pub have: Work,
    /// Work the bar requires above the anchor.
    pub need: U256,
}

impl FinalityStatus {
    /// Whether the candidate chain clears the bar.
    pub fn clears(&self) -> bool {
        work_to_u256(self.have) >= self.need
    }

    /// `have` in blocks at the anchor's work per block.
    pub fn have_blocks(&self) -> U256 {
        let unit = work_to_u256(self.anchor_work);
        if unit.is_zero() {
            return U256::ZERO;
        }
        work_to_u256(self.have) / unit
    }

    /// `need` in blocks at the anchor's work per block.
    pub fn need_blocks(&self) -> U256 {
        let unit = work_to_u256(self.anchor_work);
        if unit.is_zero() {
            return U256::ZERO;
        }
        self.need / unit
    }

    /// `have / need` in whole percent, capped at 999 so a log line stays short.
    pub fn progress_percent(&self) -> u64 {
        if self.need.is_zero() {
            return 100;
        }
        let pct = work_to_u256(self.have).saturating_mul(U256::from(100u64)) / self.need;
        if pct > U256::from(999u64) {
            999
        } else {
            pct.as_limbs()[0]
        }
    }

    /// `Φ = need / have`: the bar relative to the work carried. Below one the
    /// honest chain clears it; a fork with network share `s` is kept out while
    /// `Φ > s/(1-s)`. `None` when the chain carries no work yet.
    pub fn phi(&self) -> Option<f64> {
        let have = u256_to_f64(work_to_u256(self.have));
        (have > 0.0).then(|| u256_to_f64(self.need) / have)
    }
}

/// A [`Work`] as a [`U256`], for arithmetic [`Work`] does not carry (it has no
/// `Mul`, and the bar multiplies by a block count).
pub fn work_to_u256(work: Work) -> U256 {
    U256::from_be_bytes(work.to_be_bytes())
}

/// A [`U256`] as an `f64`.
fn u256_to_f64(value: U256) -> f64 {
    let mut out = 0.0f64;
    for limb in value.as_limbs().iter().rev() {
        out = out * 18_446_744_073_709_551_616.0 + *limb as f64;
    }
    out
}

/// A positive `f64` as a [`U256`]. Saturates at the `u128` range, far above any
/// plausible estimate; non-finite values fail closed at the maximum.
fn f64_to_u256(value: f64) -> U256 {
    if !value.is_finite() || value >= u128::MAX as f64 {
        return U256::MAX;
    }
    if value <= 0.0 {
        return U256::ZERO;
    }
    U256::from(value as u128)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::pow::Target;

    /// The mainnet genesis time the price law is defined on.
    const GENESIS: u64 = 1_231_006_505;

    fn work_from_bits(bits: u32) -> Work {
        Target::from_compact(bitcoin::CompactTarget::from_consensus(bits)).to_work()
    }

    #[test]
    fn checkpoints_are_period_starts() {
        assert!(is_checkpoint(0));
        assert!(is_checkpoint(DIFFICULTY_PERIOD));
        assert!(is_checkpoint(2 * DIFFICULTY_PERIOD));
        assert!(!is_checkpoint(1));
        assert!(!is_checkpoint(DIFFICULTY_PERIOD - 1));
        assert!(!is_checkpoint(DIFFICULTY_PERIOD + 1));
    }

    /// At the anchor instant the estimate is zero, so the coinbase floor is the
    /// whole bar.
    #[test]
    fn the_coinbase_floor_binds_at_zero_age() {
        let w = work_from_bits(0x1702_1ec5);
        let t0 = 1_789_801_745;
        assert_eq!(
            estimated_work_bar(w, t0, GENESIS, t0),
            work_to_u256(w) * U256::from(REVERSAL_BLOCKS)
        );
        assert_eq!(estimated_work(t0, GENESIS, t0), U256::ZERO);
    }

    /// The closed-form integral must match a numeric integration of the same
    /// power law, or the constants and the exponent are wrong.
    #[test]
    fn the_closed_form_matches_numeric_integration() {
        let t0 = 1_700_000_000u64;
        let days = 200u64;
        let now = t0 + days * 86_400;
        let analytic = estimated_work_f(t0, GENESIS, now);

        let steps = 20_000u64;
        let dt = (now - t0) as f64 / steps as f64;
        let numeric: f64 = (0..steps)
            .map(|i| estimated_hashrate(t0 + (i as f64 * dt) as u64, GENESIS) * dt)
            .sum();
        let relative = (analytic - numeric).abs() / analytic;
        assert!(
            relative < 1e-4,
            "analytic {analytic:.6e} vs numeric {numeric:.6e} ({relative:.2e})"
        );
    }

    /// A month after the shipped spine tip the bar is a few thousand checkpoint
    /// blocks — well below the `T/600` age term, because the bottom law prices
    /// the network below its realised work.
    #[test]
    fn a_modern_anchor_is_priced_below_its_age() {
        let spine = crate::btc::checkpoints::parse().expect("embedded spine parses");
        let (_, header) = *spine.last().expect("non-empty spine");
        let w0 = header.target().to_work();
        let t0 = header.time as u64;
        let now = t0 + 31 * 86_400;

        let blocks = estimated_work_bar(w0, t0, GENESIS, now) / work_to_u256(w0);
        let age_blocks = 31 * 86_400 / TARGET_BLOCK_SECONDS;
        assert!(
            blocks > U256::from(2_000u64) && blocks < U256::from(3_500u64),
            "bar {blocks} blocks"
        );
        assert!(
            blocks < U256::from(age_blocks),
            "the bottom law must price below the age term"
        );
    }

    #[test]
    fn the_bar_grows_with_the_clock() {
        let w = work_from_bits(0x1702_1ec5);
        let t0 = 1_789_801_745;
        let day = 86_400u64;
        assert!(
            estimated_work_bar(w, t0, GENESIS, t0 + 30 * day)
                < estimated_work_bar(w, t0, GENESIS, t0 + 60 * day)
        );
        assert!(
            estimated_work_bar(w, t0, GENESIS, t0 + 60 * day)
                < estimated_work_bar(w, t0, GENESIS, t0 + 365 * day)
        );
    }

    /// The reference bar used by the store tests is the old linear rule.
    #[test]
    fn the_linear_reference_is_the_old_rule() {
        let w = work_from_bits(0x207f_ffff);
        let t0 = 1_600_000_000;
        assert_eq!(
            linear_reference_bar(w, t0, GENESIS, t0),
            work_to_u256(w) * U256::from(REVERSAL_BLOCKS)
        );
        assert_eq!(
            linear_reference_bar(w, t0, GENESIS, t0 + 30 * 86_400),
            work_to_u256(w) * U256::from(4_320u64)
        );
    }
}
