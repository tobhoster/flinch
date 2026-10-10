//! Value-aware upgrade search, when the operator asks (`upgrade_search`, off
//! by default): of the items Radarr or Sonarr report below their profile's
//! cutoff, ask a search only for those someone will likely watch, and only
//! where the disk can take the bigger file.
//!
//! The *arrs search cutoff-unmet items blindly (RSS, or a "search all
//! cutoff unmet" button), spending indexer quota and disk on titles nobody
//! returns to. FLINCH already knows P(watch) and each volume's forecast, so
//! it ranks the backlog by P(watch) and spends a small daily budget on the
//! top, each search only where the volume's forecast headroom covers the
//! expected growth: the largest whole release Prowlarr lists minus the file.
//!
//! Never searched: a pinned item or one someone is partway through, one
//! hard to get back (C_reacq above [`MAX_REACQ`]: a search could lose a rare
//! copy to a worse one that merely ranks higher), one the upgrade-churn guard
//! flags, one the plan evicts or already handed over, one searched within
//! [`RETRY_SECS`]. Never beyond the daily cap.
//!
//! Wire shape: `GET /api/v3/wanted/cutoff?page=&pageSize=&monitored=true`
//! answers `PagingResource {page, pageSize, totalRecords, records}`; Radarr's
//! records are movies (`MovieResource.id`,
//! https://github.com/Radarr/Radarr/blob/develop/src/Radarr.Api.V3/Wanted/CutoffController.cs),
//! Sonarr's are episodes (`EpisodeResource {seriesId, seasonNumber}`,
//! https://github.com/Sonarr/Sonarr/blob/v5-develop/src/Sonarr.Api.V3/Wanted/CutoffController.cs).

use super::act::{ArrItem, DAY_SECS};
use crate::capacity::{App, VolumeForecast};
use crate::plan::MediaCandidate;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

mod ledger;
pub use ledger::{Search, SearchLedger, SearchOutcome, LEDGER_KEEP_SECS, OUTCOME_WINDOW_SECS, RETRY_SECS, STATUS_ROWS};

/// Bound of [`UpgradeSearchConfig::max_per_day`].
pub const MAX_PER_DAY: u32 = 50;
/// Above this reacquisition friction an item is not searched.
pub const MAX_REACQ: f64 = 3.0;
/// Records per cutoff page asked for.
pub const PAGE_SIZE: u32 = 250;
/// Pages read per app at most: 10 000 cutoff-unmet records.
pub const MAX_PAGES: u32 = 40;
/// Held rows the status carries, likeliest watched first.
pub const HELD_ROWS: usize = 100;

/// `settings.json` `upgrade_search`. Every field defaults individually.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpgradeSearchConfig {
    /// Search cutoff-unmet items. Off: the *arrs' own schedule only.
    pub enabled: bool,
    /// Searches per 24 hours at most; each season counts.
    pub max_per_day: u32,
}

impl Default for UpgradeSearchConfig {
    fn default() -> Self {
        Self { enabled: false, max_per_day: 5 }
    }
}

/// An [`UpgradeSearchConfig`] outside its bounds; the message names the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidUpgradeSearchConfig(pub &'static str);

impl UpgradeSearchConfig {
    pub fn validate(&self) -> Result<(), InvalidUpgradeSearchConfig> {
        if !(1..=MAX_PER_DAY).contains(&self.max_per_day) {
            return Err(InvalidUpgradeSearchConfig("upgrade searches per day (upgrade_search.max_per_day) must be 1 to 50"));
        }
        Ok(())
    }
}

/// The cutoff path for `page` (1-based).
pub fn cutoff_path(page: u32) -> String {
    format!("/api/v3/wanted/cutoff?page={page}&pageSize={PAGE_SIZE}&monitored=true")
}

