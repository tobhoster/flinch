//! The daemon's side of inflow actions ([`flinch_archive::inflow::act`]):
//! list each default instance's import lists for the approval page, then send
//! this cycle's writes — or print them in a dry run — reading each one back
//! before it is recorded. A show is unmonitored in its own Sonarr instance
//! (its subject names it). Only a confirmed write enters the ledger, so a dry
//! run or a failed write is tried again next cycle.

use super::arr_write::ArrWriter;
use super::Args;
use anyhow::{bail, Context, Result};
use flinch_archive::capacity::{App, VolumeForecast};
use flinch_archive::inflow::act::{self, Action, ImportListRef, InflowActionsStatus, InflowLedger};
use flinch_archive::inflow::Suggestion;
use reqwest::Method;

const LEDGER_FILE: &str = "inflow-actions.json";

/// What a write did.
enum Done {
    /// Sent and read back as asked.
    Confirmed,
    /// Already as asked: nothing sent, nothing to record.
    Unchanged,
    /// Printed, not sent.
    DryRun,
}

/// One cycle of inflow actions. `None` while they are off and FLINCH holds
/// nothing it changed.
pub(super) async fn run(
    http: &reqwest::Client,
    args: &Args,
    settings: &flinch_archive::daemon::RuntimeSettings,
    suggestions: &[Suggestion],
    forecasts: &[VolumeForecast],
    dry_run: bool,
    now: u64,
) -> Option<InflowActionsStatus> {
    let config = &settings.inflow_actions;
    let path = super::state_dir().join(LEDGER_FILE);
    let mut ledger: InflowLedger = super::read_state(&path);
    if !config.enabled && ledger.is_empty() {
        return None;
    }
    let before = ledger.clone();
    ledger.prune(config);
    let over_target = forecasts.iter().any(|forecast| forecast.forecast.target_reclaim_bytes > 0);
    let mut status = InflowActionsStatus { enabled: config.enabled, dry_run, over_target, ..InflowActionsStatus::default() };
    if config.enabled {
        for app in [App::Sonarr, App::Radarr] {
            let Some(writer) = args.arr(app, "").and_then(|arr| ArrWriter::new(http, arr, dry_run)) else { continue };
            match writer.get("/api/v3/importlist").await {
                Ok(answer) => status.import_lists.extend(act::known_lists(app, &answer)),
                Err(error) => status.problems.push(format!("{} import lists unreadable: {error:#}", writer.name())),
            }
        }
    }
    for action in act::plan(config, suggestions, over_target, &ledger) {
        let (what, result) = match &action {
            Action::Unmonitor { series_id, subject, title } => {
                (format!("unmonitor future seasons of {title}"), unmonitor(http, args, subject, *series_id, dry_run).await)
            }
            Action::ListOff(list) => (format!("switch {list} off"), toggle(http, args, *list, false, dry_run).await),
            Action::ListOn(list) => (format!("switch {list} back on"), toggle(http, args, *list, true, dry_run).await),
        };
        match result {
            Ok(done) => {
                record(&mut ledger, &action, &done, now);
                status.acted.push(match done {
                    Done::Confirmed => what,
                    Done::Unchanged => format!("{what}: already so"),
                    Done::DryRun => format!("would {what}"),
                });
            }
            Err(error) => status.problems.push(format!("{what}: {error:#}")),
        }
    }
    for line in &status.acted {
        println!("[flinch-arrd] inflow actions: {line}");
    }
    for problem in &status.problems {
        eprintln!("[flinch-arrd] inflow actions: {problem}");
    }
    if ledger != before {
        super::write_state(&path, &ledger);
    }
    status.unmonitored = ledger.unmonitored.keys().cloned().collect();
    status.lists_off = ledger.lists_off.iter().map(|(list, _)| *list).collect();
    Some(status)
}

/// Record a confirmed write. A list found already off is not FLINCH's to
/// switch back on; one found already on needs no restoring.
fn record(ledger: &mut InflowLedger, action: &Action, done: &Done, now: u64) {
    match (action, done) {
        (_, Done::DryRun) => {}
        (Action::Unmonitor { subject, .. }, _) => {
            ledger.unmonitored.insert(subject.clone(), now);
        }
        (Action::ListOff(list), Done::Confirmed) => ledger.lists_off.push((*list, now)),
        (Action::ListOff(_), Done::Unchanged) => {}
        (Action::ListOn(list), _) => ledger.lists_off.retain(|(off, _)| off != list),
    }
}

/// The writer of `app`'s `instance` (empty: the default).
fn writer<'a>(http: &'a reqwest::Client, args: &'a Args, app: App, instance: &str, dry_run: bool) -> Result<ArrWriter<'a>> {
    args.arr(app, instance)
        .and_then(|arr| ArrWriter::new(http, arr, dry_run))
        .with_context(|| format!("{} is not configured", flinch_archive::ids::instance_key(app, instance)))
}

/// `PUT /api/v3/series/{id}` with the future unmonitored, read back, in the
/// Sonarr instance `subject` names.
async fn unmonitor(http: &reqwest::Client, args: &Args, subject: &str, series_id: u32, dry_run: bool) -> Result<Done> {
    let instance = flinch_archive::ids::ArrRef::parse(subject).map_or("", |show| show.instance);
    let sonarr = writer(http, args, App::Sonarr, instance, dry_run)?;
    let path = format!("/api/v3/series/{series_id}");
    let mut series = sonarr.get(&path).await?;
    if !act::unmonitor_future(&mut series) {
        return Ok(Done::Unchanged);
    }
    if sonarr.send(Method::PUT, &path, &series).await?.is_none() {
        return Ok(Done::DryRun);
    }
    if !act::future_unmonitored(&sonarr.get(&path).await?) {
        bail!("{} did not keep the change", sonarr.name());
    }
    Ok(Done::Confirmed)
}

/// `PUT /api/v3/importlist/{id}` with automatic add set, read back.
async fn toggle(http: &reqwest::Client, args: &Args, list: ImportListRef, on: bool, dry_run: bool) -> Result<Done> {
    let app = writer(http, args, list.app, "", dry_run)?;
    let path = format!("/api/v3/importlist/{}", list.id);
    let mut resource = app.get(&path).await?;
    match act::set_auto_add(&mut resource, list.app, on) {
        None => bail!("the list has no automatic-add switch this version of FLINCH knows"),
        Some(false) => return Ok(Done::Unchanged),
        Some(true) => {}
    }
    if app.send(Method::PUT, &path, &resource).await?.is_none() {
        return Ok(Done::DryRun);
    }
    if act::auto_add(&app.get(&path).await?, list.app) != Some(on) {
        bail!("{} did not keep the change", app.name());
    }
    Ok(Done::Confirmed)
}
