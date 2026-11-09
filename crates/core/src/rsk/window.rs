//! The tag window: how many tags a PoW-fraud-proof sample needs.

/// A probability in `(0, 1)`: an honest hash-rate share, a tail probability.
///
/// Constructing one is the validation: [`Share::new`] returns `None` for
/// anything outside the open interval or not finite, so code that takes a
/// `Share` never has to check again and never has to panic.
#[allow(dead_code)] // wired to the window config next
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
struct Share(f64);

#[allow(dead_code)] // wired to the window config next
impl Share {
    /// `None` unless `value` is finite and strictly between 0 and 1.
    fn new(value: f64) -> Option<Self> {
        (value.is_finite() && value > 0.0 && value < 1.0).then_some(Self(value))
    }

    fn get(self) -> f64 {
        self.0
    }
}

/// One-sided 5σ tail probability (`Φ(-5) ≈ 2.87e-7`), the error budget the
/// finality rule is stated against: roughly one wrong call in 3.5 million.
#[allow(dead_code)] // wired to the window config next
const FIVE_SIGMA_ALPHA: Share = Share(2.87e-7);

/// The smallest tag sample that gives a PoW-fraud-proof guarantee at an
/// assumed honest share.
///
/// If `honest` of the merge-miners tag honestly, the chance that `n` tags
/// contain none of them is `(1 - honest)^n`. This returns the least `n` that
/// pushes that chance to `alpha` or below — the sample a light client needs
/// before "no honest tag" is a 5σ event. At 5σ (`alpha = 2.87e-7`) and
/// `honest = 0.33` that is 38 tags; at `honest = 0.10`, 143.
///
/// The inputs are shares in `(0, 1)`; the result is at least 1.
#[allow(dead_code)] // wired to the window config next
fn min_tags_for_fraud_proof(honest: Share, alpha: Share) -> u64 {
    let n = (alpha.get().ln() / (1.0 - honest.get()).ln()).ceil();
    if n.is_finite() { n.max(1.0) as u64 } else { 1 }
}

#[cfg(test)]
mod tag_tests {
    use super::{FIVE_SIGMA_ALPHA, Share, min_tags_for_fraud_proof};


    #[test]
    fn thirty_three_percent_honest_needs_38_tags() {
        assert_eq!(min_tags_for_fraud_proof(Share::new(0.33).unwrap(), FIVE_SIGMA_ALPHA), 38);
    }

    #[test]
    fn ten_percent_honest_needs_143_tags() {
        assert_eq!(min_tags_for_fraud_proof(Share::new(0.10).unwrap(), FIVE_SIGMA_ALPHA), 143);
    }

    #[test]
    fn half_honest_needs_22_tags() {
        assert_eq!(min_tags_for_fraud_proof(Share::new(0.50).unwrap(), FIVE_SIGMA_ALPHA), 22);
    }

    #[test]
    fn two_thirds_honest_needs_14_tags() {
        assert_eq!(min_tags_for_fraud_proof(Share::new(2.0 / 3.0).unwrap(), FIVE_SIGMA_ALPHA), 14);
    }

    #[test]
    fn result_is_the_first_n_that_clears_alpha() {
        for honest in [0.1, 0.25, 0.33, 0.5, 0.75] {
            let h = Share::new(honest).unwrap();
            let alpha = FIVE_SIGMA_ALPHA.get();
            let n = min_tags_for_fraud_proof(h, FIVE_SIGMA_ALPHA);
            let at = (1.0 - honest).powi(n as i32);
            let below = (1.0 - honest).powi(n as i32 - 1);
            assert!(at <= alpha, "honest {honest}: (1-h)^{n} = {at:e}");
            assert!(below > alpha, "honest {honest}: (1-h)^{} = {below:e}", n - 1);
        }
    }

    #[test]
    fn less_honest_needs_more_tags() {
        let a = min_tags_for_fraud_proof(Share::new(0.33).unwrap(), FIVE_SIGMA_ALPHA);
        let b = min_tags_for_fraud_proof(Share::new(0.10).unwrap(), FIVE_SIGMA_ALPHA);
        assert!(b > a);
    }
}
