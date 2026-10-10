//! The upgrade-churn guard ([`flinch_archive::quality::churn`]): read each
//! instance's grabs and imports of the last 30 days (every six hours, cached in
//! `arr-grabs.json`), flag the items grabbed more than the limit, and take
//! the step the settings choose — unmonitor the item, or turn upgrades off on
//! its profile — once per item, verified by reading back. Flag only by default.
//!
//! Wire shapes, from the sources: `PUT /api/v3/movie/editor {movieIds,
//! monitored}` (Radarr `MovieEditorResource.cs`); Sonarr `GET
//! /api/v3/episode?seriesId=&seasonNumber=` and `PUT /api/v3/episode/monitor
//! {episodeIds, monitored}`
//! (https://github.com/Sonarr/Sonarr/blob/v5-develop/src/Sonarr.Api.V3/Episodes/EpisodesMonitoredResource.cs);
//! `GET`/`PUT /api/v3/qualityprofile/{id}` with `upgradeAllowed`
//! (https://github.com/Radarr/Radarr/blob/develop/src/Radarr.Api.V3/Profiles/Quality/QualityProfileResource.cs).

use super::arr_write::ArrWriter;
use super::Args;
use anyhow::{Context, Result};
use flinch_archive::arr::{ArrMovie, ArrSeries};
use flinch_archive::quality::act::{self, ArrItem};
use flinch_archive::quality::churn::{
    self, Applied, ChurnItem, ChurnStatus, Event, EventCache, EventKind, EventRead, GuardAction, GuardLedger, UpgradeGuardConfig,
};
use reqwest::Method;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Flagged items the status carries at most, most grabbed first.
const STATUS_ROWS: usize = 100;

