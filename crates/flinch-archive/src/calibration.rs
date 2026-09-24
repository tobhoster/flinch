//! Calibration of the plan's delete probabilities.
//!
//! The central rule: **calibration is reported with sharpness, never alone.**
//! A model that emits the base rate for every item is perfectly calibrated and
//! completely useless. Calibration without sharpness is the metric that most
//! often certifies a dead model.

#[derive(Debug, thiserror::Error)]
pub enum ProbabilityError {
    #[error("probability {0} is not a finite value in [0.0, 1.0]")]
    OutOfRange(f32),
}

/// A probability in `[0.0, 1.0]`, validated once at construction.
///
/// `NaN` can never exist inside one, so no downstream gate has to re-validate
/// or handle an unordered comparison.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Probability(f32);

impl Probability {
    pub const ZERO: Self = Self(0.0);

    pub fn new(value: f32) -> Result<Self, ProbabilityError> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ProbabilityError::OutOfRange(value))
        }
    }

    #[inline]
    pub const fn get(self) -> f32 {
        self.0
    }
}

/// One (prediction, outcome) pair. `outcome` is the ground-truth label the
/// prediction is scored against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Observation {
    pub predicted: f32,
    pub outcome: bool,
}

/// One bucket of a reliability diagram.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReliabilityBin {
    pub count: usize,
    pub mean_predicted: f32,
    pub mean_observed: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationReport {
    pub samples: usize,
    /// Mean squared error of the probability. Proper scoring rule: it is
    /// minimised only by the true probability, which is why it — not "accuracy" —
    /// is the headline number for a probability estimator.
    pub brier: f32,
    /// Expected calibration error over equal-mass bins.
    pub ece: f32,
    /// Standard deviation of the predictions. Reported ALONGSIDE `ece` because a
    /// constant predictor scores a perfect `ece` of 0.
    pub sharpness: f32,
    /// Base rate of the outcome, for comparison against `sharpness`.
    pub base_rate: f32,
    pub reliability: Vec<ReliabilityBin>,
}

/// Brier score. Empty input yields `None` rather than a fabricated zero, which
/// would silently read as a perfect score.
pub fn brier(observations: &[Observation]) -> Option<f32> {
    if observations.is_empty() {
        return None;
    }
    let total: f32 = observations
        .iter()
        .map(|o| {
            let truth = if o.outcome { 1.0 } else { 0.0 };
            let error = o.predicted - truth;
            error * error
        })
        .sum();
    Some(total / observations.len() as f32)
}

/// Expected calibration error over `bin_count` **equal-mass** bins.
///
/// Equal-mass rather than equal-width: with equal-width bins on a skewed
/// prediction distribution most bins are empty and the score is dominated by
/// whichever bin happened to catch the tail.
pub fn expected_calibration_error(observations: &[Observation], bin_count: usize) -> Option<f32> {
    let bins = reliability_bins(observations, bin_count)?;
    let total = observations.len() as f32;
    Some(
        bins.iter()
            .map(|bin| {
                let weight = bin.count as f32 / total;
                weight * (bin.mean_predicted - bin.mean_observed).abs()
            })
            .sum(),
    )
}

/// Equal-mass reliability buckets, ordered by ascending predicted probability.
pub fn reliability_bins(
    observations: &[Observation],
    bin_count: usize,
) -> Option<Vec<ReliabilityBin>> {
    if observations.is_empty() || bin_count == 0 {
        return None;
    }
    let mut sorted: Vec<Observation> = observations.to_vec();
    // `total_cmp` rather than `partial_cmp`: predictions reaching here have
    // already been validated finite, but total_cmp cannot panic even if that
    // ever changes.
    sorted.sort_by(|a, b| a.predicted.total_cmp(&b.predicted));

    let bin_count = bin_count.min(sorted.len());
    let mut bins = Vec::with_capacity(bin_count);
    let mut start = 0usize;
    for index in 0..bin_count {
        // Spread the remainder over the leading bins so no bin is empty.
        let end = sorted.len() * (index + 1) / bin_count;
        let slice = &sorted[start..end];
        if slice.is_empty() {
            continue;
        }
        let count = slice.len();
        let mean_predicted = slice.iter().map(|o| o.predicted).sum::<f32>() / count as f32;
        let observed = slice.iter().filter(|o| o.outcome).count() as f32 / count as f32;
        bins.push(ReliabilityBin { count, mean_predicted, mean_observed: observed });
        start = end;
    }
    Some(bins)
}

