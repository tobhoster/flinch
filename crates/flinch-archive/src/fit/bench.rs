//! Head-to-head: an external decision model's P(safe) against FLINCH's, on the
//! exact same panel rows.
//!
//! The external model answers the exported panel ([`super::export`]) with one
//! `{ "id", "cut_days", "p" }` line per row (`cut_unix` optional). Its answers
//! are joined back to the rebuilt panel, and every metric is computed on the
//! joined rows only, for all three models — so no model is judged on questions
//! another one skipped. Nothing is dropped silently: every prediction line is
//! either joined or counted under the reason it was not.
//!
//! `flinch-fit --against` skips the file: it asks a System One server the
//! panel directly ([`Predictions::answered`]) and records the result as a
//! [`Benchmark`] the status page shows.

use super::candidate::{self, ModelKind};
use super::eval::{Difference, Scorecard};
use super::panel::Example;
use super::{forecasts, DEPLOYED_PRIOR_TEMPERATURE};
use crate::score::ScoreWeights;
use crate::systemone::Response;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

pub mod ask;

/// The question key `--against` asks each panel row under.
pub const QUESTION: &str = "played";
/// Where the last `--against --write` result lives in the state dir.
pub const BENCHMARK_FILE: &str = "benchmark.json";

/// Invalid line numbers kept for the report; the count is always complete.
const INVALID_LINES_SHOWN: usize = 10;

/// Join resolution for `cut_days`: a thousandth of a day. A float that
/// round-trips through another language never moves that far, and distinct
/// cut dates are whole days apart.
fn cut_key(cut_days: f32) -> i64 {
    (f64::from(cut_days) * 1000.0).round() as i64
}

#[derive(Debug, Deserialize)]
struct PredictionLine {
    id: String,
    cut_days: f32,
    p: f32,
    #[serde(default)]
    cut_unix: Option<u64>,
}

/// An external model's answers, as read from a predictions file.
#[derive(Debug, Default)]
pub struct Predictions {
    rows: Vec<PredictionLine>,
    /// 1-based numbers of non-blank lines that were not a prediction: not JSON,
    /// a missing field, or a `p` that is not a probability.
    pub invalid_lines: Vec<usize>,
}

impl Predictions {
    /// A System One server's answers, one per panel row in order: P(safe) is
    /// the complement of the P(played) it gave. A failed request (`None`) is
    /// no line at all; an answer that is not a probability is an invalid line
    /// numbered by its panel row, as in `--export-panel`. Both leave the row
    /// unanswered.
    pub fn answered(dataset: &[Example], responses: &[Option<Response>]) -> Self {
        let mut predictions = Self::default();
        for (row, (example, response)) in dataset.iter().zip(responses).enumerate() {
            let Some(response) = response else { continue };
            match response.noul(QUESTION) {
                Some(played) => predictions.rows.push(PredictionLine {
                    id: example.item_id.clone(),
                    cut_days: example.cut_days,
                    p: 1.0 - played,
                    cut_unix: Some(example.cut_unix),
                }),
                None => predictions.invalid_lines.push(row + 1),
            }
        }
        predictions
    }

    pub fn parse(text: &str) -> Self {
        let mut predictions = Self::default();
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<PredictionLine>(line) {
                Ok(row) if (0.0..=1.0).contains(&row.p) && row.cut_days.is_finite() => predictions.rows.push(row),
                _ => predictions.invalid_lines.push(index + 1),
            }
        }
        predictions
    }

    /// Non-blank lines read.
    pub fn lines(&self) -> usize {
        self.rows.len() + self.invalid_lines.len()
    }
}

/// Where every prediction line went.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JoinReport {
    pub prediction_lines: usize,
    pub joined: usize,
    /// Well-formed lines whose `(id, cut_days)` is not a row of this panel.
    pub unjoined: usize,
    /// Lines naming a panel row but echoing a different `cut_unix`: answers to
    /// another panel (exported with a different `--now`).
    pub stale: usize,
    /// Repeat answers to an already-joined row; the first one counts.
    pub duplicates: usize,
    pub invalid: usize,
    /// The first few invalid line numbers.
    pub invalid_lines: Vec<usize>,
    pub panel_rows: usize,
    /// Panel rows the predictions left unanswered.
    pub unanswered: usize,
}

/// Every model on the same joined rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeadToHead {
    pub now_unix: u64,
    pub horizon_days: f32,
    pub join: JoinReport,
    pub external: Scorecard,
    /// The hand-set priors at their deployed temperature.
    pub priors: Scorecard,
    /// The priors recalibrated to the household, out of fold
    /// ([`candidate::out_of_fold`]). Empty in a benchmark written before it existed.
    #[serde(default)]
    pub recalibrated: Scorecard,
    /// The full household fit, out of fold.
    pub fitted: Scorecard,
    /// FLINCH minus the external model on the joined rows, 95% over titles.
    /// Empty in a benchmark written before it existed.
    #[serde(default)]
    pub versus: Versus,
}

/// Each FLINCH candidate against the external model (see [`Difference`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Versus {
    pub recalibrated: Difference,
    pub fitted: Difference,
}

