//! The daemon's side of quality actions ([`flinch_archive::quality::act`]):
//! settle earlier moves against today's sizes, find each app's compact
//! profile, select this cycle's moves and send them — or print them in a dry
//! run — verifying each profile change by reading the item back before its
//! search is asked for.
//!
//! Wire shapes, from the sources (`develop`): `PUT /api/v3/movie/editor`
//! takes `MovieEditorResource {movieIds, qualityProfileId, ...}`
//! (https://github.com/Radarr/Radarr/blob/develop/src/Radarr.Api.V3/Movies/MovieEditorResource.cs);
//! `PUT /api/v3/series/editor` takes `SeriesEditorResource {seriesIds,
//! qualityProfileId, ...}`
//! (https://github.com/Sonarr/Sonarr/blob/v5-develop/src/Sonarr.Api.V3/Series/SeriesEditorResource.cs).
//! `POST /api/v3/command` takes the command's `name` and its own fields:
//! `MoviesSearch {movieIds}`
//! (https://github.com/Radarr/Radarr/blob/develop/src/NzbDrone.Core/IndexerSearch/MoviesSearchCommand.cs),
//! `SeasonSearch {seriesId, seasonNumber}`
//! (https://github.com/Sonarr/Sonarr/blob/v5-develop/src/NzbDrone.Core/IndexerSearch/SeasonSearchCommand.cs).
//! `GET /api/v3/qualityprofile` lists `{id, name, ...}`.

use super::arr_write::ArrWriter;
use super::Args;
use anyhow::{Context, Result};
use flinch_archive::arr::{ArrMovie, ArrSeries};
use flinch_archive::capacity::App;
use flinch_archive::daemon::RuntimeSettings;
use flinch_archive::plan::MediaCandidate;
use flinch_archive::quality::act::{self, ActionLedger, Compact, Inputs, Move, QualityActionsConfig, QualityActionsStatus};
use flinch_archive::signals::Signals;
use flinch_archive::ReconcileOutput;
use reqwest::Method;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// What the cycle hands over; upgrade searches read the same.
#[derive(Clone, Copy)]
pub(super) struct Library<'a> {
    /// One per card on disk, with its size and advice.
    pub(super) candidates: &'a [MediaCandidate],
    pub(super) arr: (&'a [ArrMovie], &'a [ArrSeries]),
    /// This cycle's plan: what it evicts.
    pub(super) report: &'a ReconcileOutput,
    /// The smallest whole release Prowlarr found per card.
    pub(super) signals: &'a Signals,
}

/// One cycle of quality actions. `None` while they are off and none was ever made.
pub(super) async fn run(
    http: &reqwest::Client,
    args: &Args,
    settings: &RuntimeSettings,
    library: Library<'_>,
    dry_run: bool,
    now: u64,
) -> Option<QualityActionsStatus> {
    let config = &settings.quality_actions;
    let path = super::state_dir().join("quality-actions.json");
    let mut ledger: ActionLedger = super::read_state(&path);
    if !config.enabled && ledger.actions.is_empty() {
        return None;
    }
    let before = ledger.clone();
    let sizes: HashMap<&str, u64> = library.candidates.iter().map(|candidate| (candidate.id.as_str(), candidate.size_bytes)).collect();
    ledger.settle(&sizes, now);
    ledger.prune(now);
    let mut status = QualityActionsStatus { enabled: config.enabled, dry_run, max_per_day: config.max_per_day, ..Default::default() };
    if config.enabled {
        let items = act::arr_items(library.arr.0, library.arr.1);
        let compact = compact_profiles(http, args, config, &mut status.problems).await;
        let evicting: HashSet<&str> = library.report.deleted_ids.iter().map(String::as_str).collect();
        let inputs = Inputs {
            candidates: library.candidates,
            items: &items,
            evicting: &evicting,
            smallest_release: &library.signals.smallest_release,
            grace_days: settings.planner.grace_period_days,
            compact,
            now,
        };
        let selection = act::select(&inputs, &ledger, config);
        for movement in &selection.moves {
            let Some(writer) = args.arr(movement.app, &movement.instance).and_then(|arr| ArrWriter::new(http, arr, dry_run)) else {
                let instance = flinch_archive::ids::instance_key(movement.app, &movement.instance);
                status.problems.push(format!("{instance} not configured: its moves wait"));
                continue;
            };
            match apply(&writer, movement).await {
                Ok(None) => {}
                Ok(Some(searched)) => {
                    let titles: Vec<&str> = movement.cards.iter().map(|card| card.title.as_str()).collect();
                    println!("[flinch-arrd] quality: moved {} to the compact profile (search asked: {searched})", titles.join(", "));
                    ledger.record(movement, now, Ok(searched));
                }
                Err(error) => {
                    eprintln!("[flinch-arrd] quality: {} move of item {} failed: {error:#}", writer.name(), movement.id);
                    status.problems.push(format!("{} refused a profile move ({}): tried again in a day", writer.name(), short(&error)));
                    ledger.record(movement, now, Err(short(&error)));
                }
            }
        }
        status.moves = selection.moves;
        status.held = selection.held;
    }
    if ledger != before {
        super::write_state(&path, &ledger);
    }
    status.acted_today = ledger.acted_since(now.saturating_sub(act::DAY_SECS));
    status.recent = ledger.recent();
    status.reclaimed_bytes = ledger.reclaimed_bytes();
    Some(status)
}

