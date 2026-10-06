//! The models a household panel can support, and how each is judged.
//!
//! Two candidates, cheapest first. Recalibrating the priors moves two numbers,
//! so a small panel can afford it; the full fit moves all five and needs far
//! more outcomes. Both are judged out of fold: every row is forecast by a model
//! fitted without its item.

use super::panel::Example;
use super::{fold_of, train, FOLDS};
use crate::regret::HazardModel;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    /// The priors' ranking with a fitted scale: `ln λ = a + b · ln λ_prior`.
    #[default]
    Recalibrated,
    /// Every hazard parameter, pulled toward the priors.
    Full,
}

impl ModelKind {
    pub const ALL: [ModelKind; 2] = [ModelKind::Recalibrated, ModelKind::Full];

    pub fn label(self) -> &'static str {
        match self {
            Self::Recalibrated => "recalibrated priors",
            Self::Full => "full fit",
        }
    }
}

/// Fit `kind` on `examples`, starting from the hand-set priors.
pub fn fit(kind: ModelKind, examples: &[Example]) -> HazardModel {
    let priors = HazardModel::default();
    match kind {
        ModelKind::Recalibrated => train::recalibrated(examples, &priors),
        ModelKind::Full => train::full(examples, &priors),
    }
}

/// Every row's P(watch) from `kind` fitted on the other item-grouped folds.
pub fn out_of_fold(kind: ModelKind, dataset: &[Example]) -> Vec<f64> {
    let mut forecasts = vec![0.0; dataset.len()];
    for fold in 0..FOLDS {
        let training: Vec<Example> = dataset.iter().filter(|example| fold_of(&example.item_id) != fold).cloned().collect();
        let model = fit(kind, &training);
        for (index, example) in dataset.iter().enumerate().filter(|(_, example)| fold_of(&example.item_id) == fold) {
            forecasts[index] = model.p_watch(&example.features);
        }
    }
    forecasts
}
