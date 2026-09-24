//! Fit the reclaim scorecard on this household's own playback record.
//!
//! The priors in [`crate::score::ScoreWeights`] encode domain sense, not this
//! library's behaviour. The published lesson from production decision models is
//! blunt: a threshold is meaningless until the score is calibrated, and weights
//! should be fitted on internal examples rather than hand-set. This module builds
//! that dataset out of what the daemon already records — the media server's
//! playback history, Tautulli's streams and the *arr inventory — and fits the
//! same features inference uses.
//!
//! Three deliberate constraints keep it honest:
//!
//! 1. **Temporal.** Features at a cut date use only plays *before* that date; the
//!    label is whether a play happened in the horizon *after* it. Nothing the
//!    model is asked to predict leaks into its inputs.
//! 2. **Grouped.** Every row is judged out of fold, with folds split by
//!    library item, not by row, so the same title at six cut dates cannot be
//!    validated against itself.
//! 3. **Gated.** A model is adopted only if it beats the priors out of fold on
//!    Brier *and* keeps its discrimination, and only with enough examples for
//!    its size ([`candidate`]): recalibrating the priors needs far fewer than a
//!    full refit. Otherwise the priors stay and the report says so.
//!
//! The same panel is exported for external decision models ([`export`]) and
//! their answers scored against FLINCH's on identical rows ([`bench`]), so any
//! claim that one model beats another is a command anyone can rerun.

pub mod adopt;
pub mod bench;
pub mod calibrate;
pub mod candidate;
pub mod eval;
pub mod export;
pub mod load;
pub mod panel;
pub mod plays;
pub mod train;

use crate::card::LibraryKind;
use crate::score;
use crate::watch::WatchSource;
use panel::Example;
use plays::Play;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Default horizon: how far ahead "would it be played?" is asked.
///
/// This is the time window the model's safety claim is scoped to — "nothing will
/// play it for a month" — and it is deliberately short: a long horizon cannot be
/// fully observed from recent history, and an unobserved window silently gets
/// labelled "safe", which biases the fit toward deleting things.
pub const DEFAULT_HORIZON_DAYS: f32 = 30.0;
/// Cut dates relative to now, in days — monthly, out to two years. A small
/// library only becomes a usable panel by asking the same question repeatedly at
/// different dates; each row is a real "as of" question, not a synthetic one.
pub const CUT_STEP_DAYS: f32 = 30.0;
pub const MAX_CUT_DAYS: f32 = 720.0;
/// Below this, the priors are simply better than anything a fit can say.
pub const MIN_EXAMPLES: usize = 120;
/// A recalibration moves two numbers, not every weight, so it needs a third
/// of the rows — and still both outcomes, from at least two titles.
pub const MIN_RECALIBRATION_EXAMPLES: usize = 40;
pub const MIN_RECALIBRATION_OUTCOMES: usize = 2;
/// Both classes need real representation: a one-class panel makes Brier look
/// perfect while the model discriminates nothing.
pub const MIN_POSITIVES: usize = 12;
/// Discrimination floor for adoption. Predicting the base rate is not a model.
pub const MIN_AUC: f32 = 0.60;
/// The temperature the hand-set priors are deployed at. Every comparison with
/// the priors uses it, so they are judged as they actually run.
pub const DEPLOYED_PRIOR_TEMPERATURE: f32 = 1.6;
/// Item-grouped folds. Fold 0 is the validation split the adoption gate uses;
/// all of them together give every row an out-of-fold prediction.
pub const FOLDS: usize = 4;
/// The operating floor the daemon runs with unless the operator moves it
/// (Settings → score floor). Audits of what the floor would have flagged use it.
pub const OPERATING_FLOOR: f32 = 0.75;

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
    /// Provenance of the watch evidence we currently hold, if any.
    pub watch_source: Option<WatchSource>,
    /// Current structural flag, used as a static approximation (see [`panel`]).
    pub is_newest_season: bool,
    /// Sonarr's series status ("continuing", "ended", …), seasons only.
    pub series_status: Option<String>,
    /// When the series' latest episode aired, seasons only.
    pub last_aired_epoch: Option<u64>,
    /// Resolved to Plex by catalogue id. Tautulli's silence is only ever claimed
    /// for such items, so the panel grants it nowhere else either.
    pub guid_resolved: bool,
    /// Genre names from the *arr; a current fact, like size (see [`panel`]).
    /// The taste signal counts outcomes by them ([`crate::taste`]).
    pub genres: Vec<String>,
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
        let finished = self.plays.iter().filter(|play| play.complete && play.epoch < cut);
        let numbered: HashSet<u32> = finished.clone().filter_map(|play| play.episode).collect();
        let unnumbered = finished.filter(|play| play.episode.is_none()).count();
        (numbered.len() + unnumbered) as u32
    }

    /// Watched to the end at least once before `cut` (movies).
    pub fn finished_before(&self, cut: u64) -> bool {
        self.plays.iter().any(|play| play.complete && play.epoch < cut)
    }

    pub fn played_between(&self, from: u64, to: u64) -> bool {
        self.plays.iter().any(|play| play.epoch >= from && play.epoch < to)
    }

    pub fn added_epoch(&self, now: u64) -> u64 {
        now.saturating_sub((self.age_days * 86_400.0) as u64)
    }
}