/// What a guard step did.
enum Step {
    Applied(Applied),
    /// Printed in a dry run, not sent.
    DryRun,
    /// Not taken, and why.
    Skipped(&'static str),
}

/// One cycle of the guard. `None` while it is off.
pub(super) async fn run(
    http: &reqwest::Client,
    args: &Args,
    config: &UpgradeGuardConfig,
    (movies, series): (&[ArrMovie], &[ArrSeries]),
    dry_run: bool,
    now: u64,
) -> Option<ChurnStatus> {
    if !config.enabled {
        return None;
    }
    let mut status = ChurnStatus { limit: config.max_grabs_per_item_30d, action: config.action, ..Default::default() };
    let cache = events(http, args, now, &mut status.problems).await;
    let churning = churn::detect(cache.events(), config.max_grabs_per_item_30d, now);
    let items = act::arr_items(movies, series);
    let titles = titles(movies, series);
    let path = super::state_dir().join("upgrade-guard.json");
    let mut ledger: GuardLedger = super::read_state(&path);
    let recorded = ledger.applied.len();
    ledger.prune(now);
    let mut changed = ledger.applied.len() != recorded;
    for churn in churning.into_iter().take(STATUS_ROWS) {
        let mut note = None;
        if config.action != GuardAction::Flag && !ledger.applied.contains_key(&churn.card_id) {
            match step(http, args, config.action, (&churn.card_id, items.get(&churn.card_id)), &ledger, dry_run, now).await {
                Ok(Step::Applied(applied)) => {
                    println!("[flinch-arrd] upgrade guard: {:?} on {} ({} grabs in 30 days)", applied.action, churn.card_id, churn.grabs);
                    ledger.applied.insert(churn.card_id.clone(), applied);
                    changed = true;
                }
                Ok(Step::DryRun) => note = Some("dry run: printed, not sent".to_owned()),
                Ok(Step::Skipped(why)) => note = Some(why.to_owned()),
                Err(error) => {
                    eprintln!("[flinch-arrd] upgrade guard: {:?} on {} failed: {error:#}", config.action, churn.card_id);
                    note = Some(format!("not applied: {}", error.to_string().chars().take(160).collect::<String>()));
                }
            }
        }
        let title = titles.get(&churn.card_id).cloned().unwrap_or_else(|| churn.card_id.clone());
        status.items.push(ChurnItem { applied: ledger.applied.get(&churn.card_id).cloned(), churn, title, note });
    }
    if changed {
        super::write_state(&path, &ledger);
    }
    if !status.items.is_empty() {
        println!("[flinch-arrd] upgrade guard: {} item(s) grabbed more than {} times in 30 days", status.items.len(), status.limit);
    }
    Some(status)
}

/// Every instance's grabs and imports, each read again once its cached read
/// is six hours old. A failed read keeps serving the last one.
async fn events(http: &reqwest::Client, args: &Args, now: u64, problems: &mut Vec<String>) -> EventCache {
    let path = super::state_dir().join("arr-grabs.json");
    let mut cache: EventCache = super::read_state(&path);
    let mut read_any = false;
    for arr in &args.arrs {
        let Some(reader) = ArrWriter::new(http, arr, true).filter(|_| !cache.is_fresh(arr.app, &arr.name, now)) else { continue };
        match read_events(&reader, now).await {
            Ok(events) => {
                cache.store(arr.app, &arr.name, EventRead { read_at: now, events });
                read_any = true;
            }
            Err(error) => {
                eprintln!("[flinch-arrd] upgrade guard: {} history unreadable: {error:#}", reader.name());
                let serving = if cache.read(arr.app, &arr.name).is_some() { "the last read serves" } else { "its churn is not judged" };
                problems.push(format!("{} grab history unreadable: {serving}", reader.name()));
            }
        }
    }
    // An instance no longer configured has no churn to judge.
    let before = cache.extra.len();
    cache.extra.retain(|key, _| args.arrs.iter().any(|arr| arr.key() == *key));
    if read_any || cache.extra.len() != before {
        super::write_state(&path, &cache);
    }
    cache
}

async fn read_events(reader: &ArrWriter<'_>, now: u64) -> Result<Vec<Event>> {
    let mut events = Vec::new();
    for kind in [EventKind::Grab, EventKind::Import] {
        let rows = reader.get(&churn::events_path(reader.app(), kind, now)).await?;
        let rows: Vec<Value> = serde_json::from_value(rows).context("history answer is not an array")?;
        events.extend(churn::parse_events(reader.app(), reader.instance(), kind, rows));
    }
    Ok(events)
}

/// Card id → the title the page shows: the movie, or "Show S2".
fn titles(movies: &[ArrMovie], series: &[ArrSeries]) -> HashMap<String, String> {
    let movies = movies.iter().map(|movie| (movie.card_id(), movie.title.clone()));
    let seasons = series.iter().flat_map(|show| {
        show.seasons
            .iter()
            .map(move |season| (show.season_card_id(season.season_number), format!("{} S{}", show.title, season.season_number)))
    });
    movies.chain(seasons).collect()
}

async fn step(
    http: &reqwest::Client,
    args: &Args,
    action: GuardAction,
    (card_id, item): (&str, Option<&ArrItem<'_>>),
    ledger: &GuardLedger,
    dry_run: bool,
    now: u64,
) -> Result<Step> {
    let Some(item) = item else { return Ok(Step::Skipped("no longer in the library")) };
    let Some(writer) = args.arr(item.app, item.instance).and_then(|arr| ArrWriter::new(http, arr, dry_run)) else {
        return Ok(Step::Skipped("its instance is not configured"));
    };
    match action {
        GuardAction::Flag => Ok(Step::Skipped("flag only")),
        GuardAction::Unmonitor => unmonitor(&writer, item, now).await,
        GuardAction::UpgradesOff => upgrades_off(&writer, (card_id, item), ledger, now).await,
    }
}

/// Unmonitor the movie, or every episode of the season; read back.
async fn unmonitor(writer: &ArrWriter<'_>, item: &ArrItem<'_>, now: u64) -> Result<Step> {
    match item.season {
        None => {
            let body = json!({"movieIds": [item.id], "monitored": false});
            if writer.send(Method::PUT, "/api/v3/movie/editor", &body).await?.is_none() {
                return Ok(Step::DryRun);
            }
            let read = writer.get(&format!("/api/v3/movie/{}", item.id)).await.context("reading the movie back")?;
            anyhow::ensure!(read.get("monitored").and_then(Value::as_bool) == Some(false), "the movie still reads monitored");
        }
        Some(season) => {
            let path = format!("/api/v3/episode?seriesId={}&seasonNumber={season}", item.id);
            let ids = |episodes: &Value| -> Vec<(u64, Option<bool>)> {
                let rows = episodes.as_array().map(Vec::as_slice).unwrap_or_default();
                rows.iter().filter_map(|row| Some((row.get("id")?.as_u64()?, row.get("monitored").and_then(Value::as_bool)))).collect()
            };
            let episodes: Vec<u64> = ids(&writer.get(&path).await?).into_iter().map(|(id, _)| id).collect();
            anyhow::ensure!(!episodes.is_empty(), "the season lists no episodes");
            let body = json!({"episodeIds": episodes, "monitored": false});
            if writer.send(Method::PUT, "/api/v3/episode/monitor", &body).await?.is_none() {
                return Ok(Step::DryRun);
            }
            let read = ids(&writer.get(&path).await.context("reading the episodes back")?);
            anyhow::ensure!(read.iter().all(|(_, monitored)| *monitored == Some(false)), "an episode still reads monitored");
        }
    }
    Ok(Step::Applied(Applied { action: GuardAction::Unmonitor, at: now, profile: None }))
}

/// Turn `upgradeAllowed` off on the item's profile; read back. A profile the
/// quality sync manages is left alone: the next sync would turn it back on.
async fn upgrades_off(writer: &ArrWriter<'_>, (card_id, item): (&str, &ArrItem<'_>), ledger: &GuardLedger, now: u64) -> Result<Step> {
    let Some(profile) = item.profile else { return Ok(Step::Skipped("its quality profile is unknown")) };
    let applied = Step::Applied(Applied { action: GuardAction::UpgradesOff, at: now, profile: Some(profile) });
    if ledger.profile_off(card_id, profile) {
        return Ok(applied);
    }
    if flinch_archive::trash::managed_profile_ids(&super::state_dir(), item.app, item.instance).contains(&profile) {
        return Ok(Step::Skipped("its profile is managed by the quality sync, which would turn upgrades back on"));
    }
    let path = format!("/api/v3/qualityprofile/{profile}");
    let mut body = writer.get(&path).await?;
    let fields = body.as_object_mut().context("profile answer is not an object")?;
    if fields.get("upgradeAllowed") == Some(&Value::Bool(false)) {
        return Ok(Step::Skipped("upgrades are already off on its profile"));
    }
    fields.insert("upgradeAllowed".to_owned(), Value::Bool(false));
    if writer.send(Method::PUT, &path, &body).await?.is_none() {
        return Ok(Step::DryRun);
    }
    let read = writer.get(&path).await.context("reading the profile back")?;
    anyhow::ensure!(read.get("upgradeAllowed") == Some(&Value::Bool(false)), "upgrades still read allowed");
    Ok(applied)
}
