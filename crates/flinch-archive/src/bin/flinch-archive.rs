//! Plan storage reclamation offline, from *arr-style library cards.
//!
//! Reads one JSON card per item (see `fixtures/arr/` for the shape), forecasts
//! one described disk, and plans the least-regret evictions that fit the
//! forecast with the same engine the daemon runs. It prints the plan and can
//! write it as an `eviction-plan.json` manifest. It never deletes anything.

use anyhow::{Context, Result};
use clap::Parser;
use flinch_archive::capacity::{CapacityConfig, SlidingWindowCapacityForecaster, VolumeForecast, VolumeLoad, INGEST_HISTORY_DAYS};
use flinch_archive::plan::{candidates, generate_eviction_plan, Exclusion, Manifest, PlannerConfig};
use flinch_archive::ArchiveCard;
use std::path::PathBuf;

const GIB: u64 = 1 << 30;
const DISK: &str = "disk";

#[derive(Parser, Debug)]
#[command(name = "flinch-archive", about = "Plan least-regret evictions for one disk, offline")]
struct Args {
    #[arg(long, default_value = "fixtures/arr/cards.json")]
    cards: PathBuf,
    /// Disk usage in GiB.
    #[arg(long)]
    used_gb: u64,
    /// Disk size in GiB.
    #[arg(long)]
    total_gb: u64,
    /// Bytes grabbed per day, in GiB, held steady over the history window.
    #[arg(long, default_value_t = 0)]
    ingest_gb_per_day: u64,
    /// Downloads still in flight, in GiB.
    #[arg(long, default_value_t = 0)]
    queue_gb: u64,
    #[arg(long, default_value_t = 80)]
    target_pct: u8,
    #[arg(long, default_value_t = 95)]
    emergency_pct: u8,
    #[arg(long, default_value_t = 14)]
    window_days: u32,
    #[arg(long, default_value_t = 50)]
    headroom_gb: u64,
    #[arg(long, default_value_t = 30)]
    grace_days: u32,
    /// Let never-played items compete.
    #[arg(long)]
    never_played: bool,
    /// Write the manifest here as well.
    #[arg(long)]
    plan: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let cards: Vec<ArchiveCard> =
        serde_json::from_str(&std::fs::read_to_string(&args.cards).with_context(|| format!("reading {}", args.cards.display()))?)
            .with_context(|| format!("parsing cards from {}", args.cards.display()))?;
    let capacity = CapacityConfig {
        max_capacity_bytes: None,
        target_utilization: f64::from(args.target_pct) / 100.0,
        emergency_utilization: f64::from(args.emergency_pct) / 100.0,
        sliding_window_days: args.window_days,
        headroom_buffer_bytes: args.headroom_gb.saturating_mul(GIB),
        ..CapacityConfig::default()
    };
    let forecaster = SlidingWindowCapacityForecaster::new(capacity).context("capacity flags")?;
    let ingest = [args.ingest_gb_per_day.saturating_mul(GIB); INGEST_HISTORY_DAYS];
    let load = VolumeLoad {
        total_bytes: args.total_gb.saturating_mul(GIB),
        used_bytes: args.used_gb.saturating_mul(GIB),
        daily_ingest: &ingest,
        queue_bytes: args.queue_gb.saturating_mul(GIB),
        in_flight_bytes: 0,
    };
    let forecast = forecaster.forecast(&load).context("a disk of 0 GiB has nothing to forecast")?;
    let forecasts = [VolumeForecast { volume: DISK.to_string(), forecast }];
    let planner = PlannerConfig { grace_period_days: args.grace_days, ..PlannerConfig::default() };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let never_played = (!args.never_played).then_some(Exclusion::NeverPlayedOff);
    let candidates = candidates::offline(&cards, DISK, never_played, now, &planner);
    let plan = generate_eviction_plan(&candidates, &forecasts, &planner)?;

    let gib = |bytes: u64| bytes as f64 / GIB as f64;
    let f = &forecasts[0].forecast;
    println!(
        "{:.1}% used, {:.1} GiB projected in {} d: free {:.1} GiB",
        f.current_utilization * 100.0,
        gib(f.projected_used_bytes),
        args.window_days,
        gib(f.target_reclaim_bytes)
    );
    println!(
        "{} of {} items selectable; plan takes {} ({:.1} GiB, regret {:.2}){}",
        plan.candidates_count,
        cards.len(),
        plan.items.len(),
        gib(plan.total_reclaimed_bytes),
        plan.total_regret,
        plan.method.map_or(" — healthy, solver skipped".to_string(), |method| format!(" by {method:?}")),
    );
    for item in &plan.items {
        println!("  EVICT {:>8.2} GiB  {:<40}  {}", gib(item.size_bytes), item.title, item.reason);
    }
    if let Some(path) = &args.plan {
        let manifest = serde_json::to_vec_pretty(&Manifest::new(&plan, &forecasts, true, now))?;
        std::fs::write(path, manifest).with_context(|| format!("writing {}", path.display()))?;
        println!("plan written to {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn cli_declares_its_flags() {
        use clap::CommandFactory;
        super::Args::command().debug_assert();
    }
}