/// A fitted scorecard, as written to `weights.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FittedModel {
    /// Human-readable provenance: what produced these numbers.
    pub fitted_on: String,
    /// Which candidate this is; each has its own data requirements.
    #[serde(default)]
    pub kind: candidate::ModelKind,
    pub weights: HashMap<String, f32>,
    pub bias: f32,
    pub temperature: f32,
    pub metrics: Metrics,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Metrics {
    pub examples: usize,
    /// Rows judged out of fold: every panel row.
    pub validation: usize,
    pub positives: usize,
    pub auc: f32,
    pub brier: f32,
    pub ece: f32,
    pub priors_auc: f32,
    pub priors_brier: f32,
    pub priors_ece: f32,
    pub horizon_days: f32,
    pub fitted_at_unix: u64,
    /// Rows the model flags at the operating floor, out of fold.
    pub flagged_at_floor: usize,
    pub precision_at_floor: f32,
    /// Panel-wide audit of the *prior* model through the operating floor: how
    /// many items it would have flagged, and how many of those were played
    /// afterwards. The second number is the one that costs trust if it is large.
    pub priors_flagged: usize,
    pub priors_flagged_then_played: usize,
    /// Negatives per item: how many distinct titles carry the outcome signal.
    pub negative_items: usize,
    /// Held-out intervals and confident errors of the fitted model; defaulted
    /// so a `fit.json` written before they existed still loads.
    #[serde(default)]
    pub spread: eval::Spread,
    #[serde(default)]
    pub priors_spread: eval::Spread,
}

impl FittedModel {
    /// The weights this model implies, with policy signals left at their priors.
    pub fn weights(&self) -> score::ScoreWeights {
        let mut weights = score::ScoreWeights::default();
        for (name, value) in &self.weights {
            // Frozen by design: policy, not preference — never from a file.
            if !score::ScoreWeights::frozen().contains(&name.as_str()) {
                weights.set(name, *value);
            }
        }
        weights.bias = self.bias;
        weights
    }

    /// Is this model better than the priors out of fold, by enough to trust?
    ///
    /// Deliberately strict, because a wrong "safe" here deletes something the
    /// household wanted: the model must actually rank (not just predict the base
    /// rate, which scores a beautiful Brier on a lopsided panel), must not lose
    /// discrimination against the priors, and must have seen both outcomes.
    pub fn beats_priors(&self) -> bool {
        self.shortfall().is_none()
    }

    /// The first adoption requirement this model misses, in the operator's
    /// words; `None` when it beats the priors. One source for the gate and for
    /// the status page, so the page can never claim a reason the gate did not use.
    pub fn shortfall(&self) -> Option<String> {
        shortfall(self.kind, &self.metrics)
    }
}

/// The adoption gate for a model of `kind` with these out-of-fold metrics.
pub fn shortfall(kind: candidate::ModelKind, metrics: &Metrics) -> Option<String> {
    let played = metrics.examples.saturating_sub(metrics.positives);
    let (min_examples, min_each) = match kind {
        candidate::ModelKind::Recalibrated => (MIN_RECALIBRATION_EXAMPLES, MIN_RECALIBRATION_OUTCOMES),
        candidate::ModelKind::Full => (MIN_EXAMPLES, MIN_POSITIVES),
    };
    let model = kind.label();
    if metrics.examples < min_examples {
        return Some(format!("{} of the {min_examples} panel rows the {model} needs", metrics.examples));
    }
    if metrics.positives < min_each || played < min_each {
        return Some(format!("{played} played and {} unplayed outcomes; the {model} needs {min_each} of each", metrics.positives));
    }
    if metrics.negative_items < MIN_RECALIBRATION_OUTCOMES {
        return Some(format!(
            "the played outcomes come from {} title(s); the {model} needs {MIN_RECALIBRATION_OUTCOMES}",
            metrics.negative_items
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

/// Load a fitted model from the state directory, if one was adopted.
///
/// Absence is the normal case and means "priors": the daemon must run with the
/// hand-set scorecard when no fit has earned adoption, and must never require one.
pub fn load_model(state_dir: &std::path::Path) -> Option<FittedModel> {
    let text = std::fs::read_to_string(state_dir.join("weights.json")).ok()?;
    let model: FittedModel = serde_json::from_str(&text).ok()?;
    // A model that no longer clears the bar is not used, however it got there.
    if model.beats_priors() {
        Some(model)
    } else {
        None
    }
}

/// P(safe) for each example exactly as the daemon's plan would have gated it at
/// the cut — the same `score` call, hard-guard ceiling and policy terms
/// included. What the floor would have flagged is judged on this.
pub fn probabilities(examples: &[Example], weights: &score::ScoreWeights, temperature: f32) -> Vec<f32> {
    scored(examples, weights, temperature, |score| score.p_safe)
}

/// The forecast for each example: the number the UI shows and every accuracy
/// metric scores. Policy terms and guard ceilings decide the plan and predict
/// nothing, so they stay out (see [`score::ReclaimScore::forecast`]).
pub fn forecasts(examples: &[Example], weights: &score::ScoreWeights, temperature: f32) -> Vec<f32> {
    scored(examples, weights, temperature, |score| score.forecast)
}

fn scored(examples: &[Example], weights: &score::ScoreWeights, temperature: f32, pick: fn(&score::ReclaimScore) -> f32) -> Vec<f32> {
    examples.iter().map(|example| pick(&score::score(&example.card, example.ctx, weights, temperature))).collect()
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
