//! Plan storage reclamation from *arr-style library cards.
//!
//! Reads one JSON card per item (see `fixtures/arr/` for the shape), applies
//! the deterministic policy, sharpens with the model, and writes a plan to
//! `stdout` (or `--plan PATH`). It never deletes: `apply` is the only command
//! that touches the filesystem, and it only MOVES candidates to a trash
//! directory with a TTL so an over-delete is recoverable.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use flinch_archive::plan::{Baseline, ReclaimGoal};
use flinch_archive::{ArchiveCard, ArchivePolicy};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "flinch-archive", about = "Sharp storage-reclamation planning")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Build a delete plan from library cards.
    Plan {
        #[arg(long, default_value = "fixtures/arr/cards.json")]
        cards: PathBuf,
        #[arg(long, default_value_t = 90.0)]
        retention_days: f32,
        /// Free until this many GiB are reclaimed (0 = delete everything safe).
        /// Ignored when `--used-gb`/`--total-gb` describe the disk instead.
        #[arg(long, default_value_t = 0)]
        target_free_gb: u64,
        #[arg(long, default_value_t = 0.95)]
        delete_floor: f32,
        #[arg(long)]
        plan: Option<PathBuf>,
        /// Disk usage in GiB. With `--total-gb`, the plan follows watermark
        /// governance: nothing below the ceiling; above it, free down to the
        /// release mark, least expected regret first.
        #[arg(long, requires = "total_gb")]
        used_gb: Option<u64>,
        /// Disk size in GiB (see `--used-gb`).
        #[arg(long, requires = "used_gb")]
        total_gb: Option<u64>,
        #[arg(long, default_value_t = 80.0)]
        ceiling_pct: f32,
        #[arg(long, default_value_t = 75.0)]
        release_pct: f32,
    },
    /// Move a previous plan's entries to a trash directory (recoverable).
    Apply {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        trash: PathBuf,
        /// TTL in days after which trash is purged.
        #[arg(long, default_value_t = 30)]
        ttl_days: u64,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Command::Plan { cards, retention_days, target_free_gb, delete_floor, plan, used_gb, total_gb, ceiling_pct, release_pct } => {
            let cards_value = serde_json::from_str::<Vec<ArchiveCard>>(
                &std::fs::read_to_string(&cards).with_context(|| format!("reading {}", cards.display()))?,
            )
            .with_context(|| format!("parsing cards from {}", cards.display()))?;

            const GIB: u64 = 1024 * 1024 * 1024;
            let gib = |bytes: u64| bytes as f64 / GIB as f64;
            let mut policy = ArchivePolicy { retention_days, ..ArchivePolicy::default() };
            let goal = match (used_gb, total_gb) {
                (Some(used), Some(total)) => {
                    use flinch_archive::capacity::{decide_capacity, CapacityAction, CapacitySnapshot, Latch, Volume, Watermarks};
                    let marks = Watermarks::new(ceiling_pct / 100.0, release_pct / 100.0).with_context(|| {
                        format!("watermarks need 0 < release ({release_pct}%) <= ceiling ({ceiling_pct}%) <= 100")
                    })?;
                    let total_bytes = total.saturating_mul(GIB);
                    let disk = Volume {
                        path: "disk".to_string(),
                        total_bytes,
                        free_bytes: total_bytes.saturating_sub(used.saturating_mul(GIB)),
                    };
                    let snapshot = CapacitySnapshot::of(&[disk], marks).context("a described disk is always measured")?;
                    // A one-shot plan has no previous run, so only the ceiling can latch.
                    let decision = decide_capacity(&mut policy, Some(&snapshot), &Latch::default(), false, &Default::default());
                    println!("capacity {:.1}% of {total} GiB — ceiling {ceiling_pct}%, release {release_pct}%", snapshot.utilization * 100.0);
                    match decision.action {
                        CapacityAction::Evict { goal_bytes, .. } => {
                            println!("over the ceiling: free {:.1} GiB to reach the release mark", gib(goal_bytes));
                            ReclaimGoal::Bytes(goal_bytes)
                        }
                        CapacityAction::Idle | CapacityAction::Unmeasured => {
                            println!("under the ceiling: nothing is planned");
                            ReclaimGoal::Bytes(0)
                        }
                    }
                }
                _ if target_free_gb > 0 => ReclaimGoal::Bytes(target_free_gb.saturating_mul(GIB)),
                _ => ReclaimGoal::AllSafe,
            };
            let model = Baseline::new(policy);
            let result = flinch_archive::plan::build_plan(&cards_value, &model, &policy, delete_floor, &std::collections::HashMap::new(), &goal);

            println!("{} items scanned, {} candidates, {:.1} GiB planned ({:.1} GiB eligible)",
                cards_value.len(), result.entries.len(), gib(result.reclaimed_bytes), gib(result.eligible_bytes));
            if let Some(quality) = &result.quality {
                // The baseline is the labeler, so its Brier is exactly 0 by
                // construction. The number that matters is sharpness, and the
                // real test of a trained head is whether it agrees with the
                // deterministic rules on the easy cases while differentiating
                // the fuzzy edge — not whether it beats a Brier of 0.
                println!("sharpness {:.3}  brier {:.4}  ece {:.4}  (baseline is the labeler; a head's real test is the fuzzy-edge during held-out",
                    quality.sharpness, quality.brier, quality.ece);
            }
            if let Some(goal_bytes) = result.goal_bytes.filter(|bytes| *bytes > 0) {
                println!("goal {:.1} GiB: {}", gib(goal_bytes),
                    if result.goal_met { "met" } else { "NOT met — raise retention or inspect keep-lists" });
            }
            for entry in &result.entries {
                println!("  DELETE {:-10.2} GiB  {:<40}  p={:.2}",
                    gib(entry.size_bytes), entry.title, entry.delete_probability);
            }

            if let Some(path) = plan {
                let file = std::fs::File::create(&path).with_context(|| format!("writing {}", path.display()))?;
                serde_json::to_writer_pretty(file, &result).context("serialising plan")?;
                println!("plan written to {}", path.display());
            }
        }
        Command::Apply { plan, trash, ttl_days } => {
            let plan_value = std::fs::read_to_string(&plan).with_context(|| format!("reading {}", plan.display()))?;
            let parsed: flinch_archive::plan::Plan = serde_json::from_str(&plan_value).context("parsing plan")?;
            if parsed.entries.is_empty() {
                println!("plan is empty; nothing to move");
                return Ok(());
            }
            std::fs::create_dir_all(&trash).context("creating trash")?;
            println!(
                "NOTE: entries here are identified by id only — the real SCRIPT that maps ids to on-disk paths lives in the homelab repo. This command moves nothing without that mapping and is currently a dry-run guard."
            );
            let ttl_purge_hint = ttl_days * 86400;
            println!("would move {} items to {} (TTL {ttl_days}d = {ttl_purge_hint}s)", parsed.entries.len(), trash.display());
            for entry in &parsed.entries {
                println!("  -> {}", entry.title);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn cli_declares_commands() {
        use clap::CommandFactory;
        super::Args::command().debug_assert();
    }
}