//! Fitting the hazard: penalised maximum likelihood on the panel's binary
//! outcome "played within the horizon after the cut".
//!
//! Under the exponential hazard `P = 1 − exp(−λ·H)` with `ln λ = θ · design`,
//! that outcome is a complementary log-log model,
//! `ln(−ln(1 − P)) = θ · design + ln H`. Fisher scoring fits it in a handful
//! of steps, and five parameters need no solver library. The penalty pulls θ toward the hand-set priors, not
//! toward zero: a household's few outcomes cannot outvote domain sense, a few
//! hundred can.

use super::panel::Example;
use crate::regret::{HazardModel, HORIZON_DAYS, PARAMS};

/// Pseudo-observations of pull toward the prior parameters.
pub const PRIOR_STRENGTH: f64 = 25.0;
const MAX_STEPS: usize = 60;
const TOLERANCE: f64 = 1e-9;

/// The full fit: every parameter, pulled toward `base`.
pub fn full(examples: &[Example], base: &HazardModel) -> HazardModel {
    let rows: Vec<Vec<f64>> = examples.iter().map(|example| example.features.design().to_vec()).collect();
    let prior = base.params().to_vec();
    let fitted = scoring(&rows, &labels(examples), &prior, prior.clone());
    let mut params = [0.0; PARAMS];
    params.copy_from_slice(&fitted);
    HazardModel::from_params(params)
}

/// Recalibration: keep `base`'s ranking and fit only `ln λ = a + b · ln λ_base`,
/// pulled toward `a = 0, b = 1`. Two numbers, so a small panel can afford it.
pub fn recalibrated(examples: &[Example], base: &HazardModel) -> HazardModel {
    let rows: Vec<Vec<f64>> = examples.iter().map(|example| vec![1.0, base.log_hazard(&example.features)]).collect();
    let fitted = scoring(&rows, &labels(examples), &[0.0, 1.0], vec![0.0, 1.0]);
    let (a, b) = (fitted[0], fitted[1]);
    let mut params = base.params().map(|param| b * param);
    params[0] += a;
    HazardModel::from_params(params)
}

fn labels(examples: &[Example]) -> Vec<bool> {
    examples.iter().map(|example| example.label >= 0.5).collect()
}

/// Penalised log-likelihood of the cloglog model at `theta`.
fn objective(rows: &[Vec<f64>], played: &[bool], prior: &[f64], theta: &[f64]) -> f64 {
    let fit: f64 = rows.iter().zip(played).map(|(row, played)| row_terms(row, *played, theta).0).sum();
    fit - 0.5 * PRIOR_STRENGTH * theta.iter().zip(prior).map(|(t, p)| (t - p).powi(2)).sum::<f64>()
}

/// One row's log-likelihood, its derivative in the linear predictor, and its
/// Fisher weight.
fn row_terms(row: &[f64], played: bool, theta: &[f64]) -> (f64, f64, f64) {
    let eta = (row.iter().zip(theta).map(|(x, t)| x * t).sum::<f64>() + HORIZON_DAYS.ln()).clamp(-30.0, 8.0);
    let mu = eta.exp();
    let p = (-(-mu).exp_m1()).max(1e-12);
    let weight = mu * mu * (-mu).exp() / p;
    if played {
        (p.ln(), mu * (-mu).exp() / p, weight)
    } else {
        (-mu, -mu, weight)
    }
}

/// Fisher scoring with step halving, from `start`.
fn scoring(rows: &[Vec<f64>], played: &[bool], prior: &[f64], start: Vec<f64>) -> Vec<f64> {
    let dim = prior.len();
    let mut theta = start;
    let mut best = objective(rows, played, prior, &theta);
    for _ in 0..MAX_STEPS {
        let mut gradient: Vec<f64> = theta.iter().zip(prior).map(|(t, p)| -PRIOR_STRENGTH * (t - p)).collect();
        let mut information = vec![vec![0.0; dim]; dim];
        for (index, row) in information.iter_mut().enumerate() {
            row[index] = PRIOR_STRENGTH;
        }
        for (row, played) in rows.iter().zip(played) {
            let (_, slope, weight) = row_terms(row, *played, &theta);
            for i in 0..dim {
                gradient[i] += slope * row[i];
                for j in 0..dim {
                    information[i][j] += weight * row[i] * row[j];
                }
            }
        }
        let Some(step) = solve(information, gradient) else { break };
        let mut scale = 1.0;
        let improved = loop {
            let candidate: Vec<f64> = theta.iter().zip(&step).map(|(t, s)| t + scale * s).collect();
            let value = objective(rows, played, prior, &candidate);
            if value >= best {
                break Some((candidate, value));
            }
            scale /= 2.0;
            if scale < 1e-6 {
                break None;
            }
        };
        let Some((candidate, value)) = improved else { break };
        let moved = candidate.iter().zip(&theta).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
        theta = candidate;
        best = value;
        if moved < TOLERANCE {
            break;
        }
    }
    theta
}

/// Solve `a · x = b` by Gaussian elimination with partial pivoting; `None`
/// when `a` is singular.
fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for column in 0..n {
        let pivot = (column..n).max_by(|&i, &j| a[i][column].abs().total_cmp(&a[j][column].abs()))?;
        if a[pivot][column].abs() < 1e-12 {
            return None;
        }
        a.swap(column, pivot);
        b.swap(column, pivot);
        for row in column + 1..n {
            let factor = a[row][column] / a[column][column];
            let (above, below) = a.split_at_mut(row);
            for (target, source) in below[0][column..].iter_mut().zip(&above[column][column..]) {
                *target -= factor * source;
            }
            b[row] -= factor * b[column];
        }
    }
    let mut x = vec![0.0; n];
    for row in (0..n).rev() {
        let tail: f64 = (row + 1..n).map(|k| a[row][k] * x[k]).sum();
        x[row] = (b[row] - tail) / a[row][row];
    }
    Some(x)
}
