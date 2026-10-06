//! Fit the hazard behind P(watch) on this household's own playback record.
//!
//! The priors in [`crate::regret::HazardModel`] encode domain sense, not this
//! library's behaviour. This module builds a panel out of what the daemon
//! already records — the media server's playback history, Tautulli's streams
//! and the *arr inventory — and fits the same features inference uses.
//!
//! Three constraints keep it honest:
//!
//! 1. **Temporal.** Features at a cut date use only plays *before* it; the
//!    label is whether a play happened in the horizon *after* it.
//! 2. **Grouped.** Every row is judged out of fold, with folds split by
//!    library item, so the same title at six cut dates is never validated
//!    against itself.
//! 3. **Gated.** A fit is adopted only if it beats the priors out of fold on
//!    Brier, keeps its discrimination (AUC, the C-index of a binary outcome),
//!    and has enough outcomes for its size. Otherwise the priors stay and the
//!    report says why.

pub mod adopt;
pub mod candidate;
pub mod eval;
pub mod load;
pub mod panel;
pub mod plays;
pub mod train;

use crate::card::LibraryKind;
use crate::regret::HazardModel;
use plays::Play;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// The horizon P(watch) is asked over: the planner's own.
pub const HORIZON_DAYS: f32 = crate::regret::HORIZON_DAYS as f32;
/// Cut dates relative to now, in days — monthly, out to two years. Each row
/// is a real "as of" question, not a synthetic one.
pub const CUT_STEP_DAYS: f32 = 30.0;
pub const MAX_CUT_DAYS: f32 = 720.0;
/// Below this, the priors are simply better than anything a full fit can say.
pub const MIN_EXAMPLES: usize = 120;
/// A recalibration moves two numbers, so it needs a third of the rows, and
/// still both outcomes from at least two titles.
pub const MIN_RECALIBRATION_EXAMPLES: usize = 40;
pub const MIN_RECALIBRATION_OUTCOMES: usize = 2;
/// Both outcomes need real representation in a full fit.
pub const MIN_OUTCOMES: usize = 12;
/// Discrimination floor for adoption. Predicting the base rate is not a model.
pub const MIN_AUC: f32 = 0.60;
/// Item-grouped folds; together they give every row an out-of-fold forecast.
pub const FOLDS: usize = 4;

/// Monthly cut dates out to [`MAX_CUT_DAYS`].
pub fn default_cuts() -> Vec<f32> {
    let mut cuts = Vec::new();
    let mut days = CUT_STEP_DAYS;
    while days <= MAX_CUT_DAYS {
        cuts.push(days);
        days += CUT_STEP_DAYS;
    }
    cuts
}

/// A library item as the fitter sees it: current facts plus its dated plays.
#[derive(Debug, Clone)]
pub struct FitItem {
    pub id: String,
    pub title: String,
    pub kind: LibraryKind,
    pub size_bytes: u64,
    /// Days since it was added, as of now.
    pub age_days: f32,
    pub episodes_total: Option<u32>,
    pub season_index: Option<u32>,
    pub show_title: Option<String>,
    /// Plays of this exact item, oldest first, finished or not.
    pub plays: Vec<Play>,
    /// Plays that speak for its audience, oldest first: the movie's own, or
    /// every season of its show (see [`plays::PlayLog::audience_plays`]).
    pub audience_plays: Vec<Play>,
    /// When it was on disk, from *arr history ([`crate::presence`]); empty
    /// when there is none, and the panel then dates arrival as it always did.
    pub on_disk: Vec<crate::presence::Span>,
}

impl FitItem {
    pub fn last_play_before(&self, cut: u64) -> Option<u64> {
        self.plays.iter().filter(|play| play.epoch < cut).map(|play| play.epoch).max()
    }

    pub fn plays_before(&self, cut: u64) -> usize {
        self.plays.iter().filter(|play| play.epoch < cut).count()
    }

    /// Distinct episodes watched to the end before `cut`. A rewatched episode
    /// is not a second episode, and a stream that stopped early completes
    /// nothing. Unnumbered rows cannot be told apart and each count once.
    pub fn episodes_played_before(&self, cut: u64) -> u32 {
        let finished = self.plays.iter().filter(|play| play.complete() && play.epoch < cut);
        let numbered: HashSet<u32> = finished.clone().filter_map(|play| play.episode).collect();
        let unnumbered = finished.filter(|play| play.episode.is_none()).count();
        (numbered.len() + unnumbered) as u32
    }

