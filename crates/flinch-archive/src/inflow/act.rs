//! Acting on inflow advice, only where the operator said so.
//!
//! [`super::suggest`] names storage on its way in that nobody is likely to
//! watch; this turns two kinds of it into *arr writes, opt-in and narrow:
//!
//! - **Unmonitor future seasons** of a show the operator approved from the
//!   advice list: Sonarr stops adding new seasons (`monitorNewItems: none`)
//!   and stops searching seasons with no file yet. Seasons on disk keep their
//!   monitoring. Never undone by FLINCH; the operator re-monitors in Sonarr.
//! - **Import lists off** while a disk is over its target: each list the
//!   operator picked has its automatic add switched off, and switched back
//!   on once no disk is over target (or the feature is turned off). A list
//!   that was already off is left alone and never switched on.
//!
//! Every write waits for a governed disk over its target: advice alone moves
//! nothing. Approval is per show and lapses with the advice: a show no
//! longer suggested is not touched, and dropping an approval forgets it.
//!
//! Wire shapes, from the official OpenAPI documents (`develop`):
//! Sonarr `SeriesResource.monitorNewItems` (`all` | `none`) and
//! `seasons[].monitored`, `PUT /api/v3/series/{id}`; `ImportListResource.
//! enableAutomaticAdd`, `PUT /api/v3/importlist/{id}`
//! (https://github.com/Sonarr/Sonarr/blob/develop/src/Sonarr.Api.V3/openapi.json).
//! Radarr `ImportListResource.enableAuto`, `PUT /api/v3/importlist/{id}`
//! (https://github.com/Radarr/Radarr/blob/develop/src/Radarr.Api.V3/openapi.json).

use super::Suggestion;
use crate::capacity::App;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// `settings.json` `inflow_actions`. Off, and nothing approved, by default.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InflowActionsConfig {
    pub enabled: bool,
    /// Shows (`sonarr-<id>`, `sonarr@anime-<id>`) whose future seasons may be unmonitored.
    pub approved: Vec<String>,
    /// Import lists whose automatic add goes off while a disk is over target;
    /// lists of the default instances only.
    pub import_lists: Vec<ImportListRef>,
}

/// One import list in one app's default instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ImportListRef {
    pub app: App,
    pub id: u32,
}

impl std::fmt::Display for ImportListRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} import list {}", self.app.label(), self.id)
    }
}

/// An [`InflowActionsConfig`] outside its bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidInflowActions(pub &'static str);

impl InflowActionsConfig {
    pub fn validate(&self) -> Result<(), InvalidInflowActions> {
        if self.approved.len() > 1_000 || self.approved.iter().any(|subject| series_id(subject).is_none()) {
            return Err(InvalidInflowActions("inflow_actions.approved takes up to 1000 Sonarr shows (sonarr-<id> or sonarr@<name>-<id>)"));
        }
        let mut lists = self.import_lists.clone();
        lists.sort_unstable();
        lists.dedup();
        if self.import_lists.len() > 50 || lists.len() != self.import_lists.len() || lists.iter().any(|list| list.id == 0) {
            return Err(InvalidInflowActions("inflow_actions.import_lists takes up to 50 distinct lists with an id"));
        }
        Ok(())
    }
}

/// The Sonarr series id of a show subject (`sonarr-<id>`, `sonarr@anime-<id>`;
/// [`crate::ids::ArrRef`]); its instance is the subject's.
pub fn series_id(subject: &str) -> Option<u32> {
    crate::ids::ArrRef::parse(subject).filter(|show| show.app == App::Sonarr && show.season.is_none() && show.id > 0).map(|show| show.id)
}

/// `state/inflow-actions.json`: what FLINCH changed, so it changes each
/// thing once and restores only what it switched off itself.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InflowLedger {
    /// Shows whose future seasons FLINCH unmonitored → when (unix seconds).
    pub unmonitored: BTreeMap<String, u64>,
    /// Import lists FLINCH switched off, with when.
    pub lists_off: Vec<(ImportListRef, u64)>,
}

impl InflowLedger {
    pub fn is_empty(&self) -> bool {
        self.unmonitored.is_empty() && self.lists_off.is_empty()
    }

    /// Forget shows no longer approved, so approving one again acts again.
    pub fn prune(&mut self, config: &InflowActionsConfig) {
        self.unmonitored.retain(|subject, _| config.approved.contains(subject));
    }

    pub fn is_off(&self, list: ImportListRef) -> bool {
        self.lists_off.iter().any(|(off, _)| *off == list)
    }
}

/// One write this cycle asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Unmonitor { series_id: u32, subject: String, title: String },
    ListOff(ImportListRef),
    ListOn(ImportListRef),
}

