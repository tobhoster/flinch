//! Metric tests: fixed tables for the numbers a reader can check by hand,
//! and invariants over generated rows for the properties every metric owes.

use super::*;
use proptest::prelude::*;
use rstest::rstest;

#[rstest]
#[case::perfect_ranking(&[0.9, 0.8, 0.2, 0.1], 1.0)]
#[case::inverted_ranking(&[0.1, 0.2, 0.8, 0.9], 0.0)]
// All scores equal: no discrimination at all, not a broken metric.
#[case::all_tied(&[0.5, 0.5, 0.5, 0.5], 0.5)]
// One positive tied with one negative: that pair is worth half a win.
#[case::one_cross_class_tie(&[0.9, 0.5, 0.5, 0.1], 0.875)]
fn auc_ranks_safe_rows_above_played_ones(#[case] scores: &[f32], #[case] expected: f32) {
    let labels = [1.0, 1.0, 0.0, 0.0];
    assert!((auc(scores, &labels) - expected).abs() < 1e-6, "got {}", auc(scores, &labels));
}

#[test]
fn a_single_class_cannot_be_ranked() {
    assert!((auc(&[0.9, 0.8], &[1.0, 1.0]) - 0.5).abs() < 1e-6);
    assert!((auc(&[], &[]) - 0.5).abs() < 1e-6);
}

#[rstest]
#[case::coin_flip(0.5, 1.0, std::f32::consts::LN_2)]
#[case::confident_and_right(0.9, 1.0, 0.105_360_5)]
#[case::confident_and_wrong(0.9, 0.0, std::f32::consts::LN_10)]
// Certainty in the wrong answer is clipped, not infinite.
#[case::certain_and_wrong(1.0, 0.0, 13.815_51)]
fn log_loss_prices_confidence(#[case] score: f32, #[case] label: f32, #[case] expected: f32) {
    let loss = log_loss(&[score], &[label]);
    assert!((loss - expected).abs() < 1e-4, "got {loss}");
}

#[test]
fn ece_notices_an_overconfident_model() {
    // Says 0.9, right half the time.
    let scores = [0.9, 0.9, 0.9, 0.9];
    let labels = [1.0, 1.0, 0.0, 0.0];
    let error = ece(&scores, &labels, ECE_BINS);
    assert!((error - 0.4).abs() < 1e-5, "got {error}");
}

/// The definition AUC must agree with: the share of (safe, played) pairs
/// ranked the right way round, ties counting half.
fn pairwise_auc(scores: &[f32], labels: &[f32]) -> f32 {
    let mut wins = 0.0f64;
    let mut pairs = 0.0f64;
    for (positive, _) in scores.iter().zip(labels).filter(|(_, label)| **label >= 0.5) {
        for (negative, _) in scores.iter().zip(labels).filter(|(_, label)| **label < 0.5) {
            pairs += 1.0;
            wins += if positive > negative {
                1.0
            } else if positive == negative {
                0.5
            } else {
                0.0
            };
        }
    }
    if pairs == 0.0 {
        0.5
    } else {
        (wins / pairs) as f32
    }
}

/// Rows with probabilities on a 1/1000 grid, so ties are common and every
/// transform below keeps distinct scores distinct in f32.
fn rows() -> impl Strategy<Value = (Vec<f32>, Vec<f32>)> {
    proptest::collection::vec((0u32..=1000, any::<bool>()), 1..200)
        .prop_map(|rows| rows.into_iter().map(|(grid, safe)| (grid as f32 / 1000.0, if safe { 1.0 } else { 0.0 })).unzip())
}

proptest! {
    #[test]
    fn brier_is_a_proper_mean_squared_error((scores, labels) in rows()) {
        let score = brier(&scores, &labels);
        prop_assert!((0.0..=1.0).contains(&score), "brier {score}");
        prop_assert!(log_loss(&scores, &labels) >= 0.0);
    }

    #[test]
    fn perfect_predictions_score_perfectly((_, labels) in rows()) {
        let perfect = Scorecard::of(&labels, &labels);
        prop_assert_eq!(perfect.brier, 0.0);
        prop_assert_eq!(perfect.ece, 0.0);
        prop_assert!(perfect.log_loss < 1e-5, "log-loss {}", perfect.log_loss);
        let both_classes = labels.iter().any(|label| *label >= 0.5) && labels.iter().any(|label| *label < 0.5);
        prop_assert_eq!(perfect.auc, if both_classes { 1.0 } else { 0.5 });
    }

    #[test]
    fn auc_depends_on_the_ordering_alone((scores, labels) in rows()) {
        let base = auc(&scores, &labels);
        prop_assert!((base - pairwise_auc(&scores, &labels)).abs() < 1e-5);
        for transform in [f32::sqrt as fn(f32) -> f32, |p: f32| p * p, |p: f32| 0.5 * p + 0.25] {
            let moved: Vec<f32> = scores.iter().map(|p| transform(*p)).collect();
            prop_assert_eq!(auc(&moved, &labels).to_bits(), base.to_bits());
        }
        let reversed: Vec<f32> = scores.iter().map(|p| 1.0 - p).collect();
        let both_classes = labels.iter().any(|label| *label >= 0.5) && labels.iter().any(|label| *label < 0.5);
        if both_classes {
            prop_assert!((auc(&reversed, &labels) - (1.0 - base)).abs() < 1e-5, "reversing the order mirrors AUC");
        }
    }

    #[test]
    fn predicting_the_base_rate_is_perfectly_calibrated((_, labels) in rows()) {
        let base_rate = labels.iter().sum::<f32>() / labels.len() as f32;
        let constant = vec![base_rate; labels.len()];
        let error = ece(&constant, &labels, ECE_BINS);
        prop_assert!(error < 1e-5, "ece {error} at base rate {base_rate}");
        prop_assert!((auc(&constant, &labels) - 0.5).abs() < 1e-6, "a constant ranks nothing");
    }
}