    /// Watched to the end at least once before `cut` (movies).
    pub fn finished_before(&self, cut: u64) -> bool {
        self.plays.iter().any(|play| play.complete() && play.epoch < cut)
    }

    pub fn played_between(&self, from: u64, to: u64) -> bool {
        self.plays.iter().any(|play| play.epoch >= from && play.epoch < to)
    }

    pub fn added_epoch(&self, now: u64) -> u64 {
        now.saturating_sub((self.age_days * 86_400.0) as u64)
    }
}

/// A fitted hazard, as written to `hazard.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FittedModel {
    /// Human-readable provenance: what produced these numbers.
    pub fitted_on: String,
    pub kind: candidate::ModelKind,
    pub hazard: HazardModel,
    pub metrics: Metrics,
}

/// Out-of-fold scores of a candidate and of the priors on the same rows.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Metrics {
    pub examples: usize,
    /// Rows played during their horizon.
    pub played: usize,
    /// Distinct titles with a played row: how many carry the outcome signal.
    pub played_items: usize,
    pub auc: f32,
    pub brier: f32,
    pub ece: f32,
    pub priors_auc: f32,
    pub priors_brier: f32,
    pub priors_ece: f32,
    pub horizon_days: f32,
    pub fitted_at_unix: u64,
    #[serde(default)]
    pub spread: eval::Spread,
    #[serde(default)]
    pub priors_spread: eval::Spread,
}

impl FittedModel {
    /// The first adoption requirement this model misses, in the operator's
    /// words; `None` when it beats the priors. One source for the gate and the
    /// status page.
    pub fn shortfall(&self) -> Option<String> {
        shortfall(self.kind, &self.metrics)
    }
}

/// The adoption gate for a model of `kind` with these out-of-fold metrics.
pub fn shortfall(kind: candidate::ModelKind, metrics: &Metrics) -> Option<String> {
    let unplayed = metrics.examples.saturating_sub(metrics.played);
    let (min_examples, min_each) = match kind {
        candidate::ModelKind::Recalibrated => (MIN_RECALIBRATION_EXAMPLES, MIN_RECALIBRATION_OUTCOMES),
        candidate::ModelKind::Full => (MIN_EXAMPLES, MIN_OUTCOMES),
    };
    let model = kind.label();
    if metrics.examples < min_examples {
        return Some(format!("{} of the {min_examples} panel rows the {model} needs", metrics.examples));
    }
    if metrics.played < min_each || unplayed < min_each {
        return Some(format!("{} played and {unplayed} unplayed outcomes; the {model} needs {min_each} of each", metrics.played));
    }
    if metrics.played_items < MIN_RECALIBRATION_OUTCOMES {
        return Some(format!(
            "the played outcomes come from {} title(s); the {model} needs {MIN_RECALIBRATION_OUTCOMES}",
            metrics.played_items
        ));
    }
    if metrics.auc < MIN_AUC {
        return Some(format!("out-of-fold AUC {:.2} is below {MIN_AUC:.2}: it does not rank", metrics.auc));
    }
    if metrics.brier >= metrics.priors_brier - 0.005 {
        return Some(format!("out-of-fold Brier {:.3} does not beat the priors' {:.3}", metrics.brier, metrics.priors_brier));
    }
    if metrics.auc < metrics.priors_auc - 0.02 {
        return Some(format!("out-of-fold AUC {:.2} falls behind the priors' {:.2}", metrics.auc, metrics.priors_auc));
    }
    None
}

/// The adopted hazard, if one is on file and still clears the gate. Absence is
/// the normal case and means the hand-set priors.
pub fn load_model(state_dir: &std::path::Path) -> Option<HazardModel> {
    let text = std::fs::read_to_string(state_dir.join(adopt::MODEL_FILE)).ok()?;
    let model: FittedModel = serde_json::from_str(&text).ok()?;
    model.shortfall().is_none().then_some(model.hazard)
}

thread_local! {
    /// Deterministic split seed: the same data must always produce the same
    /// report, or nobody can check a decision twice.
    static SPLIT_SALT: u64 = 0x5eed_1234_abcd;
}

/// Fold membership by item id — the grouping that stops leakage.
pub fn fold_of(item_id: &str) -> usize {
    SPLIT_SALT.with(|salt| {
        let mut hash = *salt ^ 0xcbf2_9ce4_8422_2325;
        for byte in item_id.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
        (hash % 100) as usize * FOLDS / 100
    })
}

#[cfg(test)]
mod tests;