/// This cycle's writes. `over_target`: a governed disk's forecast is over its
/// target now.
pub fn plan(config: &InflowActionsConfig, suggestions: &[Suggestion], over_target: bool, ledger: &InflowLedger) -> Vec<Action> {
    let keep_off = config.enabled && over_target;
    // Restore what FLINCH switched off once it is no longer wanted off.
    let mut actions: Vec<Action> = ledger
        .lists_off
        .iter()
        .map(|(list, _)| *list)
        .filter(|list| !keep_off || !config.import_lists.contains(list))
        .map(Action::ListOn)
        .collect();
    if !keep_off {
        return actions;
    }
    actions.extend(config.import_lists.iter().filter(|list| !ledger.is_off(**list)).map(|list| Action::ListOff(*list)));
    for suggestion in suggestions {
        let Some(series_id) = series_id(&suggestion.subject) else { continue };
        if config.approved.contains(&suggestion.subject) && !ledger.unmonitored.contains_key(&suggestion.subject) {
            actions.push(Action::Unmonitor { series_id, subject: suggestion.subject.clone(), title: suggestion.title.clone() });
        }
    }
    actions
}

/// Unmonitor a Sonarr series resource's future: no new seasons, and no
/// search for a regular season without a file. Whether anything changed.
pub fn unmonitor_future(series: &mut Value) -> bool {
    let mut changed = false;
    if series.get("monitorNewItems").and_then(Value::as_str) != Some("none") {
        series["monitorNewItems"] = Value::from("none");
        changed = true;
    }
    for season in series.get_mut("seasons").and_then(Value::as_array_mut).into_iter().flatten() {
        if is_future(season) && season.get("monitored").and_then(Value::as_bool) != Some(false) {
            season["monitored"] = Value::from(false);
            changed = true;
        }
    }
    changed
}

/// Whether [`unmonitor_future`] holds for a resource read back.
pub fn future_unmonitored(series: &Value) -> bool {
    let seasons = series.get("seasons").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    series.get("monitorNewItems").and_then(Value::as_str) == Some("none")
        && seasons.iter().filter(|season| is_future(season)).all(|season| season.get("monitored").and_then(Value::as_bool) == Some(false))
}

/// A regular season Sonarr holds no file of. Missing statistics read as
/// "has files", so a season is never unmonitored on a guess.
fn is_future(season: &Value) -> bool {
    let number = season.get("seasonNumber").and_then(Value::as_u64).unwrap_or(0);
    let files = season.pointer("/statistics/episodeFileCount").and_then(Value::as_u64);
    number > 0 && files == Some(0)
}

/// The field that switches a list's automatic add, per app.
fn auto_add_field(app: App) -> &'static str {
    match app {
        App::Radarr => "enableAuto",
        App::Sonarr => "enableAutomaticAdd",
    }
}

/// A list resource's automatic add; `None` when the field is missing.
pub fn auto_add(list: &Value, app: App) -> Option<bool> {
    list.get(auto_add_field(app)).and_then(Value::as_bool)
}

/// Set a list resource's automatic add. `None` when the field is missing
/// (an app version FLINCH does not know), else whether it changed.
pub fn set_auto_add(list: &mut Value, app: App, on: bool) -> Option<bool> {
    let current = auto_add(list, app)?;
    list[auto_add_field(app)] = Value::from(on);
    Some(current != on)
}

/// One import list as its app lists it, for the approval page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownList {
    pub app: App,
    pub id: u32,
    pub name: String,
    /// Its automatic add, as read this cycle.
    pub auto_add: Option<bool>,
}

/// Every list in an `/api/v3/importlist` answer.
pub fn known_lists(app: App, answer: &Value) -> Vec<KnownList> {
    answer
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|list| {
            let id = u32::try_from(list.get("id")?.as_u64()?).ok()?;
            let name = list.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
            Some(KnownList { app, id, name, auto_add: auto_add(list, app) })
        })
        .collect()
}

/// `status.json` `inflow_actions`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InflowActionsStatus {
    pub enabled: bool,
    pub dry_run: bool,
    /// A governed disk was over its target this cycle.
    pub over_target: bool,
    /// Every import list the apps listed, for picking.
    pub import_lists: Vec<KnownList>,
    /// This cycle's writes (or, in a dry run, what they would be), in words.
    pub acted: Vec<String>,
    /// Shows FLINCH unmonitored, and the lists it holds off.
    pub unmonitored: Vec<String>,
    pub lists_off: Vec<ImportListRef>,
    pub problems: Vec<String>,
}

#[cfg(test)]
mod tests;