#[test]
fn brier_punishes_confidence_in_the_wrong_answer() {
    let confident_wrong = brier(&[0.95, 0.05], &[0.0, 1.0]);
    let hedged_wrong = brier(&[0.55, 0.45], &[0.0, 1.0]);
    assert!(confident_wrong > hedged_wrong);
}

/// Ten items, three cut dates each: a decent but imperfect ranking, so AUC
/// and Brier both have room to move under resampling.
fn grouped_panel() -> (Vec<f32>, Vec<f32>, Vec<String>) {
    let (mut scores, mut labels, mut groups) = (Vec::new(), Vec::new(), Vec::new());
    for item in 0..10usize {
        for cut in 0..3usize {
            let safe = (item + cut) % 3 != 0;
            let score = if safe { 0.55 } else { 0.35 } + ((item * 7 + cut * 3) % 10) as f32 / 40.0;
            scores.push(score);
            labels.push(if safe { 1.0 } else { 0.0 });
            groups.push(format!("item-{item}"));
        }
    }
    (scores, labels, groups)
}

#[test]
fn the_spread_is_the_same_on_every_run() {
    let (scores, labels, groups) = grouped_panel();
    let groups: Vec<&str> = groups.iter().map(String::as_str).collect();
    assert_eq!(spread(&scores, &labels, &groups), spread(&scores, &labels, &groups));
}

#[test]
fn the_interval_brackets_the_point_estimate() {
    let (scores, labels, groups) = grouped_panel();
    let groups: Vec<&str> = groups.iter().map(String::as_str).collect();
    let result = spread(&scores, &labels, &groups);
    let [auc_low, auc_high] = result.auc.expect("two classes across ten items");
    let [brier_low, brier_high] = result.brier.expect("ten items");
    let (point_auc, point_brier) = (auc(&scores, &labels), brier(&scores, &labels));
    assert!(auc_low <= point_auc && point_auc <= auc_high, "{auc_low} ≤ {point_auc} ≤ {auc_high}");
    assert!(brier_low <= point_brier && point_brier <= brier_high, "{brier_low} ≤ {point_brier} ≤ {brier_high}");
    assert!(auc_low < auc_high, "a ten-item panel is not certain of its AUC");
}

#[test]
fn one_item_gives_no_interval() {
    // Many rows, but all one title: there is nothing to resample.
    let scores = [0.2, 0.8, 0.4, 0.6];
    let labels = [0.0, 1.0, 0.0, 1.0];
    let result = spread(&scores, &labels, &["only"; 4]);
    assert_eq!((result.auc, result.brier), (None, None));
}

#[rstest]
#[case::hedged(&[0.5, 0.8, 0.2], &[0.0, 1.0, 0.0], 0, 0)]
#[case::confident_and_right(&[0.95, 0.05], &[1.0, 0.0], 2, 0)]
#[case::confident_safe_but_played(&[0.9, 0.95], &[0.0, 1.0], 2, 1)]
#[case::confident_played_but_safe(&[0.1, 0.02], &[1.0, 0.0], 2, 1)]
#[case::both_ways_wrong(&[0.99, 0.01, 0.5], &[0.0, 1.0, 1.0], 2, 2)]
fn confident_errors_count_the_wrong_side_of_ninety(
    #[case] scores: &[f32],
    #[case] labels: &[f32],
    #[case] confident: usize,
    #[case] wrong: usize,
) {
    let groups: Vec<String> = (0..scores.len()).map(|row| row.to_string()).collect();
    let groups: Vec<&str> = groups.iter().map(String::as_str).collect();
    let result = spread(scores, labels, &groups);
    assert_eq!((result.confident, result.confident_wrong), (confident, wrong));
}
