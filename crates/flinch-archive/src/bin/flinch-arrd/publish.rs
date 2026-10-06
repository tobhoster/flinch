//! What one cycle publishes for the UI: status.json with the items beside it,
//! and a history point. The plan itself is `eviction-plan.json` (see main).

use super::state_dir;
use anyhow::Result;
use flinch_archive::daemon::{self, HistoryPoint, NeverPlayedHold, QualityCounts};
use flinch_archive::govern::Governance;
use flinch_archive::maintainerr::SyncSummary;
use flinch_archive::outside::OutsideDeletion;
use flinch_archive::watch::EvidenceHealth;
use flinch_archive::{ItemSnapshot, ReconcileOutput, StatusSnapshot};

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
