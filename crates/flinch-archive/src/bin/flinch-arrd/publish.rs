//! What one cycle publishes: the plan as `eviction-plan.json` and a log line,
//! then for the UI status.json with the items beside it, and a history point.

use super::state_dir;
use anyhow::Result;
use flinch_archive::arr::instances::Connection;
use flinch_archive::capacity::VolumeForecast;
use flinch_archive::daemon::{self, HistoryPoint, NeverPlayedHold, QualityCounts};
use flinch_archive::embedding::EmbeddingStatus;
use flinch_archive::govern::Governance;
use flinch_archive::maintainerr::SyncSummary;
use flinch_archive::outside::OutsideDeletion;
use flinch_archive::plan::{EvictionPlan, Manifest};
use flinch_archive::watch::EvidenceHealth;
use flinch_archive::{ItemSnapshot, ReconcileOutput, StatusSnapshot};

/// Log the plan and write it, dry run or not, before anything acts on it.
pub(super) fn plan(plan: &EvictionPlan, forecasts: &[VolumeForecast], dry_run: bool, now: u64) -> Result<()> {
    let gib = |bytes: u64| bytes as f64 / 1_073_741_824.0;
    println!(
        "[flinch-arrd] plan: {} item(s), {:.1} of {:.1} GiB needed, regret {:.2}{}",
        plan.items.len(),
        gib(plan.total_reclaimed_bytes),
        gib(plan.target_bytes),
        plan.total_regret,
        plan.method.map_or(" (healthy: solver skipped)".to_string(), |method| format!(" ({method:?})")),
    );
    if !plan.moves.is_empty() {
        println!("[flinch-arrd] plan: {} move(s) to the archive, {:.1} GiB", plan.moves.len(), gib(plan.moved_bytes()));
    }
    if let Some(error) = &plan.solver_error {
        eprintln!("[flinch-arrd] HiGHS failed, plan made greedily: {error}");
    }
    std::fs::create_dir_all(state_dir()).ok();
    let manifest = serde_json::to_vec_pretty(&Manifest::new(plan, forecasts, dry_run, now))?;
    flinch_archive::persist::replace(&state_dir().join("eviction-plan.json"), &manifest)?;
    Ok(())
}

/// The run's own facts, published beside the items.
pub(super) struct Run<'a> {
    pub(super) report: &'a ReconcileOutput,
    pub(super) sync: SyncSummary,
    pub(super) governance: &'a Governance,
    /// Verified FLINCH collection members still on disk, with their bytes (as
    /// of the last live cycle during a dry run or an outage).
    pub(super) handed: Vec<(&'a str, u64)>,
    pub(super) dry_run: bool,
    /// Seconds to the next run; `None` for a single `--once` run.
    pub(super) interval_s: Option<u64>,
    pub(super) model: String,
    /// What enabling never-played reclaim would add: items and GiB.
    pub(super) shadow: (u64, f32),
    pub(super) health: EvidenceHealth,
    /// Why never-played reclaim is held off this cycle, if it is.
    pub(super) never_played_hold: Option<NeverPlayedHold>,
    /// Whether the settings ask never-played reclaim to run this cycle.
    pub(super) never_played_requested: bool,
    /// Files something other than FLINCH removed lately.
    pub(super) outside: Vec<OutsideDeletion>,
    /// The taste vectors after this cycle's refresh.
    pub(super) embedding: EmbeddingStatus,
    /// Incoming storage likely wasted, for the operator.
    pub(super) inflow: Vec<flinch_archive::inflow::Suggestion>,
    /// What acting on approved advice did; `None` while off.
    pub(super) inflow_actions: Option<flinch_archive::inflow::act::InflowActionsStatus>,
    /// What this cycle's notifications did; `None` with no channel.
    pub(super) notify: Option<flinch_archive::notify::SendReport>,
    /// Storage by theme; `None` before themes exist.
    pub(super) themes: Option<flinch_archive::themes::ThemesStatus>,
    /// Downgrade moves, upgrade churn and upgrade searches; each `None` while off.
    pub(super) quality: super::upgrade_search::Quality,
    /// What the operator's rules did; `None` without rules.
    pub(super) rules: Option<flinch_archive::rules::RulesStatus>,
    /// What the torrent clients hold and keep; `None` with no client.
    pub(super) torrents: Option<flinch_archive::torrents::map::TorrentStatus>,
    /// What the native executor did; `None` while Maintainerr executes.
    pub(super) native: Option<flinch_archive::executor::NativeStatus>,
    /// What the streaming lookups know; `None` while streaming is off.
    pub(super) streaming: Option<flinch_archive::signals::streaming::StreamingStatus>,
    /// Duplicate copies; `None` while the finder is off.
    pub(super) dupes: Option<flinch_archive::dupes::DupesStatus>,
    /// Household self-service; `None` while it is off.
    pub(super) household: Option<flinch_archive::requests::HouseholdStatus>,
    /// The archive tier; `None` while it is off.
    pub(super) archive: Option<flinch_archive::archive::ArchiveStatus>,
    /// Every *arr instance of the cycle; published by [`Connection::view`],
    /// never with its key.
    pub(super) arrs: &'a [Connection],
}

