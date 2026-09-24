//! Fitting as the daemon runs it: once a day, on the files it already
//! publishes, adopted only through the out-of-fold gate in
//! [`super::shortfall`]. `flinch-fit` reports and writes through the same two
//! functions, so the fit the status page shows is the one that runs.

use super::candidate::{self, ModelKind};
use super::eval::{self, Scorecard};
use super::load::{self, Household, LoadError};
use super::panel::{self, Example, PanelSpec};
use super::{default_cuts, forecasts, probabilities, FittedModel, Metrics, DEFAULT_HORIZON_DAYS, DEPLOYED_PRIOR_TEMPERATURE, OPERATING_FLOOR};
use crate::score::ScoreWeights;
use crate::taste::{GenreRates, ItemGenres};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// How often the daemon refits: outcomes arrive by the day, not by the cycle.
pub const REFIT_SECS: u64 = 86_400;
const WEIGHTS_FILE: &str = "weights.json";
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
    #[serde(default)]
    pub kind: ModelKind,
    pub fitted_on: String,
    pub metrics: Metrics,
    /// The household's genre play-rates from every outcome closed by the fit:
    /// what the daemon's taste signal reads until the next refit. Empty in a
    /// `fit.json` written before it existed, which means no taste.
    #[serde(default)]
    pub taste: GenreRates,
}

/// What [`adopt`] did with `weights.json`.
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

/// Fit the household panel as the daemon runs it. Every candidate is judged
/// out of fold against the deployed priors on the same rows; the best one by
/// out-of-fold log-loss that clears its own gate is fitted on every row. When
/// none clears, the best one is reported with what it still lacks.
pub fn fit_model(household: &Household, dataset: &[Example], now: u64, horizon_days: f32, cut_count: usize) -> FittedModel {
    let labels: Vec<f32> = dataset.iter().map(|example| example.label).collect();
    let groups: Vec<&str> = dataset.iter().map(|example| example.item_id.as_str()).collect();
    let priors = ScoreWeights::default();
    let prior_forecasts = forecasts(dataset, &priors, DEPLOYED_PRIOR_TEMPERATURE);
    let prior_card = Scorecard::of(&prior_forecasts, &labels);
    let prior_spread = eval::spread(&prior_forecasts, &labels, &groups);
    let audit = Audit::of(dataset, &probabilities(dataset, &priors, DEPLOYED_PRIOR_TEMPERATURE));
    let positives = labels.iter().filter(|label| **label >= 0.5).count();

    let mut judged: Vec<(ModelKind, f32, Metrics)> = ModelKind::ALL
        .iter()
        .map(|&kind| {
            let rows = candidate::out_of_fold(kind, dataset);
            let forecast: Vec<f32> = rows.iter().map(|row| row.forecast).collect();
            let p_safe: Vec<f32> = rows.iter().map(|row| row.p_safe).collect();
            let card = candidate::scorecard(kind, &forecast, &labels, &prior_card);
            let stats = eval::at_threshold(&p_safe, &labels, OPERATING_FLOOR);
            let metrics = Metrics {
                examples: dataset.len(),
                validation: dataset.len(),
                positives,
                auc: card.auc,
                brier: card.brier,
                ece: card.ece,
                priors_auc: prior_card.auc,
                priors_brier: prior_card.brier,
                priors_ece: prior_card.ece,
                horizon_days,
                fitted_at_unix: now,
                flagged_at_floor: stats.flagged,
                precision_at_floor: stats.precision,
                priors_flagged: audit.flagged,
                priors_flagged_then_played: audit.flagged_then_played,
                negative_items: audit.negative_items,
                spread: candidate::spread(kind, eval::spread(&forecast, &labels, &groups), &prior_spread),
                priors_spread: prior_spread.clone(),
            };
            (kind, card.log_loss, metrics)
        })
        .collect();
    judged.sort_by(|a, b| a.1.total_cmp(&b.1));
    let pick = judged.iter().position(|(kind, _, metrics)| super::shortfall(*kind, metrics).is_none()).unwrap_or(0);
    let (kind, _, metrics) = judged.swap_remove(pick);

    let trained = candidate::fit(kind, dataset);
    let items_with_plays = household.items.iter().filter(|item| !item.plays.is_empty()).count();
    FittedModel {
        fitted_on: format!(
            "{} library items ({items_with_plays} with plays), {} history + {} Tautulli rows, panel of {} examples at {cut_count} cut dates, horizon {:.0} d",
            household.items.len(),
            household.plex_rows,
            household.tautulli_rows,
            dataset.len(),
            horizon_days,
        ),
        kind,
        weights: ScoreWeights::names()
            .iter()
            .filter(|name| !ScoreWeights::frozen().contains(name))
            .map(|name| (name.to_string(), trained.weights.get(name)))
            .collect(),
        bias: trained.weights.bias,
        temperature: trained.temperature,
        metrics,
    }
}

/// Write `weights.json` when the fit beats the priors; remove an older one when
/// it does not, so a rejected fit never lingers for the daemon to load.
pub fn adopt(state_dir: &Path, model: &FittedModel) -> Result<Adoption, RefitError> {
    let path = state_dir.join(WEIGHTS_FILE);
    if model.beats_priors() {
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
    let due = read_status(state_dir).map_or(true, |last| now.saturating_sub(last.fitted_at_unix) >= REFIT_SECS);
    (due && state_dir.join("items.json").exists()).then(|| refit(state_dir, now))
}

fn refit(state_dir: &Path, now: u64) -> Result<FitStatus, RefitError> {
    let household = load::load_household(state_dir)?;
    let cuts = default_cuts();
    let spec = PanelSpec {
        now,
        cuts_days: &cuts,
        horizon_days: DEFAULT_HORIZON_DAYS,
        tautulli_coverage_start: household.tautulli_coverage_start,
    };
    let dataset = panel::build_dataset(&household.items, &spec);
    let model = fit_model(&household, &dataset, now, DEFAULT_HORIZON_DAYS, cuts.len());
    let adopted = adopt(state_dir, &model)? == Adoption::Written;
    // Every panel row's window has closed by `now`, so these are the rates a
    // panel row cut today would read.
    let taste = GenreRates::as_of(&dataset, &ItemGenres::of(&household.items), (DEFAULT_HORIZON_DAYS * 86_400.0) as u64, now);
    let status = FitStatus {
        fitted_at_unix: now,
        adopted,
        shortfall: model.shortfall(),
        kind: model.kind,
        fitted_on: model.fitted_on,
        metrics: model.metrics,
        taste,
    };
    crate::persist::replace(&state_dir.join(STATUS_FILE), serde_json::to_string_pretty(&status)?.as_bytes())?;
    Ok(status)
}

/// Panel-wide audit of the deployed priors through the operating floor: how
/// many rows they flag, and how many of those were played afterwards.
struct Audit {
    flagged: usize,
    flagged_then_played: usize,
    negative_items: usize,
}

impl Audit {
    fn of(dataset: &[Example], priors: &[f32]) -> Self {
        let flagged = priors.iter().filter(|p| **p >= OPERATING_FLOOR).count();
        let flagged_then_played =
            dataset.iter().zip(priors).filter(|(example, p)| **p >= OPERATING_FLOOR && example.label < 0.5).count();
        let negative_items: HashSet<&str> =
            dataset.iter().filter(|example| example.label < 0.5).map(|example| example.item_id.as_str()).collect();
        Self { flagged, flagged_then_played, negative_items: negative_items.len() }
    }
}