/// One cutoff page: the movies or seasons on it, and the records in all.
/// A malformed record is skipped; a page without `records` reads as empty.
pub fn parse_cutoff_page(app: App, page: &serde_json::Value) -> (Vec<(u32, Option<u32>)>, u64) {
    let total = page.get("totalRecords").and_then(serde_json::Value::as_u64).unwrap_or(0);
    let id = |record: &serde_json::Value, key: &str| record.get(key)?.as_u64().and_then(|id| u32::try_from(id).ok());
    let records = page.get("records").and_then(serde_json::Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let items = records
        .iter()
        .filter_map(|record| match app {
            App::Radarr => Some((id(record, "id")?, None)),
            App::Sonarr => Some((id(record, "seriesId")?, Some(id(record, "seasonNumber")?))),
        })
        .collect();
    (items, total)
}

/// The card ids of `unmet` (`(app, instance, movie or series id, season)`),
/// through the library's items; one not in the library is dropped.
pub fn unmet_cards(unmet: &[(App, &str, u32, Option<u32>)], items: &HashMap<String, ArrItem<'_>>) -> HashSet<String> {
    let wanted: HashSet<(App, &str, u32, Option<u32>)> = unmet.iter().copied().collect();
    items
        .iter()
        .filter(|(_, item)| wanted.contains(&(item.app, item.instance, item.id, item.season)))
        .map(|(card, _)| card.clone())
        .collect()
}

/// Bytes each volume may still take before its forecast needs eviction:
/// θ_target·C_max − U_proj − headroom buffer, never below 0.
pub fn volume_headroom(forecasts: &[VolumeForecast], target_utilization: f64, buffer_bytes: u64) -> HashMap<String, u64> {
    forecasts
        .iter()
        .map(|volume| {
            let forecast = &volume.forecast;
            let room = target_utilization * forecast.max_capacity_bytes as f64 - forecast.projected_used_bytes as f64 - buffer_bytes as f64;
            (volume.volume.clone(), room.max(0.0) as u64)
        })
        .collect()
}

/// What a selection reads.
pub struct Inputs<'a> {
    pub candidates: &'a [MediaCandidate],
    pub items: &'a HashMap<String, ArrItem<'a>>,
    /// Card ids below their cutoff.
    pub unmet: &'a HashSet<String>,
    /// Card ids this cycle's plan evicts.
    pub planned: &'a HashSet<&'a str>,
    /// Card ids the upgrade-churn guard flags.
    pub churning: &'a HashSet<&'a str>,
    /// Card id → the largest whole release Prowlarr found, in bytes.
    pub largest_release: &'a HashMap<String, u64>,
    /// Volume → bytes it may still take ([`volume_headroom`]).
    pub headroom: &'a HashMap<String, u64>,
    pub now: u64,
}

/// One search asked for: a movie, or a season of a series.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pick {
    pub card_id: String,
    pub title: String,
    pub app: App,
    /// The instance that searches; empty for the default.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instance: String,
    /// The movie id, or the series id.
    pub arr_id: u32,
    pub season: Option<u32>,
    pub p_watch: f64,
    pub bytes: u64,
    /// The largest whole release Prowlarr listed.
    pub expected_bytes: u64,
}

/// Why a cutoff-unmet item was not searched this cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hold {
    /// Pinned, or someone is partway through.
    Protected,
    /// C_reacq above [`MAX_REACQ`].
    HardToReplace,
    /// The plan evicts it, hands it over, or a rule forces it out.
    InPlan,
    /// The upgrade-churn guard flags it.
    Churning,
    /// Prowlarr has not listed a whole release for it (or was not asked yet).
    SizeUnknown,
    /// Its volume's forecast headroom does not cover the larger file.
    NoHeadroom,
    DailyCap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Held {
    pub card_id: String,
    pub title: String,
    pub p_watch: f64,
    pub hold: Hold,
}

#[derive(Debug, Default, PartialEq)]
pub struct Selection {
    pub picks: Vec<Pick>,
    /// Likeliest watched first, at most [`HELD_ROWS`].
    pub held: Vec<Held>,
}

