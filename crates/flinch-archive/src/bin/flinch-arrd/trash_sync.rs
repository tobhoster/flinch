//! The daemon's TRaSH job ([`flinch_archive::trash`]): preview each *arr
//! instance against its source (the pinned TRaSH-Guides commit, or a pinned
//! Profilarr Compliant Database) on the operator's schedule, and apply what
//! the Quality profiles page selected, or everything with `apply` on. The
//! default instances sync their app's config; an extra instance only when
//! `trash.instances.extra` names it. It runs before any other write of a cycle
//! and never fails one: profiles are not evidence, and an outage here must
//! not hold back the plan. The library, when the cycle read it, sizes each
//! change (items per profile × cached release sizes) and tells which profiles
//! nothing uses.

use super::{state_dir, Args};
use flinch_archive::arr::{ArrMovie, ArrSeries};
use flinch_archive::capacity::VolumeForecast;
use flinch_archive::daemon::RuntimeSettings;
use flinch_archive::signals::release::ReleaseCache;
use flinch_archive::trash::{
    self, guide, pcd, AppApply, AppSync, ApplyRecord, ApplyRequest, ArrClient, DiskOutlook, Selection, Source, TrashState, Usage,
};
use std::collections::BTreeSet;
use std::path::Path;

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The page's selection, consumed once: a request is answered by one run.
fn take_request(dir: &Path) -> Option<ApplyRequest> {
    let path = dir.join(trash::APPLY_REQUEST_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            eprintln!("[flinch-arrd] trash: {} unreadable, nothing applied: {error}", path.display());
            return None;
        }
    };
    if let Err(error) = std::fs::remove_file(&path) {
        // Left in place it would apply again next cycle; refuse it instead.
        eprintln!("[flinch-arrd] trash: {} could not be consumed, nothing applied: {error}", path.display());
        return None;
    }
    serde_json::from_slice(&bytes).map_err(|error| eprintln!("[flinch-arrd] trash: apply request malformed, nothing applied: {error}")).ok()
}

async fn load_guide(http: &reqwest::Client, dir: &Path, commit: &str) -> Result<guide::Guide, String> {
    if let Some(cached) = guide::read_cache(dir, commit) {
        return Ok(cached);
    }
    println!("[flinch-arrd] trash: reading TRaSH-Guides at {commit}");
    let fetched = guide::fetch(http, &guide::GuideSource::github(commit)).await.map_err(|error| error.to_string())?;
    if let Err(error) = guide::write_cache(dir, &fetched) {
        eprintln!("[flinch-arrd] trash: guide cache write failed, it is fetched again next run: {error}");
    }
    Ok(fetched)
}

async fn load_pcd(http: &reqwest::Client, dir: &Path, config: &trash::config::PcdConfig) -> Result<guide::Guide, String> {
    if let Some(cached) = pcd::read_cache(dir, config) {
        return Ok(cached);
    }
    println!(
        "[flinch-arrd] trash: reading the PCD {} at {} (schema {} at {})",
        config.repository, config.commit, config.schema_repository, config.schema_commit
    );
    let (database, schema) = pcd::sources(config);
    let fetched = pcd::fetch(http, &database, &schema, pcd::key(config)).await.map_err(|error| error.to_string())?;
    println!("[flinch-arrd] trash: {} declares license {}", config.repository, fetched.license.as_deref().unwrap_or("?"));
    if let Err(error) = pcd::write_cache(dir, &fetched) {
        eprintln!("[flinch-arrd] trash: PCD cache write failed, it is fetched again next run: {error}");
    }
    Ok(fetched)
}

/// The last plan's forecast over every volume; `None` before the first plan.
fn outlook(dir: &Path) -> Option<DiskOutlook> {
    #[derive(serde::Deserialize, Default)]
    struct Plan {
        #[serde(default)]
        forecast: Vec<VolumeForecast>,
    }
    let plan: Plan = super::read_state(&dir.join("eviction-plan.json"));
    let capacity_bytes: u64 = plan.forecast.iter().map(|v| v.forecast.max_capacity_bytes).sum();
    (capacity_bytes > 0)
        .then(|| DiskOutlook { projected_used_bytes: plan.forecast.iter().map(|v| v.forecast.projected_used_bytes).sum(), capacity_bytes })
}

fn report(label: &str, state: &trash::AppState, outcome: Option<&trash::AppOutcome>) {
    match &state.error {
        Some(error) => eprintln!("[flinch-arrd] trash: {label}: {error}"),
        None => println!("[flinch-arrd] trash: {label}: {} change(s) pending, {} in sync", state.changes.len(), state.in_sync),
    }
    for problem in &state.problems {
        eprintln!("[flinch-arrd] trash: {label}: {problem}");
    }
    let Some(outcome) = outcome else { return };
    for line in &outcome.printed {
        println!("[dry-run] trash {label}: {line}");
    }
    for failed in &outcome.failed {
        eprintln!("[flinch-arrd] trash: {label}: {} failed: {}", failed.id, failed.error);
    }
    for id in &outcome.unverified {
        eprintln!("[flinch-arrd] trash: {label}: {id} was accepted but did not read back as applied");
    }
    if !outcome.applied.is_empty() {
        println!("[flinch-arrd] trash: {label}: applied {}", outcome.applied.join(", "));
    }
}

fn write(state: &TrashState, dir: &Path) {
    if let Err(error) = state.write(dir) {
        eprintln!("[flinch-arrd] trash: {} write failed: {error}", trash::STATE_FILE);
    }
}

