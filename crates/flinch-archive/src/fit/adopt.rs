//! Fitting as the daemon runs it: once a day, on the files it already
//! publishes, adopted only through the out-of-fold gate in
//! [`super::shortfall`]. `flinch-fit` reports and writes through the same
//! functions, so the fit the status page shows is the one that runs.

use super::candidate::{self, ModelKind};
use super::eval::{self, Scorecard};
use super::load::{self, Household, LoadError};
use super::panel::{self, Example, PanelSpec};
use super::{default_cuts, FittedModel, Metrics, HORIZON_DAYS};
use crate::regret::HazardModel;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// How often the daemon refits: outcomes arrive by the day, not by the cycle.
pub const REFIT_SECS: u64 = 86_400;
/// The adopted hazard; absent means the hand-set priors.
pub const MODEL_FILE: &str = "hazard.json";
const STATUS_FILE: &str = "fit.json";

/// The last fit, as the status page shows it. Written whether or not the fit
/// was adopted, so "priors" always arrives with its reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FitStatus {
    pub fitted_at_unix: u64,
    pub adopted: bool,
    /// The first adoption requirement the fit missed; `None` when adopted.
    pub shortfall: Option<String>,
    /// Which candidate was judged: the adopted one, or the best that fell short.
    pub kind: ModelKind,
    pub fitted_on: String,
    pub metrics: Metrics,
    /// The fitted parameters, adopted or not, for the report.
    pub hazard: HazardModel,
    /// The hand-set priors it was judged against.
    pub priors: HazardModel,
}

/// What [`adopt`] did with `hazard.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adoption {
    Written,
    /// An earlier fit no longer clears the gate, so its file was removed.
    Removed,
    PriorsKept,
}

#[derive(Debug, thiserror::Error)]
pub enum RefitError {
    #[error(transparent)]
    Load(#[from] LoadError),
    #[error("cannot write the fit: {0}")]
    Write(#[from] std::io::Error),
    #[error("cannot encode the fit: {0}")]
    Encode(#[from] serde_json::Error),
}

fn as_f32(values: &[f64]) -> Vec<f32> {
    values.iter().map(|value| *value as f32).collect()
}

/// Fit the household panel as the daemon runs it. Every candidate is judged
/// out of fold against the priors on the same rows; the best by out-of-fold
/// log-loss that clears its gate is fitted on every row. When none clears, the
/// best is reported with what it still lacks.
pub fn fit_model(household: &Household, dataset: &[Example], now: u64, cut_count: usize) -> FittedModel {
    let labels: Vec<f32> = dataset.iter().map(|example| example.label).collect();
    let groups: Vec<&str> = dataset.iter().map(|example| example.item_id.as_str()).collect();
    let priors = HazardModel::default();
    let prior_forecasts = as_f32(&dataset.iter().map(|example| priors.p_watch(&example.features)).collect::<Vec<_>>());
    let prior_card = Scorecard::of(&prior_forecasts, &labels);
    let prior_spread = eval::spread(&prior_forecasts, &labels, &groups);
    let played_items: HashSet<&str> =
        dataset.iter().filter(|example| example.label >= 0.5).map(|example| example.item_id.as_str()).collect();

    let mut judged: Vec<(ModelKind, f32, Metrics)> = ModelKind::ALL
        .iter()
        .map(|&kind| {
            let forecast = as_f32(&candidate::out_of_fold(kind, dataset));
            let mut card = Scorecard::of(&forecast, &labels);
            let mut spread = eval::spread(&forecast, &labels, &groups);
            // A recalibration is one increasing map of the priors, so it ranks
            // every row as they do: its AUC is theirs. Pooling four per-fold
            // scales would misstate that ranking; the folds judge probabilities.
            if kind == ModelKind::Recalibrated {
                card.auc = prior_card.auc;
                spread.auc = prior_spread.auc;
            }
            let metrics = Metrics {
                examples: dataset.len(),
                played: card.positives,
                played_items: played_items.len(),
                auc: card.auc,
                brier: card.brier,
                ece: card.ece,
                priors_auc: prior_card.auc,
                priors_brier: prior_card.brier,
                priors_ece: prior_card.ece,
                horizon_days: HORIZON_DAYS,
                fitted_at_unix: now,
                spread,
                priors_spread: prior_spread.clone(),
            };
            (kind, card.log_loss, metrics)
        })
        .collect();
    judged.sort_by(|a, b| a.1.total_cmp(&b.1));
    let pick = judged.iter().position(|(kind, _, metrics)| super::shortfall(*kind, metrics).is_none()).unwrap_or(0);
    let (kind, _, metrics) = judged.swap_remove(pick);
    let items_with_plays = household.items.iter().filter(|item| !item.plays.is_empty()).count();
    FittedModel {
        fitted_on: format!(
            "{} library items ({items_with_plays} with plays), {} history + {} Tautulli rows, panel of {} examples at {cut_count} cut dates, horizon {HORIZON_DAYS:.0} d",
            household.items.len(),
            household.plex_rows,
            household.tautulli_rows,
            dataset.len(),
        ),
        kind,
        hazard: candidate::fit(kind, dataset),
        metrics,
    }
}

/// Write `hazard.json` when the fit beats the priors; remove an older one when
/// it does not, so a rejected fit never lingers for the daemon to load.
pub fn adopt(state_dir: &Path, model: &FittedModel) -> Result<Adoption, RefitError> {
    let path = state_dir.join(MODEL_FILE);
    if model.shortfall().is_none() {
        crate::persist::replace(&path, serde_json::to_string_pretty(model)?.as_bytes())?;
        return Ok(Adoption::Written);
    }
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(Adoption::Removed),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Adoption::PriorsKept),
        Err(error) => Err(error.into()),
    }
}

/// The last fit the daemon recorded, if any.
pub fn read_status(state_dir: &Path) -> Option<FitStatus> {
    serde_json::from_str(&std::fs::read_to_string(state_dir.join(STATUS_FILE)).ok()?).ok()
}

/// The daemon's refit: at most once per [`REFIT_SECS`], and only once it has
/// published a library to fit on. `None` when none was due.
pub fn refit_if_due(state_dir: &Path, now: u64) -> Option<Result<FitStatus, RefitError>> {
    let due = read_status(state_dir).is_none_or(|last| now.saturating_sub(last.fitted_at_unix) >= REFIT_SECS);
    (due && state_dir.join("items.json").exists()).then(|| refit(state_dir, now))
}

/// Build the panel `now` and fit it.
pub fn panel_fit(household: &Household, now: u64) -> (Vec<Example>, FittedModel) {
    let cuts = default_cuts();
    let dataset = panel::build_dataset(&household.items, &PanelSpec { now, cuts_days: &cuts, horizon_days: HORIZON_DAYS });
    let model = fit_model(household, &dataset, now, cuts.len());
    (dataset, model)
}

fn refit(state_dir: &Path, now: u64) -> Result<FitStatus, RefitError> {
    let household = load::load_household(state_dir)?;
    let (_, model) = panel_fit(&household, now);
    let adopted = adopt(state_dir, &model)? == Adoption::Written;
    let status = FitStatus {
        fitted_at_unix: now,
        adopted,
        shortfall: model.shortfall(),
        kind: model.kind,
        fitted_on: model.fitted_on,
        metrics: model.metrics,
        hazard: model.hazard,
        priors: HazardModel::default(),
    };
    crate::persist::replace(&state_dir.join(STATUS_FILE), serde_json::to_string_pretty(&status)?.as_bytes())?;
    Ok(status)
}
