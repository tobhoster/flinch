//! `flinch-fit` — fit, export and benchmark the reclaim scorecard on this
//! household's own playback record.
//!
//! Reads what the daemon already records (`items.json` for the library,
//! `playback.json` and `tautulli.json` for every play), builds a temporal panel
//! of "as of" questions, and then:
//!
//! - by default fits the same features inference uses and, with `--write`,
//!   writes `weights.json` **only if the fit beats the priors on held-out data**;
//! - `--export-panel <path>` writes every panel row as JSONL for an external
//!   decision model to answer;
//! - `--export-predictions <path>` writes FLINCH's prior P(safe) per row in the
//!   predictions format, as a worked example and a harness self-check;
//! - `--score <path>` scores an external model's predictions against FLINCH's
//!   priors and its out-of-fold household fit, on exactly the joined rows;
//! - `--against <base-url>` skips the file: it asks any System One server (JEV,
//!   Kev, `laya serve`, Nimble) the panel directly, one `played` question per
//!   row with the row's as-of state as text, and scores the answers as
//!   `--score` does. `--model` names the model to ask for, the bearer key is
//!   read from the variable `--api-key-env` names (never from argv), and
//!   `--concurrency` bounds requests in flight. A failed row is left
//!   unanswered. With `--write` the result is kept as `benchmark.json` for the
//!   status page, the endpoint reduced to `scheme://host[:port]`.
//!
//! Cut dates are relative to "now": pin `--now <unix>` (printed by
//! `--export-panel`) to score answers on a later day.
//!
//! Exit codes: 0 report produced, 1 state unreadable, an output unwritable, or
//! every `--against` request failed.

use anyhow::Context;
use clap::Parser;
use flinch_archive::fit::adopt::{self, Adoption};
use flinch_archive::fit::bench::{self, ask, Benchmark, HeadToHead, Predictions};
use flinch_archive::fit::eval;
use flinch_archive::fit::load::{self, Household};
use flinch_archive::fit::panel::{self, Example, PanelSpec};
use flinch_archive::fit::{self, export, DEPLOYED_PRIOR_TEMPERATURE, OPERATING_FLOOR as FLOOR};
use flinch_archive::persist;
use flinch_archive::score::ScoreWeights;
use flinch_archive::systemone::Endpoint;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(name = "flinch-fit", about = "Fit and benchmark the reclaim scorecard on this household's playback record")]
struct Args {
    #[arg(long, default_value = "/state")]
    state_dir: PathBuf,
    #[arg(long, default_value_t = fit::DEFAULT_HORIZON_DAYS)]
    horizon_days: f32,
    /// The panel's "now", unix seconds (default: the current time).
    #[arg(long)]
    now: Option<u64>,
    /// Write weights.json if the fit beats the priors (removes a stale one if not);
    /// with --against, write benchmark.json.
    #[arg(long)]
    write: bool,
    /// Machine-readable output for the fit report, --score and --against.
    #[arg(long)]
    json: bool,
    /// Write every panel row as JSONL: {id, cut_days, cut_unix, horizon_days, label, state, features}.
    #[arg(long)]
    export_panel: Option<PathBuf>,
    /// Write FLINCH's prior P(safe) per panel row as JSONL: {id, cut_days, cut_unix, p}.
    #[arg(long)]
    export_predictions: Option<PathBuf>,
    /// Score an external model's predictions (JSONL {id, cut_days, p}) against FLINCH.
    #[arg(long)]
    score: Option<PathBuf>,
    /// Ask a System One server (base URL, e.g. http://localhost:8009) every panel row and score it against FLINCH.
    #[arg(long)]
    against: Option<String>,
    /// The model --against asks for (the server's default when omitted).
    #[arg(long)]
    model: Option<String>,
    /// Environment variable holding --against's bearer key; the key never goes on the command line.
    #[arg(long, default_value = "SYSTEMONE_API_KEY")]
    api_key_env: String,
    /// --against requests in flight at once.
    #[arg(long, default_value_t = 4)]
    concurrency: usize,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let now = args.now.unwrap_or_else(unix_now);
    let household = load::load_household(&args.state_dir)?;
    let cuts = fit::default_cuts();
    let spec =
        PanelSpec { now, cuts_days: &cuts, horizon_days: args.horizon_days, tautulli_coverage_start: household.tautulli_coverage_start };
    let dataset = panel::build_dataset(&household.items, &spec);

