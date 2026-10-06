//! Expected eviction regret: how much the household loses if an item goes.
//!
//! ```text
//! R = P(watch within the horizon | history) × C_reacq × A_household
//! ```
//!
//! - **P(watch)** comes from an exponential hazard, `λ = λ₀·exp(βᵀx)`, so
//!   `P = 1 − exp(−λ·H)`. Features: days since the last play (right-censored to
//!   days on disk when never played), lifetime finished viewings, plays of the
//!   same show in the last 14 days, and an annual cycle (`cos` of the days since
//!   the last play over a year, for the film played every December). Anyone
//!   partway through it raises P to at least 0.95.
//! - **C_reacq** is how hard it is to get back: bigger files, few seeders and
//!   usenet copies past retention cost more.
//! - **A_household** is who still wants it: on someone's watchlist, or
//!   requested by them, weighted per user.
//!
//! The hand-set priors follow this household's own record: finished titles are
//! almost never replayed (110 finished in 200 days, none replayed more than a
//! day later), so finishing earns nothing and the base rate is low. Watching
//! the show right now is the strong signal, and partway is floored at 0.95.
//! The daily fit ([`crate::fit`]) replaces them once the panel can.

use crate::fit::plays::{Play, Viewer};
use std::collections::{HashMap, HashSet};

const DAY_SECS: u64 = 86_400;

/// How far ahead "will it be watched" looks, in days.
pub const HORIZON_DAYS: f64 = 90.0;
/// Plays of the same show this recent count as active watching.
pub const VELOCITY_DAYS: u64 = 14;
/// A viewer with a play this recent is active.
pub const ACTIVE_VIEWER_DAYS: u64 = 30;
/// A viewer this far into an item is partway through it.
pub const PARTWAY: std::ops::RangeInclusive<f32> = 0.10..=0.90;
/// P(watch) floor for an item someone is partway through.
pub const PARTWAY_FLOOR: f64 = 0.95;
/// C_reacq never falls below this: a tiny file is cheap to replace, never free.
pub const MIN_FRICTION: f64 = 0.1;

/// The hazard's parameters. `λ₀` is per day.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HazardModel {
    pub lambda0_per_day: f64,
    /// Per `ln(1 + days since the last play)`; negative: older is colder.
    pub beta_recency: f64,
    /// Per `ln(1 + finished viewings)`.
    pub beta_scrobbles: f64,
    /// Per `ln(1 + plays of the show in the last 14 days)`.
    pub beta_velocity: f64,
    /// Per unit of the annual cycle, −1..1.
    pub beta_cyclical: f64,
}

impl Default for HazardModel {
    fn default() -> Self {
        Self { lambda0_per_day: 0.004, beta_recency: -0.5, beta_scrobbles: 0.0, beta_velocity: 0.7, beta_cyclical: 0.3 }
    }
}

/// What the hazard reads about one item.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WatchFeatures {
    /// Days since the last play; days on disk when never played.
    pub days_since_last_play: f64,
    /// Never played: `days_since_last_play` is censored at acquisition.
    pub never_played: bool,
    /// Finished viewings: plays of a movie, or finished episodes over the
    /// season's episode count.
    pub lifetime_scrobbles: f64,
    /// Plays of the item's show (a movie's own) in the last [`VELOCITY_DAYS`].
    pub active_series_velocity: u32,
    /// `cos(2π·days_since_last_play / 365.25)`; 0 when never played.
    pub annual_cyclical_offset: f64,
    /// An active viewer is partway through it.
    pub partway: bool,
}

/// How many parameters the hazard has: `ln λ₀` and the four β.
pub const PARAMS: usize = 5;

