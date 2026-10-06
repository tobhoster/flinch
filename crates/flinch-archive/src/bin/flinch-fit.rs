//! `flinch-fit` — fit the hazard behind P(watch) on this household's own
//! playback record and report how it does against the hand-set priors.
//!
//! Reads what the daemon already records (`items.json` for the library,
//! `playback.json` and `tautulli.json` for every play), builds a temporal panel
//! of "as of" questions, judges each candidate out of fold, and prints the
//! report. With `--write` it writes `hazard.json` only if the fit beats the
//! priors (and removes a stale one if not) — the same gate the daemon's daily
//! refit uses. `--now <unix>` pins the panel's date to reproduce a report.
//!
//! Exit codes: 0 report produced, 1 state unreadable or output unwritable.

use clap::Parser;
use flinch_archive::fit::adopt::{self, Adoption};
use flinch_archive::fit::load;
use flinch_archive::regret::HazardModel;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "flinch-fit", about = "Fit the P(watch) hazard on this household's playback record")]
struct Args {
    #[arg(long, default_value = "/state")]
    state_dir: PathBuf,
    /// The panel's "now", unix seconds (default: the current time).
    #[arg(long)]
    now: Option<u64>,
    /// Write hazard.json if the fit beats the priors; remove a stale one if not.
    #[arg(long)]
    write: bool,
    /// Print the fitted model as JSON instead of the report.
    #[arg(long)]
    json: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let now = args.now.unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()));
    let household = load::load_household(&args.state_dir)?;
    let (dataset, model) = adopt::panel_fit(&household, now);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&model)?);
    } else {
        let m = &model.metrics;
        println!("{}", model.fitted_on);
        println!("{} rows, {} played ({} titles), horizon {:.0} d", dataset.len(), m.played, m.played_items, m.horizon_days);
        println!("                 AUC     Brier   ECE");
        println!("  {:<14} {:.3}   {:.3}   {:.3}", model.kind.label(), m.auc, m.brier, m.ece);
        println!("  {:<14} {:.3}   {:.3}   {:.3}", "priors", m.priors_auc, m.priors_brier, m.priors_ece);
        print_params("fitted", &model.hazard);
        print_params("priors", &HazardModel::default());
        match model.shortfall() {
            None => println!("beats the priors out of fold"),
            Some(reason) => println!("priors stay: {reason}"),
        }
    }
    if args.write {
        let outcome = adopt::adopt(&args.state_dir, &model)?;
        eprintln!(
            "[flinch-fit] {}",
            match outcome {
                Adoption::Written => "hazard.json written",
                Adoption::Removed => "a stale hazard.json was removed: the priors run",
                Adoption::PriorsKept => "nothing written: the priors run",
            }
        );
    }
    Ok(())
}

fn print_params(label: &str, model: &HazardModel) {
    println!(
        "  {label:<7} λ₀ {:.4}/d · β recency {:+.2} · viewings {:+.2} · show plays {:+.2} · cycle {:+.2}",
        model.lambda0_per_day, model.beta_recency, model.beta_scrobbles, model.beta_velocity, model.beta_cyclical
    );
}