    let mut acted = false;
    if let Some(path) = &args.export_panel {
        let rows = export::write_panel(create(path)?, &dataset, args.horizon_days)?;
        eprintln!("[flinch-fit] wrote {rows} panel row(s) to {} · pin --now {now} to score answers to it", path.display());
        acted = true;
    }
    if let Some(path) = &args.export_predictions {
        let priors = fit::forecasts(&dataset, &ScoreWeights::default(), DEPLOYED_PRIOR_TEMPERATURE);
        let rows = export::write_predictions(create(path)?, &dataset, &priors)?;
        eprintln!("[flinch-fit] wrote {rows} prior forecast(s) to {}", path.display());
        acted = true;
    }
    if let Some(path) = &args.score {
        let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        let result = bench::head_to_head(&dataset, &Predictions::parse(&text), now, args.horizon_days);
        if args.json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            print_head_to_head(
                &result,
                &path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned()),
            );
        }
        acted = true;
    }
    if let Some(base_url) = &args.against {
        against(&args, base_url, &dataset, now)?;
        acted = true;
    }
    if !acted {
        fit_report(&args, &household, &dataset, now, cuts.len())?;
    }
    Ok(())
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|elapsed| elapsed.as_secs()).unwrap_or(0)
}

fn create(path: &Path) -> anyhow::Result<BufWriter<std::fs::File>> {
    let file = std::fs::File::create(path).with_context(|| format!("cannot create {}", path.display()))?;
    Ok(BufWriter::new(file))
}

/// Ask a System One server every panel row, score it like `--score`, and with
/// `--write` keep the result as `benchmark.json`. Only the endpoint's origin is
/// ever printed or written: the base URL may carry a credential.
fn against(args: &Args, base_url: &str, dataset: &[Example], now: u64) -> anyhow::Result<()> {
    let origin = bench::endpoint_origin(base_url);
    let endpoint = Endpoint {
        base_url: base_url.to_string(),
        model: args.model.clone().filter(|model| !model.is_empty()),
        api_key: std::env::var(&args.api_key_env).ok().filter(|key| !key.is_empty()),
    };
    let concurrency = args.concurrency.max(1);
    eprintln!("[flinch-fit] asking {origin} about {} panel row(s), {concurrency} at a time", dataset.len());
    // Not following redirects keeps the System One key on the host it was set for.
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().context("cannot build the HTTP client")?;
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().context("cannot start the async runtime")?;
    let asked = runtime.block_on(ask::ask_panel(&http, &endpoint, dataset, args.horizon_days, concurrency));
    if let Some(error) = &asked.first_error {
        if asked.failed == dataset.len() {
            anyhow::bail!("every request to {origin} failed; the first: {error}");
        }
        eprintln!("[flinch-fit] {} of {} request(s) failed and stay unanswered; the first: {error}", asked.failed, dataset.len());
    }
    let model = asked.model().or(endpoint.model.as_deref()).unwrap_or("unknown").to_string();
    let result = bench::head_to_head(dataset, &Predictions::answered(dataset, &asked.responses), now, args.horizon_days);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        print_head_to_head(&result, &format!("{model} @ {origin}"));
    }
    if args.write {
        let path = args.state_dir.join(bench::BENCHMARK_FILE);
        let benchmark = Benchmark { model, endpoint: origin, scored_at_unix: unix_now(), result };
        persist::replace(&path, &serde_json::to_vec_pretty(&benchmark)?).with_context(|| format!("cannot write {}", path.display()))?;
        eprintln!("[flinch-fit] wrote {}", path.display());
    }
    Ok(())
}

/// `external` names the model the table's first row scores: a predictions
/// file, or `model @ scheme://host[:port]`.
fn print_head_to_head(result: &HeadToHead, external: &str) {
    let join = &result.join;
    println!(
        "[flinch-fit] head-to-head · external {external} · panel now {} · horizon {:.0} d · label 1 = nothing played it within the horizon",
        result.now_unix, result.horizon_days
    );
    println!(
        "[flinch-fit] predictions: {} line(s) · {} joined · {} unjoined · {} stale · {} duplicate · {} invalid{} · {} of {} panel row(s) unanswered",
        join.prediction_lines,
        join.joined,
        join.unjoined,
        join.stale,
        join.duplicates,
        join.invalid,
        if join.invalid_lines.is_empty() { String::new() } else { format!(" (lines {:?})", join.invalid_lines) },
        join.unanswered,
        join.panel_rows,
    );
    if join.joined == 0 {
        if join.prediction_lines == 0 {
            println!("[flinch-fit] nothing answered: no prediction to score");
        } else {
            println!("[flinch-fit] nothing joined: were the answers made for a panel with a different --now or --horizon-days?");
        }
        return;
    }
    let base_rate = result.external.positives as f32 / result.external.n as f32;
    println!("[flinch-fit] scored on the same {} row(s) · base rate {base_rate:.3}", result.external.n);
    println!("    {:34} {:>7} {:>8} {:>9} {:>7}", "model", "AUC", "Brier", "log-loss", "ECE");
    let priors = format!("FLINCH priors (T={DEPLOYED_PRIOR_TEMPERATURE:.2})");
    let rows = [
        ("external", &result.external),
        (priors.as_str(), &result.priors),
        ("FLINCH recalibrated (out-of-fold)", &result.recalibrated),
        ("FLINCH full fit (out-of-fold)", &result.fitted),
    ];
    for (name, card) in rows {
        println!("    {name:34} {:7.4} {:8.4} {:9.4} {:7.4}", card.auc, card.brier, card.log_loss, card.ece);
    }
    println!("[flinch-fit] FLINCH minus external, 95% over resampled titles:");
    println!("    recalibrated  {}", bench::describe(&result.versus.recalibrated));
    println!("    full fit      {}", bench::describe(&result.versus.fitted));
    println!("[flinch-fit] AUC ranks eviction: over the ceiling the lowest (1 − P(safe)) per byte goes first. Brier, log-loss and ECE judge the probabilities; lower is better.");
}

