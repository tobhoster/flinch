//! Fitting itself: logistic regression and the temperature search.
//!
//! Kept apart from dataset construction so the two concerns stay readable: what
//! counts as an example lives in `fit`, how a model is learnt from examples lives
//! here. Both are small on purpose — eight parameters and a few thousand rows do
//! not need a solver library, and a hand-rolled gradient descent is inspectable.

use super::panel::Example;
use super::DEPLOYED_PRIOR_TEMPERATURE;
use crate::score::ScoreWeights;
use std::collections::HashMap;

/// Pseudo-examples of pull toward the deployed priors: a panel this small
/// cannot outvote them, a few hundred real outcomes will.
pub const PRIOR_STRENGTH: f32 = 25.0;

/// A trained scorecard in raw (interpretable) units.
pub struct Fit {
    pub weights: HashMap<&'static str, f32>,
    pub bias: f32,
    /// Training Brier, for the report.
    pub train_brier: f32,
}

impl Fit {
    /// Raw logit of one example under this fit.
    pub fn logit(&self, example: &Example) -> f32 {
        self.bias + example.values.iter().map(|(name, value)| self.weights.get(name).copied().unwrap_or(0.0) * value).sum::<f32>()
    }

    /// The weight table the daemon would run, policy signals at their priors.
    pub fn weights(&self) -> ScoreWeights {
        let mut weights = ScoreWeights::default();
        for (name, value) in &self.weights {
            weights.set(name, *value);
        }
        weights.bias = self.bias;
        weights
    }
}

/// Every weight training may move: all but the frozen policy signals.
pub fn trainable() -> Vec<&'static str> {
    ScoreWeights::names().iter().copied().filter(|name| !ScoreWeights::frozen().contains(name)).collect()
}

/// The fit `flinch-fit` performs: every trainable weight, shrunk toward the
/// model that actually runs — the priors at their deployed temperature — not
/// toward zero. Signals with a zero prior start at zero and move only as far as
/// this household's outcomes push them.
pub fn fit_scorecard(training: &[Example]) -> Fit {
    let names = trainable();
    let deployed = ScoreWeights::default();
    let prior: HashMap<&'static str, f32> = names.iter().map(|name| (*name, deployed.get(name) / DEPLOYED_PRIOR_TEMPERATURE)).collect();
    train(training, &names, PRIOR_STRENGTH, &prior)
}