/// Standard deviation of the predictions.
pub fn sharpness(observations: &[Observation]) -> Option<f32> {
    if observations.is_empty() {
        return None;
    }
    let n = observations.len() as f32;
    let mean = observations.iter().map(|o| o.predicted).sum::<f32>() / n;
    let variance = observations
        .iter()
        .map(|o| {
            let delta = o.predicted - mean;
            delta * delta
        })
        .sum::<f32>()
        / n;
    Some(variance.sqrt())
}

pub fn calibration_report(observations: &[Observation], bin_count: usize) -> Option<CalibrationReport> {
    let brier = brier(observations)?;
    let ece = expected_calibration_error(observations, bin_count)?;
    let sharpness = sharpness(observations)?;
    let reliability = reliability_bins(observations, bin_count)?;
    let base_rate =
        observations.iter().filter(|o| o.outcome).count() as f32 / observations.len() as f32;
    Some(CalibrationReport {
        samples: observations.len(),
        brier,
        ece,
        sharpness,
        base_rate,
        reliability,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observations(pairs: &[(f32, bool)]) -> Vec<Observation> {
        pairs
            .iter()
            .map(|&(predicted, outcome)| Observation { predicted, outcome })
            .collect()
    }

    #[test]
    fn probability_rejects_non_finite_and_out_of_range() {
        for bad in [f32::NAN, f32::INFINITY, -0.001, 1.001] {
            assert!(
                Probability::new(bad).is_err(),
                "{bad} must not be accepted as a probability"
            );
        }
        assert!(Probability::new(0.0).is_ok());
        assert!(Probability::new(1.0).is_ok());
    }

    #[test]
    fn empty_input_yields_none_not_a_flattering_zero() {
        assert!(brier(&[]).is_none());
        assert!(expected_calibration_error(&[], 15).is_none());
        assert!(sharpness(&[]).is_none());
        assert!(calibration_report(&[], 10).is_none());
    }

    #[test]
    fn brier_is_zero_for_a_perfect_predictor_and_one_for_a_confidently_wrong_one() {
        let perfect = observations(&[(1.0, true), (0.0, false)]);
        assert!(brier(&perfect).expect("non-empty") < 1e-6);

        let inverted = observations(&[(0.0, true), (1.0, false)]);
        assert!((brier(&inverted).expect("non-empty") - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_constant_predictor_at_the_base_rate_is_perfectly_calibrated_but_has_no_sharpness() {
        // Half the outcomes true, every prediction 0.5. This is the failure mode
        // sharpness exists to catch: ECE says "perfect", and the model is useless.
        let pairs: Vec<(f32, bool)> = (0..100).map(|i| (0.5f32, i % 2 == 0)).collect();
        let report = calibration_report(&observations(&pairs), 10).expect("non-empty");
        assert!(report.ece < 0.02, "constant-at-base-rate scores a near-perfect ECE");
        assert!(report.sharpness < 1e-6, "and zero sharpness, which is what damns it");
    }

    #[test]
    fn ece_detects_systematic_overconfidence() {
        // Always predicts 0.9; only 50% actually occur.
        let pairs: Vec<(f32, bool)> = (0..100).map(|i| (0.9f32, i % 2 == 0)).collect();
        let ece = expected_calibration_error(&observations(&pairs), 10).expect("non-empty");
        assert!(
            (ece - 0.4).abs() < 0.02,
            "predicting 0.9 when 0.5 occur is a 0.4 calibration gap, got {ece}"
        );
    }

    #[test]
    fn reliability_bins_are_equal_mass_and_cover_every_sample() {
        let pairs: Vec<(f32, bool)> = (0..100)
            .map(|i| (i as f32 / 100.0, i % 3 == 0))
            .collect();
        let bins = reliability_bins(&observations(&pairs), 10).expect("non-empty");
        assert_eq!(bins.len(), 10);
        assert_eq!(bins.iter().map(|b| b.count).sum::<usize>(), 100);
        assert!(bins.iter().all(|b| b.count == 10), "equal-mass bins");
    }
}