/// This cycle's searches, likeliest watched first within the day's remaining
/// cap and each volume's headroom, and why every other unmet item waits.
pub fn select(inputs: &Inputs<'_>, ledger: &SearchLedger, config: &UpgradeSearchConfig) -> Selection {
    let mut held = Vec::new();
    let mut ready = Vec::new();
    for candidate in inputs.candidates.iter().filter(|candidate| inputs.unmet.contains(&candidate.id)) {
        let Some(item) = inputs.items.get(&candidate.id) else { continue };
        if ledger.blocks(&candidate.id, inputs.now) {
            continue;
        }
        let p_watch = candidate.regret.p_watch;
        let hold = |hold| Held { card_id: candidate.id.clone(), title: candidate.title.clone(), p_watch, hold };
        let expected = inputs.largest_release.get(&candidate.id).copied();
        let refusal = if candidate.protect {
            Some(Hold::Protected)
        } else if candidate.regret.friction > MAX_REACQ {
            Some(Hold::HardToReplace)
        } else if candidate.handed || candidate.force.is_some() || inputs.planned.contains(candidate.id.as_str()) {
            Some(Hold::InPlan)
        } else if inputs.churning.contains(candidate.id.as_str()) {
            Some(Hold::Churning)
        } else if expected.is_none() {
            Some(Hold::SizeUnknown)
        } else {
            None
        };
        match (refusal, expected) {
            (None, Some(expected_bytes)) => ready.push((
                candidate.volume.as_deref(),
                Pick {
                    card_id: candidate.id.clone(),
                    title: candidate.title.clone(),
                    app: item.app,
                    instance: item.instance.to_string(),
                    arr_id: item.id,
                    season: item.season,
                    p_watch,
                    bytes: candidate.size_bytes,
                    expected_bytes,
                },
            )),
            (refusal, _) => held.push(hold(refusal.unwrap_or(Hold::SizeUnknown))),
        }
    }
    // Likeliest watched first; the id keeps equal chances in a stable order.
    ready.sort_by(|(_, a), (_, b)| b.p_watch.total_cmp(&a.p_watch).then_with(|| a.card_id.cmp(&b.card_id)));
    let mut budget = (config.max_per_day as usize).saturating_sub(ledger.searched_since(inputs.now.saturating_sub(DAY_SECS)));
    let mut room = inputs.headroom.clone();
    let mut picks = Vec::new();
    for (volume, pick) in ready {
        let growth = pick.expected_bytes.saturating_sub(pick.bytes);
        let left = volume.and_then(|volume| room.get_mut(volume)).filter(|left| **left >= growth);
        let refusal = match left {
            None => Some(Hold::NoHeadroom),
            Some(_) if budget == 0 => Some(Hold::DailyCap),
            Some(left) => {
                *left -= growth;
                budget -= 1;
                None
            }
        };
        match refusal {
            None => picks.push(pick),
            Some(hold) => held.push(Held { card_id: pick.card_id, title: pick.title, p_watch: pick.p_watch, hold }),
        }
    }
    held.sort_by(|a, b| b.p_watch.total_cmp(&a.p_watch).then_with(|| a.card_id.cmp(&b.card_id)));
    held.truncate(HELD_ROWS);
    Selection { picks, held }
}

/// The search command for a pick: `MoviesSearch {movieIds}` or
/// `SeasonSearch {seriesId, seasonNumber}`.
pub fn command(pick: &Pick) -> serde_json::Value {
    match pick.season {
        None => serde_json::json!({"name": "MoviesSearch", "movieIds": [pick.arr_id]}),
        Some(season) => serde_json::json!({"name": "SeasonSearch", "seriesId": pick.arr_id, "seasonNumber": season}),
    }
}

/// `status.json` `upgrade_search`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UpgradeSearchStatus {
    pub enabled: bool,
    /// This cycle's searches were printed, not sent.
    pub dry_run: bool,
    pub max_per_day: u32,
    /// Searches asked (or tried) in the last 24 hours.
    pub searched_today: usize,
    /// Items below their cutoff, across both apps.
    pub unmet: usize,
    /// This cycle's searches.
    pub picks: Vec<Pick>,
    /// Unmet items not searched this cycle, and why.
    pub held: Vec<Held>,
    /// The newest searches and what became of them.
    pub recent: Vec<Search>,
    /// Searches that brought the item up to its cutoff.
    pub upgraded: usize,
    /// What could not be read or written this cycle, one sentence each.
    pub problems: Vec<String>,
}

#[cfg(test)]
mod tests;