/// The first line of an error, for the ledger and the page.
fn short(error: &anyhow::Error) -> String {
    error.to_string().chars().take(160).collect()
}

/// Each instance's compact profile: the one the quality sync manages there,
/// else the one the settings name (the app-wide name for a default instance,
/// its own `compact_profile` for an extra one). A failed lookup leaves that
/// instance without one.
async fn compact_profiles(http: &reqwest::Client, args: &Args, config: &QualityActionsConfig, problems: &mut Vec<String>) -> Compact {
    let state = super::state_dir();
    let mut compact = Compact::default();
    for arr in &args.arrs {
        let named = match arr.is_default() {
            true => config.profile_name(arr.app),
            false => Some(arr.compact_profile.as_str()).filter(|name| !name.is_empty()),
        };
        let managed = flinch_archive::trash::compact_profile_id(&state, arr.app, &arr.name);
        let id = match (managed, named, ArrWriter::new(http, arr, true)) {
            (Some(id), ..) => Some(id),
            (None, Some(name), Some(reader)) => match profile_named(&reader, name).await {
                Ok(Some(id)) => Some(id),
                Ok(None) => {
                    problems.push(format!("{} has no quality profile named \"{name}\"", reader.name()));
                    None
                }
                Err(error) => {
                    eprintln!("[flinch-arrd] quality: {} profiles unreadable: {error:#}", reader.name());
                    problems.push(format!("{} quality profiles unreadable", reader.name()));
                    None
                }
            },
            _ => None,
        };
        if let Some(id) = id {
            compact.set(arr.app, &arr.name, id);
        }
    }
    compact
}

/// The id of the profile named `name` (case-insensitive).
async fn profile_named(reader: &ArrWriter<'_>, name: &str) -> Result<Option<u32>> {
    let profiles = reader.get("/api/v3/qualityprofile").await?;
    let profiles = profiles.as_array().context("profiles answer is not an array")?;
    let named = profiles.iter().find(|profile| profile.get("name").and_then(Value::as_str).is_some_and(|it| it.eq_ignore_ascii_case(name)));
    Ok(named.and_then(|profile| profile.get("id")?.as_u64()).and_then(|id| u32::try_from(id).ok()))
}

/// Move the item's profile, read it back, then ask for the searches.
/// `Ok(None)` in a dry run; `Ok(Some(searched))` once the profile reads back moved.
async fn apply(writer: &ArrWriter<'_>, movement: &Move) -> Result<Option<bool>> {
    let (editor, item, body) = match writer.app() {
        App::Radarr => (
            "/api/v3/movie/editor",
            format!("/api/v3/movie/{}", movement.id),
            json!({"movieIds": [movement.id], "qualityProfileId": movement.to_profile}),
        ),
        App::Sonarr => (
            "/api/v3/series/editor",
            format!("/api/v3/series/{}", movement.id),
            json!({"seriesIds": [movement.id], "qualityProfileId": movement.to_profile}),
        ),
    };
    if writer.send(Method::PUT, editor, &body).await?.is_none() {
        for command in searches(movement) {
            writer.send(Method::POST, "/api/v3/command", &command).await?;
        }
        return Ok(None);
    }
    let read = writer.get(&item).await.context("reading the profile back")?;
    let profile = read.get("qualityProfileId").and_then(Value::as_u64);
    anyhow::ensure!(profile == Some(u64::from(movement.to_profile)), "the profile did not change (it reads {profile:?})");
    let mut searched = true;
    for command in searches(movement) {
        if let Err(error) = writer.send(Method::POST, "/api/v3/command", &command).await {
            eprintln!("[flinch-arrd] quality: {} search for item {} not started: {error:#}", writer.name(), movement.id);
            searched = false;
        }
    }
    Ok(Some(searched))
}

/// The search commands for a move: the movie, or each season moved.
fn searches(movement: &Move) -> Vec<Value> {
    match movement.app {
        App::Radarr => vec![json!({"name": "MoviesSearch", "movieIds": [movement.id]})],
        App::Sonarr => movement
            .cards
            .iter()
            .filter_map(|card| card.season)
            .map(|season| json!({"name": "SeasonSearch", "seriesId": movement.id, "seasonNumber": season}))
            .collect(),
    }
}
