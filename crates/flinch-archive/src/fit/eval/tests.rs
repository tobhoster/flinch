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
fn threshold_stats_report_what_the_floor_actually_flags() {
    let scores = [0.9, 0.8, 0.3, 0.2];
    let labels = [1.0, 0.0, 1.0, 0.0];
    let stats = at_threshold(&scores, &labels, 0.75);
    assert_eq!(stats.flagged, 2);
    assert!((stats.precision - 0.5).abs() < 1e-6, "one of two flagged was safe");
    assert!((stats.recall - 0.5).abs() < 1e-6, "one of two safe items found");
}

#[test]
fn brier_punishes_confidence_in_the_wrong_answer() {
    let confident_wrong = brier(&[0.95, 0.05], &[0.0, 1.0]);
    let hedged_wrong = brier(&[0.55, 0.45], &[0.0, 1.0]);
    assert!(confident_wrong > hedged_wrong);
}

#[test]
fn the_zero_error_bound_matches_its_closed_form() {
    // With no errors in n trials the bound is exactly 1 − δ^(1/n).
    let bound = binomial_upper_bound(0, 125, 0.1);
    let exact = 1.0 - 0.1f64.powf(1.0 / 125.0);
    assert!((bound - exact).abs() < 1e-6, "{bound} vs {exact}");
}

#[test]
fn more_errors_raise_the_bound_and_more_data_lowers_it() {
    assert!(binomial_upper_bound(2, 125, 0.1) > binomial_upper_bound(1, 125, 0.1));
    assert!(binomial_upper_bound(1, 125, 0.1) > binomial_upper_bound(0, 125, 0.1));
    assert!(binomial_upper_bound(1, 500, 0.1) < binomial_upper_bound(1, 125, 0.1));
}

#[test]
fn a_small_panel_cannot_certify_a_tight_risk_level() {
    // 50 rows, not one error: still only α ≥ 1 − 0.1^(1/50) ≈ 4.5% can be
    // vouched for. Asking for 1% must be refused, not rounded.
    let scores = vec![0.9f32; 50];
    let labels = vec![1.0f32; 50];
    assert!(certified_floor(&scores, &labels, 0.01, 0.1).is_none());
    assert!(certified_floor(&scores, &labels, 0.05, 0.1).is_some());
}

#[test]
fn a_stricter_risk_level_never_lowers_the_floor() {
    // Played items (label 0) only score below 0.70; 3 of them at exactly 0.69.
    let mut scores = Vec::new();
    let mut labels = Vec::new();
    for index in 0..1000usize {
        let score = 0.5 + (index % 50) as f32 / 100.0;
        scores.push(score);
        labels.push(if score < 0.695 && index % 7 == 0 { 0.0 } else { 1.0 });
    }
    let strict = certified_floor(&scores, &labels, 0.005, 0.1).expect("certifiable");
    let loose = certified_floor(&scores, &labels, 0.05, 0.1).expect("certifiable");
    assert!((strict.floor - 0.70).abs() < 1e-6, "strict stops above the first errors, got {}", strict.floor);
    assert_eq!(strict.false_reclaims, 0);
    assert!(loose.floor <= strict.floor, "looser α may go lower, never higher");
    assert!(loose.upper_bound <= 0.05);
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

/// Twenty titles at three cut dates each; every fifth title was played.
fn paired_panel() -> (Vec<f32>, Vec<String>) {
    let labels = (0..60).map(|row| if (row / 3) % 5 == 0 { 0.0 } else { 1.0 }).collect();
    let groups = (0..60).map(|row| format!("title-{}", row / 3)).collect();
    (labels, groups)
}

#[test]
fn a_model_against_itself_differs_by_exactly_nothing() {
    let (labels, groups) = paired_panel();
    let groups: Vec<&str> = groups.iter().map(String::as_str).collect();
    let scores: Vec<f32> = labels.iter().enumerate().map(|(row, label)| 0.2 + 0.6 * label - 0.01 * (row % 3) as f32).collect();
    let difference = paired_difference(&scores, &scores, &labels, &groups);
    for interval in [difference.brier, difference.log_loss, difference.auc] {
        assert_eq!(interval, Some([0.0, 0.0]));
    }
}

#[test]
fn a_calibrated_model_beats_a_miscalibrated_one_on_every_resample() {
    let (labels, groups) = paired_panel();
    let groups: Vec<&str> = groups.iter().map(String::as_str).collect();
    // Same ranking, but one says 0.8 where the truth is 0.8, the other 0.3.
    let calibrated: Vec<f32> = labels.iter().map(|label| if *label >= 0.5 { 0.85 } else { 0.6 }).collect();
    let timid: Vec<f32> = labels.iter().map(|label| if *label >= 0.5 { 0.35 } else { 0.1 }).collect();
    let difference = paired_difference(&calibrated, &timid, &labels, &groups);
    let [low, high] = difference.brier.expect("an interval over twenty titles");
    assert!(high < 0.0, "Brier [{low}, {high}] must favour the calibrated model");
    assert!(difference.log_loss.is_some_and(|[_, high]| high < 0.0));
    assert_eq!(difference.auc, Some([0.0, 0.0]), "same ranking, same AUC");
}

#[test]
fn one_title_gives_no_paired_interval() {
    let difference = paired_difference(&[0.9, 0.8], &[0.1, 0.2], &[1.0, 0.0], &["only", "only"]);
    assert_eq!(difference, Difference::default());
}
