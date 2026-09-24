//! Recalibrated priors: the model a small panel can afford.
//!
//! A full refit moves every weight, and with a handful of played outcomes that
//! mostly re-ranks noise. Recalibration keeps the priors' ranking and fits only
//! how their forecast logit maps to a probability: p = σ(slope · logit +
//! intercept). The slope is pulled toward the deployed mapping (1 / T) as hard
//! as the full fit pulls its weights ([`PRIOR_STRENGTH`] pseudo-examples). The
//! intercept is the base-rate correction and, like the full fit's bias, nearly
//! free: one pseudo-example keeps it finite when the rows hold a single
//! outcome. The slope stays positive, so the order the priors give is kept
//! exactly; only the probabilities become this household's.

use super::panel::Example;
use super::train::PRIOR_STRENGTH;
use super::{forecasts, DEPLOYED_PRIOR_TEMPERATURE};
use crate::score::ScoreWeights;

/// Pull on the intercept, in pseudo-examples: enough to keep it finite, too
/// little to hold it off the household's base rate.
const INTERCEPT_STRENGTH: f64 = 1.0;
/// The transform must stay increasing, or it would reverse the priors' order.
const MIN_SLOPE: f64 = 0.05;
const MAX_NEWTON_STEPS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Calibration {
    pub slope: f32,
    pub intercept: f32,
}

impl Calibration {
    /// How the priors run today: their temperature, no shift.
    pub fn deployed() -> Self {
        Self { slope: 1.0 / DEPLOYED_PRIOR_TEMPERATURE, intercept: 0.0 }
    }

    /// The same mapping as a scorecard's bias and temperature, the form the
    /// daemon runs: σ((logit + bias) / T) = σ(slope · logit + intercept).
    pub fn bias_and_temperature(self) -> (f32, f32) {
        let temperature = 1.0 / self.slope;
        (self.intercept * temperature, temperature)
    }
}

/// The priors' forecast logit at temperature 1, per example: what a
/// calibration maps to a probability.
pub fn prior_logits(examples: &[Example]) -> Vec<f32> {
    forecasts(examples, &ScoreWeights::default(), 1.0)
        .into_iter()
        .map(|p| {
            let p = f64::from(p).clamp(1e-9, 1.0 - 1e-9);
            (p / (1.0 - p)).ln() as f32
        })
        .collect()
}

/// MAP fit of `labels` on `logits`. The objective is strictly convex (both
/// parameters carry a Gaussian pull), so damped Newton converges from the
/// deployed mapping; with no rows it stays there.
pub fn fit(logits: &[f32], labels: &[f32]) -> Calibration {
    let start = Calibration::deployed();
    let centre = (f64::from(start.slope), f64::from(start.intercept));
    let strength = f64::from(PRIOR_STRENGTH);
    let rows: Vec<(f64, f64)> = logits.iter().zip(labels).map(|(x, y)| (f64::from(*x), f64::from(*y))).collect();
    let objective = |a: f64, b: f64| -> f64 {
        let data: f64 = rows.iter().map(|(x, y)| softplus(a * x + b) - y * (a * x + b)).sum();
        data + 0.5 * strength * (a - centre.0).powi(2) + 0.5 * INTERCEPT_STRENGTH * (b - centre.1).powi(2)
    };

    let (mut a, mut b) = centre;
    for _ in 0..MAX_NEWTON_STEPS {
        let (mut grad_a, mut grad_b) = (strength * (a - centre.0), INTERCEPT_STRENGTH * (b - centre.1));
        let (mut h_aa, mut h_ab, mut h_bb) = (strength, 0.0, INTERCEPT_STRENGTH);
        for (x, y) in &rows {
            let p = sigmoid(a * x + b);
            grad_a += (p - y) * x;
            grad_b += p - y;
            let w = p * (1.0 - p);
            h_aa += w * x * x;
            h_ab += w * x;
            h_bb += w;
        }
        let det = h_aa * h_bb - h_ab * h_ab;
        let (step_a, step_b) = ((h_bb * grad_a - h_ab * grad_b) / det, (h_aa * grad_b - h_ab * grad_a) / det);
        // Damped: shorten the step until the objective does not rise.
        let current = objective(a, b);
        let mut scale = 1.0;
        while scale > 1e-6 && objective(a - scale * step_a, b - scale * step_b) > current {
            scale *= 0.5;
        }
        a -= scale * step_a;
        b -= scale * step_b;
        if (scale * step_a).abs() + (scale * step_b).abs() < 1e-10 {
            break;
        }
    }
    Calibration { slope: a.max(MIN_SLOPE) as f32, intercept: b as f32 }
}

fn sigmoid(z: f64) -> f64 {
    1.0 / (1.0 + (-z).exp())
}

/// log(1 + e^z), without overflow for large z.
fn softplus(z: f64) -> f64 {
    if z > 30.0 {
        z
    } else {
        z.exp().ln_1p()
    }
}

#[cfg(test)]
mod tests;
