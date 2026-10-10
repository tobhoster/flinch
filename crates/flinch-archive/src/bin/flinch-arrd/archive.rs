//! The daemon's side of the archive tier ([`flinch_archive::archive`]): find
//! this cycle's destinations for the planner, then send the plan's moves once
//! each has stayed planned for the grace runs — or print them in a dry run —
//! reading every item back at its new root before its freed bytes are
//! credited through the eviction ledger. Moves are FLINCH's own writes, so
//! they run under either executor.
//!
//! Wire shapes, from the sources (`develop`): `PUT /api/v3/movie/editor`
//! takes `MovieEditorResource {movieIds, rootFolderPath, moveFiles}` and
//! queues a `BulkMoveMovieCommand` when `moveFiles` is set
//! (https://github.com/Radarr/Radarr/blob/develop/src/Radarr.Api.V3/Movies/MovieEditorController.cs);
//! `PUT /api/v3/series/editor` takes `SeriesEditorResource {seriesIds,
//! rootFolderPath, moveFiles}` and queues a `BulkMoveSeriesCommand`
//! (https://github.com/Sonarr/Sonarr/blob/develop/src/Sonarr.Api.V3/Series/SeriesEditorController.cs).
//! Both store the new path at once and copy in the background, so
//! `GET /api/v3/movie/{id}` / `GET /api/v3/series/{id}` read the new `path`
//! back immediately; the source disk frees as the copy ends, which the
//! ledger's credit covers until the drop shows.

use super::arr_write::ArrWriter;
use super::{state_dir, Args};
use anyhow::{Context, Result};
use flinch_archive::archive::{self, ArchiveStatus, ArchivedItem};
use flinch_archive::capacity::{App, EvictionLedger, HandedOver};
use flinch_archive::daemon::{self, Grace, RuntimeSettings};
use flinch_archive::govern::Governance;
use flinch_archive::plan::{ArchiveDestination, PlanMove};
use flinch_archive::ReconcileOutput;
use reqwest::Method;
use serde_json::json;

/// Where each planned move's grace streak is kept.
fn streaks_path() -> std::path::PathBuf {
    state_dir().join("archive-streaks.json")
}

/// This cycle's destinations for the planner, and the roots that cannot be
/// one; empty while the tier is off. The default instances archive to the
/// app-wide roots, each extra one to its own `archive_root`.
pub(super) fn destinations(args: &Args, settings: &RuntimeSettings, governance: &Governance) -> (Vec<ArchiveDestination>, Vec<String>) {
    let extra = args
        .arrs
        .iter()
        .filter(|arr| settings.archive.enabled && !arr.is_default() && !arr.archive_root.is_empty())
        .map(|arr| (arr.app, arr.name.as_str(), arr.archive_root.as_str()));
    let defaults = settings.archive.roots().map(|(app, instance, root)| (app, instance as &str, root));
    let roots = defaults.chain(extra);
    let found = archive::destinations(roots, &governance.library, &governance.forecasts, &settings.capacity);
    for problem in &found.1 {
        eprintln!("[flinch-arrd] archive: {problem}; nothing moves there");
    }
    found
}

pub(super) struct Cycle<'a> {
    pub(super) http: &'a reqwest::Client,
    pub(super) args: &'a Args,
    pub(super) settings: &'a RuntimeSettings,
    pub(super) report: &'a ReconcileOutput,
    pub(super) destinations: (Vec<ArchiveDestination>, Vec<String>),
    pub(super) dry_run: bool,
    pub(super) interval_s: u64,
    pub(super) now: u64,
}

/// Send the moves past their grace streak, at most the per-run cap; `None`
/// while the tier is off.
pub(super) async fn run(cycle: Cycle<'_>, ledger: &mut EvictionLedger) -> Option<ArchiveStatus> {
    let Cycle { http, args, settings, report, destinations: (destinations, unresolved), dry_run, interval_s, now } = cycle;
    if !settings.archive.enabled {
        return None;
    }
    let moves = &report.plan.moves;
    let ids: Vec<String> = moves.iter().map(|planned| planned.id.clone()).collect();
    let mut streaks = daemon::read_candidate_state(&streaks_path());
    let ripe = daemon::advance_streaks(&mut streaks, &ids, Grace { runs: settings.grace_runs, interval_s }, now);
    let mut status = ArchiveStatus {
        dry_run,
        destinations,
        unresolved,
        planned: moves.len(),
        planned_bytes: report.plan.moved_bytes(),
        ..ArchiveStatus::default()
    };
    let due: Vec<&PlanMove> = moves.iter().filter(|planned| ripe.contains(&planned.id)).take(settings.archive.max_moves_per_run).collect();
    for planned in &due {
        match send(http, args, planned, dry_run).await {
            Ok(path) => {
                if path.is_some() {
                    let handed = HandedOver {
                        id: &planned.id,
                        title: &planned.title,
                        app: planned.app,
                        volume: &planned.volume,
                        bytes: planned.size_bytes,
                    };
                    ledger.record_move(handed, now);
                }
                println!("[flinch-arrd] archive: {} {} → {}", if dry_run { "would move" } else { "moved" }, planned.title, planned.root);
                status.moved.push(ArchivedItem {
                    id: planned.id.clone(),
                    title: planned.title.clone(),
                    bytes: planned.size_bytes,
                    root: planned.root.clone(),
                    path,
                });
                // Sent: a later plan that names it again starts a new streak.
                streaks.streaks.remove(&planned.id);
                streaks.streak_started.remove(&planned.id);
            }
            Err(error) => {
                eprintln!("[flinch-arrd] archive: {} not moved: {error:#}", planned.title);
                status.failed.push(format!("{}: {error:#}", planned.title));
            }
        }
    }
    status.waiting = moves.len().saturating_sub(due.len());
    if let Err(error) = daemon::write_candidate_state(&streaks_path(), &streaks) {
        eprintln!("[flinch-arrd] archive-streaks.json write failed: {error}");
    }
    Some(status)
}

/// Move one item and read it back: the new path under the archive root, or
/// `None` in a dry run, which only prints the write.
async fn send(http: &reqwest::Client, args: &Args, planned: &PlanMove, dry_run: bool) -> Result<Option<String>> {
    let writer = args
        .arr(planned.app, &planned.instance)
        .and_then(|arr| ArrWriter::new(http, arr, dry_run))
        .with_context(|| format!("{} is not configured", flinch_archive::ids::instance_key(planned.app, &planned.instance)))?;
    let (kind, ids) = match planned.app {
        App::Radarr => ("movie", "movieIds"),
        App::Sonarr => ("series", "seriesIds"),
    };
    let body = json!({ (ids): [planned.arr_id], "rootFolderPath": planned.root, "moveFiles": true });
    if writer.send(Method::PUT, &format!("/api/v3/{kind}/editor"), &body).await?.is_none() {
        return Ok(None);
    }
    let read = writer.get(&format!("/api/v3/{kind}/{}", planned.arr_id)).await.context("read back")?;
    let path = read.get("path").and_then(|path| path.as_str()).unwrap_or_default();
    if !archive::under_root(path, &planned.root) {
        anyhow::bail!("{} reads back {path:?}, not under {}", writer.name(), planned.root);
    }
    Ok(Some(path.to_string()))
}