/// One run of the job, when due: on schedule, after a settings change, or
/// when the page asked for an apply. `library` is the cycle's inventory, when
/// it could be read.
pub(super) async fn run(http: &reqwest::Client, args: &Args, settings: &RuntimeSettings, library: Option<(&[ArrMovie], &[ArrSeries])>) {
    let config = &settings.trash;
    let dir = state_dir();
    let previous = TrashState::read(&dir).unwrap_or_default();
    let request = take_request(&dir);
    if !config.enabled {
        if request.is_some() {
            eprintln!("[flinch-arrd] trash: apply request ignored: the TRaSH sync is off");
        }
        if previous.enabled {
            // Keep the record of what FLINCH created; drop the stale preview.
            let off = TrashState {
                enabled: false,
                apps: previous.apps.into_iter().map(|app| trash::AppState { changes: Vec::new(), ..app }).collect(),
                ..previous
            };
            write(&off, &dir);
        }
        return;
    }
    // Dry run unless the planner says otherwise; FLINCH_DRY_RUN forces one,
    // as for every other write the daemon makes.
    let forced_dry = std::env::var("FLINCH_DRY_RUN").map(|v| v == "1" || v == "true").unwrap_or(false);
    let dry_run = settings.planner.dry_run || forced_dry;
    let now = now();
    let fingerprint = trash::fingerprint(config);
    let scheduled = now >= previous.refreshed_at_unix.saturating_add(u64::from(config.schedule_hours) * 3600);
    // A failed read is retried every cycle rather than left for the schedule.
    let read_failed = previous.error.is_some() || previous.apps.iter().any(|app| app.error.is_some());
    let changed = !previous.enabled || previous.config_fingerprint != fingerprint || previous.dry_run != dry_run || read_failed;
    if !(scheduled || changed || request.is_some()) {
        return;
    }
    // Only the sources some instance reads are fetched.
    let trash_guide = if config.uses(Source::Trash) { Some(load_guide(http, &dir, &config.guide_commit).await) } else { None };
    let pcd_guide = if config.uses(Source::Pcd) { Some(load_pcd(http, &dir, &config.pcd).await) } else { None };
    let (trash_guide, pcd_guide) = match (trash_guide.transpose(), pcd_guide.transpose()) {
        (Ok(trash_guide), Ok(pcd_guide)) => (trash_guide, pcd_guide),
        (Err(error), _) | (_, Err(error)) => {
            eprintln!("[flinch-arrd] trash: {error}");
            let failed = TrashState {
                enabled: true,
                refreshed_at_unix: now,
                config_fingerprint: fingerprint,
                dry_run,
                error: Some(error),
                ..previous
            };
            write(&failed, &dir);
            return;
        }
    };
    let requested: Option<BTreeSet<String>> = request.as_ref().map(|request| request.changes.iter().cloned().collect());
    // The page's selection wins; without one, `apply` syncs everything on schedule.
    let automatic = requested.is_none() && config.apply;
    let releases: Option<ReleaseCache> = library.map(|_| super::read_state(&dir.join("releases.json")));
    let (smallest, largest) = releases.as_ref().map(|cache| (cache.smallest(), cache.largest())).unwrap_or_default();
    let outlook = outlook(&dir);
    let empty = guide::AppGuide::default();
    let mut apps = Vec::new();
    let mut outcomes = Vec::new();
    for arr in &args.arrs {
        let (app, name) = (arr.app, arr.name.as_str());
        let Some(instance) = config.instances.of(app, name) else { continue };
        let client = ArrClient::new(http, app, &arr.base, &arr.key).named(name);
        let owned = previous.instance(app, name).map(|state| state.owned.clone()).unwrap_or_default();
        let source = match instance.source {
            Source::Trash => trash_guide.as_ref(),
            Source::Pcd => pcd_guide.as_ref(),
        };
        let usage = library.map(|(movies, series)| Usage::of((app, name), movies, series, &smallest, &largest));
        let sync = AppSync {
            client: &client,
            config: instance,
            guide: source.map_or(&empty, |guide| guide.app(app)),
            delete_unmanaged: config.delete_unmanaged_custom_formats,
            delete_unused_profiles: config.delete_unused_profiles,
            owned,
            usage: usage.as_ref(),
            outlook,
            keep_profile: match arr.is_default() {
                true => settings.quality_actions.profile_name(app),
                false => Some(arr.compact_profile.as_str()).filter(|name| !name.is_empty()),
            },
        };
        let selection = match (&requested, automatic) {
            (Some(ids), _) => Some((Selection::Only(ids), dry_run)),
            (None, true) => Some((Selection::All, dry_run)),
            (None, false) => None,
        };
        let (state, outcome) = trash::sync_app(sync, selection).await;
        report(&arr.key(), &state, outcome.as_ref());
        if let Some(outcome) = outcome {
            outcomes.push(AppApply { app, instance: name.to_string(), outcome });
        }
        apps.push(state);
    }
    let last_apply = if requested.is_some() || automatic {
        Some(ApplyRecord { at_unix: now, dry_run, automatic, requested: request.map(|r| r.changes).unwrap_or_default(), apps: outcomes })
    } else {
        previous.last_apply
    };
    let state = TrashState {
        enabled: true,
        guide_commit: trash_guide.map(|guide| guide.commit).unwrap_or_default(),
        pcd_commit: pcd_guide.as_ref().map(|guide| guide.commit.clone()).unwrap_or_default(),
        pcd_license: pcd_guide.and_then(|guide| guide.license),
        refreshed_at_unix: now,
        config_fingerprint: fingerprint,
        dry_run,
        apply_automatically: config.apply,
        error: None,
        apps,
        last_apply,
    };
    write(&state, &dir);
}