/// A paired interval (FLINCH minus external) in words: which side it favours,
/// or no clear difference when it spans zero. Lower Brier and log-loss, and
/// higher AUC, are better.
pub fn describe(difference: &Difference) -> String {
    let verdict = |interval: Option<[f32; 2]>, lower_wins: bool| match interval {
        None => "too few titles to say".to_string(),
        Some([low, high]) => {
            let (flinch, external) = if lower_wins { (high < 0.0, low > 0.0) } else { (low > 0.0, high < 0.0) };
            let word = if flinch {
                "FLINCH better"
            } else if external {
                "external better"
            } else {
                "no clear difference"
            };
            format!("{word} [{low:+.3}, {high:+.3}]")
        }
    };
    format!(
        "Brier {} · log-loss {} · AUC {}",
        verdict(difference.brier, true),
        verdict(difference.log_loss, true),
        verdict(difference.auc, false)
    )
}

/// A head-to-head against a live System One server, as `benchmark.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Benchmark {
    pub model: String,
    /// `scheme://host[:port]` only (see [`endpoint_origin`]).
    pub endpoint: String,
    pub scored_at_unix: u64,
    pub result: HeadToHead,
}

/// The last benchmark `flinch-fit --against --write` recorded, if any.
pub fn read_benchmark(state_dir: &Path) -> Option<Benchmark> {
    serde_json::from_str(&std::fs::read_to_string(state_dir.join(BENCHMARK_FILE)).ok()?).ok()
}

/// A base URL reduced to `scheme://host[:port]`, safe to print and publish:
/// userinfo, path, query and fragment can all carry a credential.
pub fn endpoint_origin(base_url: &str) -> String {
    match reqwest::Url::parse(base_url.trim()) {
        Ok(url) => match (url.host_str(), url.port()) {
            (Some(host), Some(port)) => format!("{}://{host}:{port}", url.scheme()),
            (Some(host), None) => format!("{}://{host}", url.scheme()),
            (None, _) => "unknown".to_string(),
        },
        Err(_) => "unknown".to_string(),
    }
}

pub fn head_to_head(dataset: &[Example], predictions: &Predictions, now_unix: u64, horizon_days: f32) -> HeadToHead {
    let (joined, join) = join(dataset, predictions);
    let labels: Vec<f32> = joined.iter().map(|(index, _)| dataset[*index].label).collect();
    let external: Vec<f32> = joined.iter().map(|(_, p)| *p).collect();
    let on_joined = |all: &[f32]| -> Vec<f32> { joined.iter().map(|(index, _)| all[*index]).collect() };
    let priors = on_joined(&forecasts(dataset, &ScoreWeights::default(), DEPLOYED_PRIOR_TEMPERATURE));
    let priors_card = Scorecard::of(&priors, &labels);
    let groups: Vec<&str> = joined.iter().map(|(index, _)| dataset[*index].item_id.as_str()).collect();
    let judged = |kind| -> (Scorecard, Difference) {
        let all: Vec<f32> = candidate::out_of_fold(kind, dataset).iter().map(|row| row.forecast).collect();
        let own = on_joined(&all);
        (candidate::scorecard(kind, &own, &labels, &priors_card), candidate::difference(kind, &own, &priors, &external, &labels, &groups))
    };
    let (recalibrated, recalibrated_versus) = judged(ModelKind::Recalibrated);
    let (fitted, fitted_versus) = judged(ModelKind::Full);
    HeadToHead {
        now_unix,
        horizon_days,
        join,
        external: Scorecard::of(&external, &labels),
        priors: priors_card,
        recalibrated,
        fitted,
        versus: Versus { recalibrated: recalibrated_versus, fitted: fitted_versus },
    }
}

/// Match prediction lines to panel rows: `(panel index, p)` per joined row.
fn join(dataset: &[Example], predictions: &Predictions) -> (Vec<(usize, f32)>, JoinReport) {
    let index: HashMap<(&str, i64), usize> =
        dataset.iter().enumerate().map(|(row, example)| ((example.item_id.as_str(), cut_key(example.cut_days)), row)).collect();
    let mut answered = vec![false; dataset.len()];
    let mut joined = Vec::new();
    let mut report = JoinReport {
        prediction_lines: predictions.lines(),
        invalid: predictions.invalid_lines.len(),
        invalid_lines: predictions.invalid_lines.iter().take(INVALID_LINES_SHOWN).copied().collect(),
        panel_rows: dataset.len(),
        ..JoinReport::default()
    };
    for line in &predictions.rows {
        match index.get(&(line.id.as_str(), cut_key(line.cut_days))).copied() {
            None => report.unjoined += 1,
            Some(row) if line.cut_unix.is_some_and(|cut| cut != dataset[row].cut_unix) => report.stale += 1,
            Some(row) if answered[row] => report.duplicates += 1,
            Some(row) => {
                answered[row] = true;
                joined.push((row, line.p));
            }
        }
    }
    report.joined = joined.len();
    report.unanswered = dataset.len() - joined.len();
    (joined, report)
}

#[cfg(test)]
mod tests;
