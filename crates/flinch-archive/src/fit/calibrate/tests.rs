use super::*;
use rstest::rstest;

/// Deterministic Bernoulli draws: SplitMix64, so the data never changes.
fn draws(n: usize, slope: f64, intercept: f64) -> (Vec<f32>, Vec<f32>) {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    };
    (0..n)
        .map(|i| {
            let x = -4.0 + 8.0 * i as f64 / n as f64;
            let label = if next() < sigmoid(slope * x + intercept) { 1.0 } else { 0.0 };
            (x as f32, label)
        })
        .unzip()
}

#[test]
fn it_recovers_a_known_mapping_from_enough_rows() {
    let (logits, labels) = draws(20_000, 0.9, 2.0);
    let fitted = fit(&logits, &labels);
    assert!((fitted.slope - 0.9).abs() < 0.08, "slope {}", fitted.slope);
    assert!((fitted.intercept - 2.0).abs() < 0.12, "intercept {}", fitted.intercept);
}

#[test]
fn without_rows_the_priors_keep_their_deployed_mapping() {
    assert_eq!(fit(&[], &[]), Calibration::deployed());
}

#[rstest]
#[case::every_row_safe(1.0)]
#[case::every_row_played(0.0)]
fn a_single_outcome_stays_finite_and_increasing(#[case] label: f32) {
    let logits: Vec<f32> = (0..200).map(|i| -3.0 + i as f32 * 0.03).collect();
    let fitted = fit(&logits, &vec![label; logits.len()]);
    assert!(fitted.intercept.is_finite() && fitted.slope.is_finite(), "{fitted:?}");
    assert!(fitted.slope >= MIN_SLOPE as f32, "the transform must stay increasing: {fitted:?}");
}

#[test]
fn the_scorecard_form_gives_the_same_probability() {
    let calibration = Calibration { slope: 0.8, intercept: 1.7 };
    let (bias, temperature) = calibration.bias_and_temperature();
    for logit in [-6.0f32, -1.0, 0.0, 0.5, 3.0, 9.0] {
        let direct = sigmoid(f64::from(calibration.slope * logit + calibration.intercept));
        let as_scorecard = sigmoid(f64::from((logit + bias) / temperature));
        assert!((direct - as_scorecard).abs() < 1e-5, "logit {logit}: {direct} vs {as_scorecard}");
    }
}
