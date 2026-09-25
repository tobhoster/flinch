//! The models a household panel can support, and how each is judged.
//!
//! Two candidates, cheapest first. Recalibrating the priors moves two numbers,
//! so a small panel can afford it; the full fit moves every trainable weight
//! and needs far more outcomes. Both are judged out of fold: every row is
//! forecast by a model fitted without its item. The adoption gate and the
//! head-to-head read the same numbers, so the model the status page compares
//! is the one that runs.

use super::eval::{self, Difference, Scorecard, Spread};
use super::panel::Example;
use super::{calibrate, fold_of, train, FOLDS};
use crate::score::{self, ScoreWeights};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    /// The priors' weights, with this household's base rate and confidence.
    Recalibrated,
    /// Every trainable weight fitted to this household. The default, because
    /// a `weights.json` written before recalibration existed holds one.
    #[default]
    Full,
}

impl ModelKind {
    pub const ALL: [ModelKind; 2] = [ModelKind::Recalibrated, ModelKind::Full];

    pub fn label(self) -> &'static str {
        match self {
            ModelKind::Recalibrated => "recalibrated priors",
            ModelKind::Full => "full fit",
        }
    }
}

/// A candidate fitted and ready to score: what the daemon would run.
#[derive(Debug, Clone)]
pub struct Trained {
    pub weights: ScoreWeights,
    pub temperature: f32,
}

/// Fit `kind` on `examples`, temperature included.
pub fn fit(kind: ModelKind, examples: &[Example]) -> Trained {
    match kind {
        ModelKind::Recalibrated => {
            let labels: Vec<f32> = examples.iter().map(|example| example.label).collect();
            let (bias, temperature) = calibrate::fit(&calibrate::prior_logits(examples), &labels).bias_and_temperature();
            let mut weights = ScoreWeights::default();
            weights.bias = bias;
            Trained { weights, temperature }
        }
        ModelKind::Full => {
            let fit = train::fit_scorecard(examples);
            // Fitted on the weights exactly as the daemon applies them, bias
            // included — not on some intermediate scaled quantity.
            let logits: Vec<(f32, f32)> = examples.iter().map(|example| (fit.logit(example), example.label)).collect();
            Trained { weights: fit.weights(), temperature: train::fit_temperature(&logits) }
        }
    }
}

/// One row judged out of fold: the forecast every accuracy metric scores, and
/// the gated P(safe) the floor acts on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Judged {
    pub forecast: f32,
    pub p_safe: f32,
}

/// Every row forecast by `kind` fitted on the other item-grouped folds: the
/// position a zero-shot external model is in, with no home advantage.
pub fn out_of_fold(kind: ModelKind, dataset: &[Example]) -> Vec<Judged> {
    let folds: Vec<usize> = dataset.iter().map(|example| fold_of(&example.item_id)).collect();
    let mut out = vec![Judged { forecast: 0.0, p_safe: 0.0 }; dataset.len()];
    for fold in 0..FOLDS {
        if !folds.contains(&fold) {
            continue;
        }
        let training: Vec<Example> =
            dataset.iter().zip(&folds).filter(|(_, other)| **other != fold).map(|(example, _)| example.clone()).collect();
        let trained = fit(kind, &training);
        for (row, example) in dataset.iter().enumerate().filter(|(row, _)| folds[*row] == fold) {
            let scored = score::score(&example.card, example.ctx, &trained.weights, trained.temperature);
            out[row] = Judged { forecast: scored.forecast, p_safe: scored.p_safe };
        }
    }
    out
}

/// A candidate's scores from its out-of-fold forecasts. A recalibration is one
/// increasing map of the priors, so the model it adopts ranks every row
/// exactly as they do: its AUC, and the AUC's interval, are theirs. Pooling
/// four per-fold calibrations would mix four scales and misstate that ranking.
/// The probability metrics (Brier, log-loss, ECE) are what the folds judge.
pub fn scorecard(kind: ModelKind, forecasts: &[f32], labels: &[f32], priors: &Scorecard) -> Scorecard {
    let card = Scorecard::of(forecasts, labels);
    match kind {
        ModelKind::Recalibrated => Scorecard { auc: priors.auc, ..card },
        ModelKind::Full => card,
    }
}

/// [`scorecard`]'s rule for the bootstrap intervals.
pub fn spread(kind: ModelKind, own: Spread, priors: &Spread) -> Spread {
    match kind {
        ModelKind::Recalibrated => Spread { auc: priors.auc, ..own },
        ModelKind::Full => own,
    }
}

/// [`scorecard`]'s rule for a paired comparison with another model `other`
/// on the same rows (see [`eval::paired_difference`]).
pub fn difference(kind: ModelKind, forecasts: &[f32], priors: &[f32], other: &[f32], labels: &[f32], groups: &[&str]) -> Difference {
    let own = eval::paired_difference(forecasts, other, labels, groups);
    match kind {
        ModelKind::Recalibrated => Difference { auc: eval::paired_difference(priors, other, labels, groups).auc, ..own },
        ModelKind::Full => own,
    }
}
