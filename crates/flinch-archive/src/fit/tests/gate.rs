//! The fit and its adoption gate.

use super::super::adopt::fit_model;
use super::super::candidate::{self, ModelKind};
use super::super::load::Household;
use super::*;
use crate::regret::HazardModel;

fn household(items: Vec<FitItem>) -> Household {
    Household {
        items,
        unreadable_rows: 0,
        plex_rows: 0,
        tautulli_rows: 0,
        vectors: crate::embedding::VectorStore::from_vectors(Vec::new()),
    }
}

/// A household that rewatches every title it played within the last month of
/// each cut, and never touches anything older: recency decides everything.
fn recency_driven(titles: usize) -> Vec<FitItem> {
    (0..titles)
        .map(|n| {
            // Every 45 days; odd titles stop playing two years before now.
            let plays: Vec<u64> =
                (0..16u64).map(|k| NOW - (40 + 45 * k) * DAY).filter(|epoch| n % 2 == 0 || *epoch < NOW - 700 * DAY).collect();
            item(&format!("radarr-{n}"), LibraryKind::Movie, 900.0, plays)
        })
        .collect()
}

#[test]
fn a_near_empty_watch_log_keeps_the_priors() {
    let items =
        vec![item("radarr-1", LibraryKind::Movie, 900.0, vec![]), item("radarr-2", LibraryKind::Movie, 900.0, vec![NOW - 500 * DAY])];
    let dataset = panel(&items, &default_cuts(), HORIZON_DAYS);
    let model = fit_model(&household(items), &dataset, NOW, default_cuts().len());
    assert!(model.shortfall().is_some(), "two titles cannot outvote the priors");
    let empty = fit_model(&household(Vec::new()), &[], NOW, 0);
    assert!(empty.shortfall().is_some(), "no rows, no fit");
    assert!(empty.hazard.lambda0_per_day.is_finite());
}

#[test]
fn a_clear_household_signal_is_learnt_and_adopted() {
    let items = recency_driven(60);
    let dataset = panel(&items, &default_cuts(), HORIZON_DAYS);
    let model = fit_model(&household(items), &dataset, NOW, default_cuts().len());
    assert_eq!(model.shortfall(), None, "{:?}", model.metrics);
    assert!(model.metrics.auc > model.metrics.priors_auc - 0.02 && model.metrics.brier < model.metrics.priors_brier);
}

#[test]
fn recalibration_keeps_the_priors_ranking() {
    let dataset = panel(&recency_driven(30), &default_cuts(), HORIZON_DAYS);
    let fitted = candidate::fit(ModelKind::Recalibrated, &dataset);
    let priors = HazardModel::default();
    let order = |model: &HazardModel| {
        let mut rows: Vec<usize> = (0..dataset.len()).collect();
        rows.sort_by(|a, b| model.log_hazard(&dataset[*a].features).total_cmp(&model.log_hazard(&dataset[*b].features)));
        rows
    };
    assert_eq!(order(&fitted), order(&priors), "one increasing map of the priors' linear predictor");
}

#[rstest]
#[case::recalibration_with_too_few_rows(ModelKind::Recalibrated, 39, 10, 2, true)]
#[case::recalibration_enough(ModelKind::Recalibrated, 40, 10, 2, false)]
#[case::full_needs_more_rows(ModelKind::Full, 100, 30, 5, true)]
#[case::one_played_title_is_not_a_signal(ModelKind::Recalibrated, 200, 30, 1, true)]
fn each_candidate_needs_the_data_its_size_demands(
    #[case] kind: ModelKind,
    #[case] examples: usize,
    #[case] played: usize,
    #[case] played_items: usize,
    #[case] short: bool,
) {
    let metrics =
        Metrics { examples, played, played_items, auc: 0.8, brier: 0.1, priors_auc: 0.75, priors_brier: 0.2, ..Metrics::default() };
    assert_eq!(shortfall(kind, &metrics).is_some(), short);
}

#[test]
fn a_fit_that_does_not_beat_the_priors_brier_is_refused() {
    let metrics = Metrics {
        examples: 500,
        played: 100,
        played_items: 20,
        auc: 0.8,
        brier: 0.198,
        priors_auc: 0.8,
        priors_brier: 0.2,
        ..Metrics::default()
    };
    assert!(shortfall(ModelKind::Full, &metrics).is_some_and(|reason| reason.contains("Brier")));
}
