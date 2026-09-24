//! What one cycle publishes for the UI and the operator: status.json with the
//! items beside it, a history point, and the plan summary.

use super::state_dir;
use anyhow::Result;
use flinch_archive::daemon::{self, HistoryPoint, InflowCounts};
use flinch_archive::govern::Governance;
use flinch_archive::maintainerr::SyncSummary;
use flinch_archive::watch::EvidenceHealth;
use flinch_archive::{ItemSnapshot, ReconcileOutput, StatusSnapshot};

/// The run's own facts, published beside the items.
pub(super) struct Run<'a> {
    pub(super) report: &'a ReconcileOutput,
    pub(super) sync: SyncSummary,
    pub(super) governance: &'a Governance,
    /// Verified FLINCH collection members still on disk, with their bytes (as
    /// of the last enforcing cycle during a dry run or an outage).
    pub(super) handed: Vec<(&'a str, u64)>,
    pub(super) enforcing: bool,
    /// Seconds to the next run; `None` for a single `--once` run.
    pub(super) interval_s: Option<u64>,
    pub(super) model: String,
    /// What arming never-played reclaim would add: items and GiB.
    pub(super) shadow: (u64, f32),
    pub(super) health: EvidenceHealth,
}

pub(super) fn publish(run: Run<'_>, items: &[ItemSnapshot]) -> Result<()> {
    let Run { report, sync, governance, handed, enforcing, interval_s, model, shadow: (shadow_items, shadow_gib), health } = run;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let dir = state_dir();
    let status = StatusSnapshot {
        scanned: report.scanned,
        delete_candidates: report.delete_candidates,
        kept: report.kept,
        reclaimed_bytes: report.reclaimed_bytes,
        protections_added: sync.exclusions_added,
        protections_skipped_repeat: sync.already_protected,
        dry_run: !enforcing,
        inflow: InflowCounts::of(items),
        ran_at_unix: now,
        interval_s: interval_s.unwrap_or(0),
        next_run_unix: interval_s.map_or(0, |interval| now + interval),
        movies: items.iter().filter(|i| i.kind == "movie").count() as u64,
        seasons: items.iter().filter(|i| i.kind == "season").count() as u64,
        model,
        shadow_items,
        shadow_gib,
        capacity: governance.status(&report.volumes, handed),
        eligible_bytes: report.eligible_bytes,
        last_error: None,
        last_error_at: None,
        evidence_problems: health.problems().iter().map(|problem| problem.to_string()).collect(),
        evidence: health,
        sync,
        fit: flinch_archive::fit::adopt::read_status(&dir),
        benchmark: flinch_archive::fit::bench::read_benchmark(&dir),
    };
    daemon::write_snapshots(&dir.join("status.json"), &dir.join("items.json"), &status, items)?;
    daemon::append_history(
        &dir.join("history.json"),
        &HistoryPoint {
            ran_at_unix: status.ran_at_unix,
            scanned: report.scanned as u64,
            delete_candidates: report.delete_candidates as u64,
            reclaimed_bytes: report.reclaimed_bytes,
            protections_added: status.sync.exclusions_added as u64,
            dry_run: Some(status.dry_run),
            utilization: governance.snapshot.as_ref().map(|snapshot| snapshot.utilization),
        },
    )?;
    let outcome = serde_json::json!({
        "scanned": report.scanned,
        "delete_candidates": report.delete_candidates,
        "kept": report.kept,
        "reclaimed_bytes": report.reclaimed_bytes,
        "protections_added": status.sync.exclusions_added,
        "protections_skipped_repeat": status.sync.already_protected,
        "maintainerr": &status.sync,
    });
    println!("[flinch-arrd] {outcome}");
    flinch_archive::persist::replace(&dir.join("plan.json"), outcome.to_string().as_bytes())?;
    Ok(())
}
