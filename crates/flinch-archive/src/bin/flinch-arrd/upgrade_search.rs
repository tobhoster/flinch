//! The daemon's side of upgrade searches ([`flinch_archive::quality::upgrade`]):
//! read each instance's cutoff-unmet list page by page, settle earlier searches
//! against it, select this cycle's searches and send them — or print them in
//! a dry run.
//!
//! Wire shapes, from the sources (`develop`): `GET /api/v3/wanted/cutoff`
//! (Radarr `CutoffController.cs`,
//! https://github.com/Radarr/Radarr/blob/develop/src/Radarr.Api.V3/Wanted/CutoffController.cs;
//! Sonarr `CutoffController.cs`,
//! https://github.com/Sonarr/Sonarr/blob/v5-develop/src/Sonarr.Api.V3/Wanted/CutoffController.cs).
//! `POST /api/v3/command` with `MoviesSearch {movieIds}`
//! (https://github.com/Radarr/Radarr/blob/develop/src/NzbDrone.Core/IndexerSearch/MoviesSearchCommand.cs)
//! or `SeasonSearch {seriesId, seasonNumber}`
//! (https://github.com/Sonarr/Sonarr/blob/v5-develop/src/NzbDrone.Core/IndexerSearch/SeasonSearchCommand.cs).

use super::arr_write::ArrWriter;
use super::quality_act::{self, Library};
use super::{upgrade_guard, Args};
use anyhow::Result;
use flinch_archive::daemon::RuntimeSettings;
use flinch_archive::govern::Governance;
use flinch_archive::quality::act::{self, QualityActionsStatus};
use flinch_archive::quality::churn::ChurnStatus;
use flinch_archive::quality::upgrade::{self, Inputs, SearchLedger, UpgradeSearchStatus, MAX_PAGES, PAGE_SIZE};
use reqwest::Method;
use std::collections::{HashMap, HashSet};

/// The cycle's quality steps, for the status.
pub(super) struct Quality {
    pub(super) actions: Option<QualityActionsStatus>,
    pub(super) churn: Option<ChurnStatus>,
    pub(super) search: Option<UpgradeSearchStatus>,
}