fn fit_report(args: &Args, household: &Household, dataset: &[Example], now: u64, cut_count: usize) -> anyhow::Result<()> {
    let model = adopt::fit_model(household, dataset, now, args.horizon_days, cut_count);
    let priors = ScoreWeights::default();
    let priors_all = fit::probabilities(dataset, &priors, DEPLOYED_PRIOR_TEMPERATURE);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&model)?);
    } else {
        print_misses(dataset, &priors_all);
        let prior_forecasts = fit::forecasts(dataset, &priors, DEPLOYED_PRIOR_TEMPERATURE);
        print_report(household, dataset, &model, (&priors_all, &prior_forecasts));
    }
    if args.write {
        let path = args.state_dir.join("weights.json");
        match adopt::adopt(&args.state_dir, &model)? {
            Adoption::Written => println!("[flinch-fit] wrote {}", path.display()),
            Adoption::Removed => println!("[flinch-fit] removed a previously written {}", path.display()),
            Adoption::PriorsKept => {}
        }
    }
    Ok(())
}

/// Error analysis: every panel row the deployed model would have flagged that
/// the household then played. Naming these is how a guard earns or loses trust
/// — a refit cannot be justified until we know what it would be fixing.
fn print_misses(dataset: &[Example], priors_all: &[f32]) {
    let priors = ScoreWeights::default();
    let misses: Vec<(&Example, f32)> = dataset
        .iter()
        .zip(priors_all)
        .filter(|(example, p)| **p >= FLOOR && example.label < 0.5)
        .map(|(example, p)| (example, *p))
        .collect();
    if misses.is_empty() {
        return;
    }
    println!("[flinch-fit] the deployed model would have flagged {} item(s) that were played afterwards:", misses.len());
    for (example, probability) in misses.iter().take(8) {
        // Which signals carried the score for this row.
        let mut contributions: Vec<(&str, f32)> = example
            .values
            .iter()
            .map(|(name, value)| (*name, priors.get(name) * value))
            .filter(|(_, contribution)| contribution.abs() > 0.05)
            .collect();
        contributions.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()));
        let why: Vec<String> = contributions.iter().take(3).map(|(name, contribution)| format!("{name} {contribution:+.2}")).collect();
        println!("    {} · P(safe) {:.0}% · {}", example.card.title, probability * 100.0, why.join(", "));
    }
}