/// Logistic regression by full-batch gradient descent, shrunk toward a prior.
///
/// The regulariser pulls each weight toward `prior` (raw units), not toward
/// zero. With the handful of outcomes a household produces, zero-centred L2
/// collapses the model to the base rate and throws away the domain knowledge
/// the priors encode; an informative prior keeps that knowledge until the data
/// is strong enough to overrule it. `prior_strength` is measured in
/// pseudo-examples and divided by `n`, so its pull fades as outcomes accumulate —
/// the MAP estimate of a Bayesian logistic regression with a Gaussian prior
/// centred on the priors. Names missing from `prior` are centred on zero.
///
/// Features are standardised internally for conditioning and the result is
/// converted back to raw space, so fitted weights are readable in the same units
/// as the priors (`never_played` is still "logit per unit of evidence").
pub fn train(examples: &[Example], names: &[&'static str], prior_strength: f32, prior: &HashMap<&'static str, f32>) -> Fit {
    let n = examples.len().max(1) as f32;
    let mut mean = vec![0.0f32; names.len()];
    let mut std = vec![1.0f32; names.len()];
    for (index, name) in names.iter().enumerate() {
        let column: Vec<f32> = examples.iter().map(|example| example.values.get(name).copied().unwrap_or(0.0)).collect();
        let column_mean = column.iter().sum::<f32>() / n;
        let variance = column.iter().map(|value| (value - column_mean).powi(2)).sum::<f32>() / n;
        mean[index] = column_mean;
        std[index] = variance.sqrt().max(1e-3);
    }

    let rows: Vec<(Vec<f32>, f32)> = examples
        .iter()
        .map(|example| {
            let x = names
                .iter()
                .enumerate()
                .map(|(index, name)| (example.values.get(name).copied().unwrap_or(0.0) - mean[index]) / std[index])
                .collect();
            (x, example.label)
        })
        .collect();

    // The prior, moved into standardised units: raw = standardised / std.
    let centre: Vec<f32> = names.iter().enumerate().map(|(index, name)| prior.get(name).copied().unwrap_or(0.0) * std[index]).collect();
    let pull = prior_strength / n;
    let mut w = centre.clone();
    let mut b = 0.0f32;
    let lr = 0.35f32;
    for _ in 0..4000 {
        let mut grad = vec![0.0f32; names.len()];
        let mut grad_b = 0.0f32;
        for (x, label) in &rows {
            let logit: f32 = b + x.iter().zip(&w).map(|(value, weight)| value * weight).sum::<f32>();
            let error = 1.0 / (1.0 + (-logit).exp()) - label;
            for (index, value) in x.iter().enumerate() {
                grad[index] += error * value;
            }
            grad_b += error;
        }
        for index in 0..w.len() {
            w[index] -= lr * (grad[index] / n + pull * (w[index] - centre[index]));
        }
        b -= lr * grad_b / n;
    }

    let train_brier = rows
        .iter()
        .map(|(x, label)| {
            let logit: f32 = b + x.iter().zip(&w).map(|(value, weight)| value * weight).sum::<f32>();
            let p = 1.0 / (1.0 + (-logit).exp());
            (p - label).powi(2)
        })
        .sum::<f32>()
        / n;

    // Back to raw units: w_scaled / std, with the bias absorbing the centring.
    let mut raw = HashMap::new();
    let mut bias = b;
    for (index, name) in names.iter().enumerate() {
        let value = w[index] / std[index];
        raw.insert(*name, value);
        bias -= value * mean[index];
    }
    Fit { weights: raw, bias, train_brier }
}

/// Temperature that minimises Brier on held-out logits.
///
/// This is the single highest-value calibration step available with data this
/// small: it cannot reorder candidates, only stop the model from claiming more
/// confidence than its track record supports.
pub fn fit_temperature(validation: &[(f32, f32)]) -> f32 {
    if validation.is_empty() {
        return 1.0;
    }
    let mut best = (f32::MAX, 1.0f32);
    let mut temperature = 0.4f32;
    while temperature <= 4.01 {
        let brier = validation
            .iter()
            .map(|(logit, label)| {
                let p = 1.0 / (1.0 + (-(logit / temperature)).exp());
                (p - label).powi(2)
            })
            .sum::<f32>()
            / validation.len() as f32;
        // Ties go to the temperature nearest 1.0: a rescale that buys nothing is
        // a distortion, and two fits of the same data must not disagree on it.
        let better = brier < best.0 - 1e-6;
        let tied_but_closer = (brier - best.0).abs() <= 1e-6 && (temperature - 1.0).abs() < (best.1 - 1.0).abs();
        if better || tied_but_closer {
            best = (brier, temperature);
        }
        temperature += 0.02;
    }
    best.1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fit::eval;

    /// A training row over hand-set feature values. Only `values` and `label`
    /// reach `train`; the golden card keeps the row a complete, scorable example.
    fn example(item_id: String, values: HashMap<&'static str, f32>, label: f32) -> Example {
        Example {
            item_id,
            cut_days: 0.0,
            cut_unix: 0,
            card: crate::golden::golden_movie(),
            ctx: crate::score::HouseholdContext::default(),
            values,
            label,
        }
    }

    fn synthetic() -> Vec<Example> {
        let mut examples = Vec::new();
        for index in 0..200 {
            let dwell = (index % 10) as f32 / 3.0;
            let mut values = HashMap::new();
            values.insert("dwell", dwell);
            values.insert("never_played", if index % 3 == 0 { 1.0 } else { 0.0 });
            let label = if dwell > 2.0 { 1.0 } else { 0.0 };
            examples.push(example(format!("item-{index}"), values, label));
        }
        examples
    }

    #[test]
    fn training_recovers_a_signal_the_data_actually_contains() {
        let fit = train(&synthetic(), &["dwell", "never_played"], 2.0, &HashMap::new());
        let dwell = fit.weights.get("dwell").copied().unwrap_or(0.0);
        // `never_played` is noise here; dwell is the truth.
        let noise = fit.weights.get("never_played").copied().unwrap_or(0.0).abs();
        assert!(dwell > 0.5, "dwell must be learnt positive, got {dwell}");
        assert!(dwell > noise, "the real signal must outweigh the decoy: {dwell} vs {noise}");
        assert!(fit.train_brier < 0.2, "a learnable pattern must fit: {}", fit.train_brier);
    }

    #[test]
    fn fitted_weights_are_in_raw_units_not_standardised_ones() {
        // A feature with a large spread must not end up with a tiny weight purely
        // because it was standardised for conditioning.
        let mut examples = Vec::new();
        for index in 0..200 {
            let dwell = if index % 2 == 0 { 0.0 } else { 4.0 };
            let mut values = HashMap::new();
            values.insert("dwell", dwell);
            examples.push(example(format!("item-{index}"), values, if dwell > 0.0 { 1.0 } else { 0.0 }));
        }
        let fit = train(&examples, &["dwell"], 0.0, &HashMap::new());
        let dwell = fit.weights.get("dwell").copied().unwrap_or(0.0);
        // 4 units of dwell must be worth several logits, not "about 1".
        assert!(dwell > 0.5, "raw-unit weight should stay interpretable, got {dwell}");
    }

    #[test]
    fn an_uninformative_panel_keeps_the_prior_that_zero_centring_throws_away() {
        // Labels alternate regardless of dwell: the data says nothing about it.
        // A zero-centred fit ends at zero; a prior-centred one keeps most of what
        // the prior knew, and uninformative data may only pull it toward zero.
        let mut examples = Vec::new();
        for index in 0..60 {
            let mut values = HashMap::new();
            values.insert("dwell", (index % 5) as f32);
            examples.push(example(format!("i{index}"), values, (index % 2) as f32));
        }
        let prior: HashMap<&'static str, f32> = [("dwell", 0.8f32)].into_iter().collect();
        let kept = train(&examples, &["dwell"], 25.0, &prior).weights["dwell"];
        let erased = train(&examples, &["dwell"], 25.0, &HashMap::new()).weights["dwell"];
        assert!(erased.abs() < 0.1, "no signal and no prior: nothing to learn, got {erased}");
        assert!(kept > 0.3, "the prior must survive an uninformative panel, got {kept}");
        assert!(kept <= 0.8 + 1e-3, "uninformative data may only pull toward zero, got {kept}");
    }

    #[test]
    fn strong_evidence_overrules_a_wrong_prior() {
        // The prior says dwell argues *against* reclaiming; 200 clean examples say
        // the opposite. Data this strong must win.
        let prior: HashMap<&'static str, f32> = [("dwell", -2.0f32)].into_iter().collect();
        let dwell = train(&synthetic(), &["dwell"], 25.0, &prior).weights["dwell"];
        assert!(dwell > 0.0, "clear evidence must overturn a wrong prior, got {dwell}");
    }

    #[test]
    fn temperature_search_softens_an_overconfident_model() {
        // Logits near certainty, truth is 50/50.
        let validation: Vec<(f32, f32)> = vec![(4.0, 1.0), (4.0, 0.0), (4.0, 1.0), (4.0, 0.0)];
        let temperature = fit_temperature(&validation);
        assert!(temperature > 2.0, "an overconfident model must be softened, got {temperature}");
        let calibrated = eval::brier(
            &validation.iter().map(|(l, _)| 1.0 / (1.0 + (-(l / temperature)).exp())).collect::<Vec<_>>(),
            &validation.iter().map(|(_, y)| *y).collect::<Vec<_>>(),
        );
        let raw = eval::brier(
            &validation.iter().map(|(l, _)| 1.0 / (1.0 + (-l).exp())).collect::<Vec<_>>(),
            &validation.iter().map(|(_, y)| *y).collect::<Vec<_>>(),
        );
        assert!(calibrated < raw, "temperature must improve held-out Brier");
    }

    #[test]
    fn temperature_is_left_alone_when_the_model_is_already_honest() {
        // Logits of 0 -> p 0.5, and the labels are half positive.
        let validation: Vec<(f32, f32)> = vec![(0.0, 1.0), (0.0, 1.0), (0.0, 0.0), (0.0, 0.0)];
        assert!((fit_temperature(&validation) - 1.0).abs() < 0.05);
    }
}