impl WatchFeatures {
    /// The hazard's design row: `[1, ln(1+days), ln(1+viewings), ln(1+show plays), cycle]`,
    /// so `ln λ = params · design`. The fitter and the planner both read λ through it.
    pub fn design(&self) -> [f64; PARAMS] {
        let ln1p = |value: f64| value.max(0.0).ln_1p();
        [
            1.0,
            ln1p(self.days_since_last_play),
            ln1p(self.lifetime_scrobbles),
            ln1p(f64::from(self.active_series_velocity)),
            self.annual_cyclical_offset,
        ]
    }
}

impl HazardModel {
    /// `[ln λ₀, β_recency, β_scrobbles, β_velocity, β_cyclical]`.
    pub fn params(&self) -> [f64; PARAMS] {
        [self.lambda0_per_day.ln(), self.beta_recency, self.beta_scrobbles, self.beta_velocity, self.beta_cyclical]
    }

    pub fn from_params(params: [f64; PARAMS]) -> Self {
        let [ln_lambda0, beta_recency, beta_scrobbles, beta_velocity, beta_cyclical] = params;
        Self { lambda0_per_day: ln_lambda0.exp(), beta_recency, beta_scrobbles, beta_velocity, beta_cyclical }
    }

    /// `ln λ`: the linear predictor.
    pub fn log_hazard(&self, x: &WatchFeatures) -> f64 {
        self.params().iter().zip(x.design()).map(|(param, value)| param * value).sum()
    }

    /// λ, per day.
    pub fn hazard(&self, x: &WatchFeatures) -> f64 {
        self.log_hazard(x).exp()
    }

    /// P(watched within [`HORIZON_DAYS`]), floored at [`PARTWAY_FLOOR`] when
    /// someone is partway through.
    pub fn p_watch(&self, x: &WatchFeatures) -> f64 {
        let p = 1.0 - (-self.hazard(x) * HORIZON_DAYS).exp();
        let p = if p.is_finite() { p.clamp(0.0, 1.0) } else { 1.0 };
        if x.partway {
            p.max(PARTWAY_FLOOR)
        } else {
            p
        }
    }
}

/// The plays one item's features are read from.
#[derive(Debug, Clone, Copy)]
pub struct PlayHistory<'a> {
    /// Plays of exactly this item, oldest first.
    pub item: &'a [&'a Play],
    /// Plays of its show (a movie's own), oldest first.
    pub audience: &'a [&'a Play],
    /// Episodes in the season; `None` for a movie.
    pub episodes_total: Option<u32>,
    /// Days since the last play from the merged watch state, which also
    /// knows plays the logs do not.
    pub last_watched_days: Option<f32>,
    pub added_days_ago: f32,
}

impl WatchFeatures {
    pub fn read(history: &PlayHistory, now: u64) -> Self {
        let last_play = history.item.iter().filter(|play| play.epoch <= now).map(|play| play.epoch).max();
        let from_log = last_play.map(|epoch| (now - epoch) as f64 / DAY_SECS as f64);
        let watched = history.last_watched_days.map(f64::from).or(from_log);
        let days_since_last_play = watched.unwrap_or(f64::from(history.added_days_ago)).max(0.0);
        let finished = history.item.iter().filter(|play| play.complete()).count() as f64;
        let lifetime_scrobbles = match history.episodes_total {
            Some(episodes) => finished / f64::from(episodes.max(1)),
            None => finished,
        };
        let since = now.saturating_sub(VELOCITY_DAYS * DAY_SECS);
        let active_series_velocity = history.audience.iter().filter(|play| play.epoch >= since && play.epoch <= now).count() as u32;
        let annual_cyclical_offset = if watched.is_some() { (std::f64::consts::TAU * days_since_last_play / 365.25).cos() } else { 0.0 };
        Self {
            days_since_last_play,
            never_played: watched.is_none(),
            lifetime_scrobbles,
            active_series_velocity,
            annual_cyclical_offset,
            partway: partway(history, now),
        }
    }
}