pub(super) fn publish(run: Run<'_>, items: &[ItemSnapshot]) -> Result<()> {
    let Run {
        report,
        sync,
        governance,
        handed,
        dry_run,
        interval_s,
        model,
        shadow: (shadow_items, shadow_gib),
        health,
        never_played_hold,
        never_played_requested,
        outside,
        embedding,
        inflow,
        inflow_actions,
        notify,
        themes,
        quality,
        rules,
        torrents,
        native,
        streaming,
        dupes,
        household,
        archive,
        arrs,
    } = run;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let dir = state_dir();
    let plan = &report.plan;
    let capacity = governance.status(plan, handed);
    let status = StatusSnapshot {
        scanned: report.scanned,
        delete_candidates: plan.items.len(),
        kept: report.scanned - plan.items.len(),
        reclaimed_bytes: plan.total_reclaimed_bytes,
        protections_added: sync.exclusions_added,
        protections_skipped_repeat: sync.already_protected,
        dry_run,
        quality: QualityCounts::of(items),
        ran_at_unix: now,
        interval_s: interval_s.unwrap_or(0),
        next_run_unix: interval_s.map_or(0, |interval| now + interval),
        movies: items.iter().filter(|i| i.kind == "movie").count() as u64,
        seasons: items.iter().filter(|i| i.kind == "season").count() as u64,
        model,
        shadow_items,
        shadow_gib,
        capacity,
        eligible_bytes: plan.eligible_bytes,
        last_error: None,
        last_error_at: None,
        evidence_problems: health.problems().iter().map(|problem| problem.to_string()).collect(),
        never_played_hold,
        never_played_requested,
        evidence: health,
        sync,
        fit: flinch_archive::fit::adopt::read_status(&dir),
        outside_deletions: outside,
        embedding: Some(embedding),
        inflow,
        notify,
        themes,
        quality_actions: quality.actions,
        upgrade_churn: quality.churn,
        upgrade_search: quality.search,
        rules,
        inflow_actions,
        torrents,
        trash: flinch_archive::trash::TrashState::read(&dir).and_then(|state| state.summary()),
        streaming,
        watch_sources: flinch_archive::watch_sources::WatchSourcesStatus::read(&dir),
        dupes,
        household,
        archive,
        arr_instances: arrs.iter().map(Connection::view).collect(),
        native,
    };
    daemon::write_snapshots(&dir.join("status.json"), &dir.join("items.json"), &status, items)?;
    daemon::append_history(
        &dir.join("history.json"),
        &HistoryPoint {
            ran_at_unix: status.ran_at_unix,
            scanned: report.scanned as u64,
            delete_candidates: status.delete_candidates as u64,
            reclaimed_bytes: status.reclaimed_bytes,
            protections_added: status.sync.exclusions_added as u64,
            dry_run: Some(status.dry_run),
            utilization: status.capacity.as_ref().map(|capacity| capacity.utilization as f32),
        },
    )?;
    Ok(())
}
