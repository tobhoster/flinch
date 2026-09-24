//! The adoption gate and the candidate models it judges: what each needs,
//! and what recalibrating the priors must preserve.

use super::*;

#[test]
fn fitted_weights_never_touch_the_frozen_policy_signals() {
    let model = FittedModel {
        fitted_on: "test".to_string(),
        kind: candidate::ModelKind::Full,
        weights: [
            ("never_played".to_string(), 1.1f32),
            ("rewatched".to_string(), -0.7),
            ("no_evidence".to_string(), 9.0),
            ("protected_by_tag".to_string(), 9.0),
        ]
        .into_iter()
        .collect(),
        bias: 0.2,
        temperature: 1.3,
        metrics: Metrics::default(),
    };
    let weights = model.weights();
    assert!((weights.never_played - 1.1).abs() < 1e-6);
    assert!((weights.rewatched + 0.7).abs() < 1e-6, "a household signal read back from weights.json");
    assert!((weights.no_evidence - score::ScoreWeights::default().no_evidence).abs() < 1e-6, "policy stays");
    assert!((weights.protected_by_tag - score::ScoreWeights::default().protected_by_tag).abs() < 1e-6);
    assert!((weights.bias - 0.2).abs() < 1e-6);
}

#[test]
fn adoption_requires_a_real_improvement_out_of_fold() {
    let mut model = FittedModel {
        fitted_on: "test".to_string(),
        kind: candidate::ModelKind::Full,
        weights: HashMap::new(),
        bias: 0.0,
        temperature: 1.0,
        metrics: Metrics {
            examples: MIN_EXAMPLES,
            positives: 40,
            negative_items: 12,
            auc: 0.8,
            brier: 0.10,
            priors_brier: 0.20,
            priors_auc: 0.7,
            ..Default::default()
        },
    };
    assert!(model.beats_priors());

    // A one-class panel: perfect Brier, no discrimination. Refuse it.
    model.metrics.positives = model.metrics.examples;
    assert!(!model.beats_priors(), "a single-class panel must never be adopted");

    // Ranking at chance is not a model, however good its Brier.
    model.metrics.positives = 40;
    model.metrics.auc = 0.52;
    assert!(!model.beats_priors(), "chance-level ranking must be refused");
    model.metrics.auc = 0.8;

    // A tie is not an improvement.
    model.metrics.brier = model.metrics.priors_brier;
    assert!(!model.beats_priors());

    // Better Brier but worse ranking is a trade the guard should refuse.
    model.metrics.brier = 0.15;
    model.metrics.auc = 0.60;
    assert!(!model.beats_priors());

    // Too little data: priors win by default.
    model.metrics.auc = 0.9;
    model.metrics.examples = 20;
    assert!(!model.beats_priors());
}

/// A panel of `examples` rows with `played` played outcomes from
/// `played_titles` titles, where the model clearly beats the priors.
#[rstest]
#[case::recalibration_on_a_small_panel(candidate::ModelKind::Recalibrated, 79, 2, 2, true)]
#[case::the_full_fit_cannot_use_that_panel(candidate::ModelKind::Full, 79, 2, 2, false)]
#[case::recalibration_needs_forty_rows(candidate::ModelKind::Recalibrated, 39, 2, 2, false)]
#[case::one_played_title_teaches_nothing(candidate::ModelKind::Recalibrated, 79, 3, 1, false)]
#[case::the_full_fit_with_enough_outcomes(candidate::ModelKind::Full, 200, 12, 5, true)]
fn each_candidate_needs_the_data_its_size_demands(
    #[case] kind: candidate::ModelKind,
    #[case] examples: usize,
    #[case] played: usize,
    #[case] played_titles: usize,
    #[case] adopted: bool,
) {
    let metrics = Metrics {
        examples,
        positives: examples - played,
        negative_items: played_titles,
        auc: 0.9,
        brier: 0.02,
        priors_brier: 0.2,
        priors_auc: 0.85,
        ..Default::default()
    };
    assert_eq!(shortfall(kind, &metrics).is_none(), adopted, "{:?}", shortfall(kind, &metrics));
}

#[test]
fn recalibration_keeps_the_priors_order_and_calibrates_them_to_the_household() {
    // Forty films nobody ever plays, and two the household plays after a cut.
    let mut items: Vec<FitItem> =
        (0..40).map(|index| item(&format!("radarr-{index}"), LibraryKind::Movie, 400.0 + index as f32 * 5.0, vec![])).collect();
    items.push(item("radarr-90", LibraryKind::Movie, 400.0, vec![NOW - 70 * DAY]));
    items.push(item("radarr-91", LibraryKind::Movie, 400.0, vec![NOW - 100 * DAY]));
    let dataset = panel(&items, &[60.0, 90.0, 120.0], 30.0);
    assert!(dataset.iter().any(|row| row.label < 0.5), "the panel must hold played outcomes");
    let labels: Vec<f32> = dataset.iter().map(|row| row.label).collect();

    let priors = forecasts(&dataset, &score::ScoreWeights::default(), DEPLOYED_PRIOR_TEMPERATURE);
    let trained = candidate::fit(candidate::ModelKind::Recalibrated, &dataset);
    let recalibrated = forecasts(&dataset, &trained.weights, trained.temperature);

    for (i, j) in (0..dataset.len()).flat_map(|i| (0..dataset.len()).map(move |j| (i, j))) {
        if priors[i] < priors[j] {
            assert!(recalibrated[i] <= recalibrated[j], "rows {i} and {j} changed order");
        }
    }
    let brier = |p: &[f32]| p.iter().zip(&labels).map(|(p, y)| (p - y).powi(2)).sum::<f32>() / labels.len() as f32;
    assert!(brier(&recalibrated) < brier(&priors), "{} vs the priors' {}", brier(&recalibrated), brier(&priors));
}