/// Some active viewer is between [`PARTWAY`] through the item: for a movie,
/// their latest play stopped there; for a season, that share of its episodes
/// is finished. Plays with no viewer count as one anonymous household viewer.
fn partway(history: &PlayHistory, now: u64) -> bool {
    let active_since = now.saturating_sub(ACTIVE_VIEWER_DAYS * DAY_SECS);
    let mut by_viewer: HashMap<Option<&Viewer>, Vec<&Play>> = HashMap::new();
    for play in history.item.iter().filter(|play| play.epoch <= now) {
        by_viewer.entry(play.viewer.as_ref()).or_default().push(play);
    }
    by_viewer.values().any(|plays| {
        let Some(latest) = plays.iter().max_by_key(|play| play.epoch) else { return false };
        if latest.epoch < active_since {
            return false;
        }
        let progress = match history.episodes_total {
            None => latest.fraction,
            Some(episodes) => {
                let finished: HashSet<u32> = plays.iter().filter(|play| play.complete()).filter_map(|play| play.episode).collect();
                finished.len() as f32 / episodes.max(1) as f32
            }
        };
        PARTWAY.contains(&progress)
    })
}

/// What re-acquiring an item would take.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reacquisition {
    pub size_bytes: u64,
    /// Most seeders on any torrent release; `None` when unknown.
    pub seeders: Option<u32>,
    /// No usenet copy is within the servers' retention.
    pub usenet_out_of_retention: bool,
}

impl Reacquisition {
    /// `1 + 0.3·log₁₀(S / 1 GB) + 2 / max(seeders, 1) + 5·[out of retention]`,
    /// at least [`MIN_FRICTION`]. Unknown seeders add nothing: missing evidence
    /// never makes an item look harder to replace than it is.
    pub fn friction(&self) -> f64 {
        let size = 0.3 * (self.size_bytes.max(1) as f64 / 1e9).log10();
        let seeders = self.seeders.map_or(0.0, |seeders| 2.0 / f64::from(seeders.max(1)));
        let retention = if self.usenet_out_of_retention { 5.0 } else { 0.0 };
        (1.0 + size + seeders + retention).max(MIN_FRICTION)
    }
}

/// One user's claim on an item.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Claim {
    pub weight: f64,
    pub watchlisted: bool,
    pub requested: bool,
}

/// `1 + max_u w_u·(2·[watchlist] + 1.5·[requester])`. The leading 1 keeps an
/// item nobody claimed at its own P × C instead of a regret of zero, which
/// would let the solver evict anything unclaimed for free.
pub fn household(claims: impl IntoIterator<Item = Claim>) -> f64 {
    let strongest = claims
        .into_iter()
        .map(|claim| claim.weight.max(0.0) * (2.0 * f64::from(u8::from(claim.watchlisted)) + 1.5 * f64::from(u8::from(claim.requested))))
        .fold(0.0, f64::max);
    1.0 + strongest
}

/// One item's regret and its parts.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Regret {
    pub p_watch: f64,
    pub friction: f64,
    pub household: f64,
    pub value: f64,
}

impl Regret {
    pub fn new(p_watch: f64, friction: f64, household: f64) -> Self {
        Self { p_watch, friction, household, value: p_watch * friction * household }
    }
}

/// The reason a selected item is cheap to lose, in a few words.
pub fn describe(features: &WatchFeatures, regret: &Regret) -> String {
    let age = months_or_days(features.days_since_last_play);
    let history = if features.never_played { format!("Never played, {age} on disk") } else { format!("Watched {age} ago") };
    let viewings = match features.lifetime_scrobbles {
        n if n < 0.5 => String::new(),
        n if n < 1.5 => " · 1 viewing".to_string(),
        n => format!(" · {} viewings", n.round()),
    };
    format!("{history}{viewings} · P(watch) {:.0}% · regret {:.2}", regret.p_watch * 100.0, regret.value)
}

fn months_or_days(days: f64) -> String {
    if days < 60.0 {
        format!("{} d", days.round())
    } else {
        format!("{} mo", (days / 30.4).round())
    }
}

#[cfg(test)]
mod tests;