/// Downgrade moves, the churn guard, then upgrade searches: the guard's
/// flags this cycle hold their items back from a search.
pub(super) async fn quality_cycle(
    http: &reqwest::Client,
    args: &Args,
    settings: &RuntimeSettings,
    (library, governance): (Library<'_>, &Governance),
    dry_run: bool,
    now: u64,
) -> Quality {
    let actions = quality_act::run(http, args, settings, library, dry_run, now).await;
    let churn = upgrade_guard::run(http, args, &settings.upgrade_guard, library.arr, dry_run, now).await;
    let search = run(http, args, settings, (library, governance, churn.as_ref()), dry_run, now).await;
    Quality { actions, churn, search }
}

/// One cycle of upgrade searches. `None` while they are off and none was ever asked.
async fn run(
    http: &reqwest::Client,
    args: &Args,
    settings: &RuntimeSettings,
    (library, governance, churn): (Library<'_>, &Governance, Option<&ChurnStatus>),
    dry_run: bool,
    now: u64,
) -> Option<UpgradeSearchStatus> {
    let config = &settings.upgrade_search;
    let path = super::state_dir().join("upgrade-searches.json");
    let mut ledger: SearchLedger = super::read_state(&path);
    if !config.enabled && ledger.searches.is_empty() {
        return None;
    }
    let before = ledger.clone();
    let mut status = UpgradeSearchStatus { enabled: config.enabled, dry_run, max_per_day: config.max_per_day, ..Default::default() };
    let items = act::arr_items(library.arr.0, library.arr.1);
    let unmet = cutoff_unmet(http, args, &items, &mut status.problems).await;
    let sizes: HashMap<&str, u64> = library.candidates.iter().map(|candidate| (candidate.id.as_str(), candidate.size_bytes)).collect();
    ledger.settle(unmet.as_ref(), &sizes, now);
    ledger.prune(now);
    if let (true, Some(unmet)) = (config.enabled, &unmet) {
        status.unmet = unmet.len();
        let report = library.report;
        let planned: HashSet<&str> =
            report.deleted_ids.iter().map(String::as_str).chain(report.plan.items.iter().map(|item| item.id.as_str())).collect();
        let churning: HashSet<&str> =
            churn.map(|churn| churn.items.iter().map(|item| item.churn.card_id.as_str()).collect()).unwrap_or_default();
        let capacity = &governance.config;
        let headroom = upgrade::volume_headroom(&governance.forecasts, capacity.target_utilization, capacity.headroom_buffer_bytes);
        let inputs = Inputs {
            candidates: library.candidates,
            items: &items,
            unmet,
            planned: &planned,
            churning: &churning,
            largest_release: &library.signals.largest_release,
            headroom: &headroom,
            now,
        };
        let selection = upgrade::select(&inputs, &ledger, config);
        for pick in &selection.picks {
            let Some(writer) = args.arr(pick.app, &pick.instance).and_then(|arr| ArrWriter::new(http, arr, dry_run)) else {
                let instance = flinch_archive::ids::instance_key(pick.app, &pick.instance);
                status.problems.push(format!("{instance} not configured: its searches wait"));
                continue;
            };
            match writer.send(Method::POST, "/api/v3/command", &upgrade::command(pick)).await {
                Ok(None) => {}
                Ok(Some(_)) => {
                    println!("[flinch-arrd] upgrade: asked {} to search {} (P(watch) {:.2})", writer.name(), pick.title, pick.p_watch);
                    ledger.record(pick, now, Ok(()));
                }
                Err(error) => {
                    let reason: String = error.to_string().chars().take(160).collect();
                    eprintln!("[flinch-arrd] upgrade: {} search for {} failed: {error:#}", writer.name(), pick.card_id);
                    status.problems.push(format!("{} refused an upgrade search ({reason}): tried again in a day", writer.name()));
                    ledger.record(pick, now, Err(reason));
                }
            }
        }
        status.picks = selection.picks;
        status.held = selection.held;
    }
    if ledger != before {
        super::write_state(&path, &ledger);
    }
    status.searched_today = ledger.searched_since(now.saturating_sub(act::DAY_SECS));
    status.recent = ledger.recent();
    status.upgraded = ledger.upgraded();
    Some(status)
}

/// Every instance's cutoff-unmet cards; `None` when any instance could not
/// be read in full (nothing settles against half a list).
async fn cutoff_unmet(
    http: &reqwest::Client,
    args: &Args,
    items: &HashMap<String, act::ArrItem<'_>>,
    problems: &mut Vec<String>,
) -> Option<HashSet<String>> {
    let mut rows = Vec::new();
    let mut complete = true;
    for arr in &args.arrs {
        let Some(reader) = ArrWriter::new(http, arr, true) else { continue };
        match read_pages(&reader).await {
            Ok(found) => rows.extend(found.into_iter().map(|(id, season)| (arr.app, arr.name.as_str(), id, season))),
            Err(error) => {
                eprintln!("[flinch-arrd] upgrade: {} cutoff list unreadable: {error:#}", reader.name());
                problems.push(format!("{} cutoff-unmet list unreadable: no upgrade searches this cycle", reader.name()));
                complete = false;
            }
        }
    }
    complete.then(|| upgrade::unmet_cards(&rows, items))
}

/// One app's cutoff-unmet list, at most [`MAX_PAGES`] pages.
async fn read_pages(reader: &ArrWriter<'_>) -> Result<Vec<(u32, Option<u32>)>> {
    let mut found = Vec::new();
    for page in 1..=MAX_PAGES {
        let answer = reader.get(&upgrade::cutoff_path(page)).await?;
        let (rows, total) = upgrade::parse_cutoff_page(reader.app(), &answer);
        let empty = rows.is_empty();
        found.extend(rows);
        if empty || u64::from(page) * u64::from(PAGE_SIZE) >= total {
            break;
        }
    }
    Ok(found)
}