/// `scores` pairs the gated score the floor acts on with the forecast the
/// accuracy metrics judge, row for row.
fn print_report(household: &Household, dataset: &[Example], model: &fit::FittedModel, scores: (&[f32], &[f32])) {
    let (priors_all, prior_forecasts) = scores;
    let metrics = &model.metrics;
    let priors = ScoreWeights::default();
    println!("[flinch-fit] {}", model.fitted_on);
    if household.unreadable_rows > 0 {
        println!("[flinch-fit] {} items.json row(s) were not a readable library item and were left out", household.unreadable_rows);
    }
    println!(
        "[flinch-fit] panel: {} examples, every one judged out of fold, {} positives, horizon {:.0} d",
        metrics.examples, metrics.positives, metrics.horizon_days
    );
    // Both columns in effective units (weight ÷ temperature), the scale each
    // model actually runs at: a recalibration changes the temperature, not the
    // weights, and must not read as "every weight moved".
    let (weights, temperature) = (model.weights(), model.temperature);
    println!("[flinch-fit] {} · weights at the scale they run (deployed prior → this model):", model.kind.label());
    for name in ScoreWeights::names() {
        if ScoreWeights::frozen().contains(name) {
            println!("    {name:18} {:7.2} (frozen policy, not fitted)", priors.get(name));
            continue;
        }
        let prior = priors.get(name) / DEPLOYED_PRIOR_TEMPERATURE;
        println!("    {name:18} {prior:7.2} → {:7.2}", weights.get(name) / temperature);
    }
    println!("[flinch-fit] bias {:.2} · temperature {:.2} (prior model: {DEPLOYED_PRIOR_TEMPERATURE:.2})", model.bias, model.temperature);
    println!(
        "[flinch-fit] out-of-fold forecasts: AUC {:.3} (priors {:.3}) · Brier {:.4} (priors {:.4}) · ECE {:.3} (priors {:.3})",
        metrics.auc, metrics.priors_auc, metrics.brier, metrics.priors_brier, metrics.ece, metrics.priors_ece
    );
    println!("[flinch-fit] at floor {FLOOR:.2}: {} flagged, precision {:.2}", metrics.flagged_at_floor, metrics.precision_at_floor);
    println!(
        "[flinch-fit] deployed model through the floor {FLOOR:.2}: {} of {} panel rows flagged, {} of those were played afterwards",
        metrics.priors_flagged, metrics.examples, metrics.priors_flagged_then_played
    );
    print_risk(dataset, priors_all, metrics.priors_flagged_then_played);
    print_buckets(dataset, priors_all, prior_forecasts);
    println!(
        "[flinch-fit] outcome signal: {} negative row(s) from {} distinct item(s)",
        metrics.examples - metrics.positives,
        metrics.negative_items
    );
    let adoption = match model.shortfall() {
        None => format!("adopting the {}, better than the hand-set priors out of fold", model.kind.label()),
        Some(shortfall) => format!("KEEPING PRIORS — {shortfall}"),
    };
    println!("[flinch-fit] adoption: {adoption}");
}

/// The floor with a guarantee behind it, for the deployed model: with
/// probability ≥ 90%, the share of items reclaimed and then played stays under
/// α. Rows repeat items across cut dates, so exchangeability is approximate and
/// the bound is indicative rather than exact.
fn print_risk(dataset: &[Example], priors_all: &[f32], current_false: usize) {
    println!(
        "[flinch-fit] risk at the current floor {FLOOR:.2}: {current_false} false reclaim(s) in {} rows · 90% upper bound {:.1}%",
        dataset.len(),
        eval::binomial_upper_bound(current_false, dataset.len(), 0.1) * 100.0
    );
    let labels: Vec<f32> = dataset.iter().map(|example| example.label).collect();
    for alpha in [0.01f64, 0.02, 0.05] {
        match eval::certified_floor(priors_all, &labels, alpha, 0.1) {
            Some(certified) => println!(
                "    certified for α={:.0}%: floor ≥ {:.2} ({} flagged, {} false, bound {:.1}%)",
                alpha * 100.0,
                certified.floor,
                certified.flagged,
                certified.false_reclaims,
                certified.upper_bound * 100.0
            ),
            None => println!(
                "    certified for α={:.0}%: not yet — needs ≥ {} rows even with zero errors",
                alpha * 100.0,
                ((1.0f64 / 0.1).ln() / alpha).ceil() as usize
            ),
        }
    }
}

/// Where is the scorecard blind, and is that where the misses are? A model that
/// reads titles or synopses can only help in the bucket with no behavioural
/// evidence; if the misses sit elsewhere it would add nothing. Flags are what
/// the floor would act on; AUC ranks the forecasts.
fn print_buckets(dataset: &[Example], priors_all: &[f32], prior_forecasts: &[f32]) {
    println!("[flinch-fit] bucket analysis (where a text/taste prior could add anything):");
    for (name, has_evidence) in [("watch evidence", true), ("blind (none)", false)] {
        let rows: Vec<usize> = (0..dataset.len())
            .filter(|row| {
                let values = &dataset[*row].values;
                (values.contains_key("never_played") || values.contains_key("partially_played")) == has_evidence
            })
            .collect();
        let scores: Vec<f32> = rows.iter().map(|row| priors_all[*row]).collect();
        let forecasts: Vec<f32> = rows.iter().map(|row| prior_forecasts[*row]).collect();
        let labels: Vec<f32> = rows.iter().map(|row| dataset[*row].label).collect();
        let played = labels.iter().filter(|label| **label < 0.5).count();
        let flagged = scores.iter().filter(|score| **score >= FLOOR).count();
        let flagged_played = scores.iter().zip(&labels).filter(|(score, label)| **score >= FLOOR && **label < 0.5).count();
        println!(
            "    {name:16} rows {:4} · played afterwards {played:3} · flagged {flagged:3} · of those played {flagged_played:2} · AUC {:.2}",
            rows.len(),
            eval::auc(&forecasts, &labels)
        );
    }
}
