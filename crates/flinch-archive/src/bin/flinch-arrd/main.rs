//! The daemon: inventory -> forecast -> plan -> hand-off, on a tick.
//!
//! Run this as a long-lived process. Each cycle pulls the *arr libraries,
//! merges media-server watch state, forecasts every library volume, plans the
//! least-regret evictions that fit each forecast, and writes the plan to
//! `state/eviction-plan.json`. It hands the plan to Maintainerr only when the
//! planner's `dry_run` is off, and never deletes anything itself.

use anyhow::{Context, Result};
use clap::Parser;
use flinch_archive::daemon::{reconcile, NeverPlayedHold};
use flinch_archive::maintainerr::{self as mx, HttpMaintainerr, OwnedState, SyncItem};
use flinch_archive::plan::{candidates, Exclusion, Manifest};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

mod evidence;
mod fetch;
mod handoff;
mod history;
mod media;
mod model;
mod publish;
mod signals;
mod sink;
mod snapshot;
mod storage;

use fetch::{fetch_disks, fetch_inventory, Fetched};
use sink::Sink;

#[derive(Parser, Debug)]
#[command(name = "flinch-arrd", about = "Continuous *arr swarm loop")]
struct Args {
    #[arg(long, default_value = "http://radarr:7878")]
    radarr_url: String,
    #[arg(long, default_value = "")]
    radarr_key: String,
    #[arg(long, default_value = "http://sonarr:8989")]
    sonarr_url: String,
    #[arg(long, default_value = "")]
    sonarr_key: String,
    #[arg(long, default_value = "http://maintainerr:5555")]
    maintainerr_url: String,
    #[arg(long, default_value = "")]
    maintainerr_key: String,
    /// Seerr (Overseerr/Jellyseerr): requests and watchlists. Empty disables.
    #[arg(long, default_value = "")]
    seerr_url: String,
    #[arg(long, default_value = "")]
    seerr_key: String,
    /// Prowlarr: seeders per title, for how hard a re-download would be.
    #[arg(long, default_value = "")]
    prowlarr_url: String,
    #[arg(long, default_value = "")]
    prowlarr_key: String,
    /// SABnzbd: the usenet servers' retention.
    #[arg(long, default_value = "")]
    sabnzbd_url: String,
    #[arg(long, default_value = "")]
    sabnzbd_key: String,
    /// JSON export of media-server watch state (see examples/watch-state.json).
    #[arg(long)]
    watch_state: Option<PathBuf>,
    /// Run once and exit instead of looping.
    #[arg(long)]
    once: bool,
    // Grace runs, per-run caps and collections are operator settings
    // (state/settings.json, set from the UI); they have no flags.
    #[arg(long, default_value_t = 3600)]
    interval_s: u64,
}

/// Flag value unless an environment variable sets it. Environment wins when the
/// flag would; for a container the flags are the defaults and env is the knob.
fn env_or(env: &str, flag: String) -> String {
    match std::env::var(env) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => flag,
    }
}

fn resolve(args: &mut Args) {
    args.radarr_url = env_or("RADARR_URL", args.radarr_url.clone());
    args.sonarr_url = env_or("SONARR_URL", args.sonarr_url.clone());
    args.maintainerr_url = env_or("MAINTAINERR_URL", args.maintainerr_url.clone());
    args.seerr_url = env_or("SEERR_URL", args.seerr_url.clone());
    args.prowlarr_url = env_or("PROWLARR_URL", args.prowlarr_url.clone());
    args.sabnzbd_url = env_or("SABNZBD_URL", args.sabnzbd_url.clone());
    for (key, env) in [
        (&mut args.radarr_key, "RADARR_API_KEY"),
        (&mut args.sonarr_key, "SONARR_API_KEY"),
        (&mut args.maintainerr_key, "MAINTAINERR_API_KEY"),
        (&mut args.seerr_key, "SEERR_API_KEY"),
        (&mut args.prowlarr_key, "PROWLARR_API_KEY"),
        (&mut args.sabnzbd_key, "SABNZBD_API_KEY"),
    ] {
        if key.is_empty() {
            *key = env_or(env, String::new());
        }
    }
    if let Ok(value) = std::env::var("FLINCH_WATCH_STATE") {
        if !value.is_empty() {
            args.watch_state = Some(PathBuf::from(value));
        }
    }
    if let Ok(value) = std::env::var("FLINCH_INTERVAL_S") {
        if let Ok(seconds) = value.parse() {
            args.interval_s = seconds;
        }
    }
    if std::env::var("FLINCH_ONCE").map(|v| v == "1" || v == "true").unwrap_or(false) {
        args.once = true;
    }
}

fn state_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("FLINCH_STATE_DIR").unwrap_or_else(|_| "state".to_string()))
}

/// A missing state file is a first run; an unreadable one starts over.
pub(crate) fn read_state<T: serde::de::DeserializeOwned + Default>(path: &std::path::Path) -> T {
    let parsed = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return T::default(),
        Err(error) => Err(error.to_string()),
    };
    parsed.unwrap_or_else(|error| {
        eprintln!("[flinch-arrd] {} unreadable, starting it over: {error}", path.display());
        T::default()
    })
}

pub(crate) fn write_state<T: serde::Serialize>(path: &std::path::Path, value: &T) {
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| serde_json::to_vec(value).map_err(std::io::Error::from))
        .and_then(|bytes| flinch_archive::persist::replace(path, &bytes));
    if let Err(error) = written {
        eprintln!("[flinch-arrd] {} write failed, it is read again next cycle: {error}", path.display());
    }
}

/// The shortest gap between the starts of two cycles when run.now asks for
/// one. The UI's "run now" must not become a way to spin cycles back to back.
const MIN_TRIGGER_GAP: Duration = Duration::from_secs(60);

fn main() -> Result<()> {
    let mut args = Args::parse();
    resolve(&mut args);
    // Maintainerr checks no key (MX-09); one is only sent when set.
    if args.radarr_key.is_empty() || args.sonarr_key.is_empty() {
        anyhow::bail!("Radarr and Sonarr api keys are required (flags or *_API_KEY env)");
    }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(run(&args))
}

async fn run(args: &Args) -> Result<()> {
    // Redirects are never followed: every request carries an API key or the
    // Plex token in a header, and a redirect would hand it to another host. A
    // 3xx then surfaces as an error naming the status.
    let http = reqwest::Client::builder().timeout(Duration::from_secs(30)).redirect(reqwest::redirect::Policy::none()).build()?;

    // A settings file that stops parsing must not revert every operator choice
    // to its default mid-flight: the last good copy stays in force, loudly.
    let mut last_good = flinch_archive::daemon::RuntimeSettings::default();
    loop {
        // Operator settings win over env: the UI writes state/settings.json and
        // each cycle picks it up, so no redeploy is needed to change cadence,
        // grace, caps, collections or Plex credentials.
        let settings = match flinch_archive::daemon::read_settings(state_dir().join("settings.json").as_path()) {
            Ok(settings) => {
                last_good = settings.clone();
                settings
            }
            Err(error) => {
                eprintln!("[flinch-arrd] {error} — keeping the last good settings");
                last_good.clone()
            }
        };
        let interval_s = if settings.interval_s > 0 { settings.interval_s } else { args.interval_s };
        let interval = Duration::from_secs(interval_s.max(60));
        let trigger = state_dir().join("run.now");
        if trigger.exists() {
            std::fs::remove_file(&trigger).ok();
            println!("[flinch-arrd] manual run requested via run.now");
        }
        // A failed cycle is logged and published, never fatal: one transient
        // *arr or Plex error must not crash-loop the daemon and stale the UI.
        let started = tokio::time::Instant::now();
        let result = cycle(args, &http, &settings, interval_s).await;
        if let Err(error) = &result {
            eprintln!("[flinch-arrd] cycle failed: {error:#}");
            flinch_archive::daemon::record_cycle_error(state_dir().join("status.json").as_path(), &format!("{error:#}"));
        }
        if args.once {
            // A one-shot probe (CronJob) reports failure through its exit code.
            return result;
        }
        // Short sleeps so a manual trigger is noticed promptly without
        // busy-waiting. A trigger sooner than MIN_TRIGGER_GAP after this cycle
        // started waits for it (the file stays, so the request is kept).
        let step = std::time::Duration::from_secs(5);
        let mut slept = std::time::Duration::ZERO;
        while slept < interval {
            if trigger.exists() && started.elapsed() >= MIN_TRIGGER_GAP {
                break;
            }
            tokio::time::sleep(step).await;
            slept += step;
        }
    }
}

/// One full pass: inventory → evidence → forecast → plan → hand-off → publish.
async fn cycle(args: &Args, http: &reqwest::Client, settings: &flinch_archive::daemon::RuntimeSettings, interval_s: u64) -> Result<()> {
    let Fetched { movies, series, removals } = fetch_inventory(http, args, &settings.keep_tag).await?;
    let disks = fetch_disks(http, args).await;

    // Cards first: the same set decides and displays, and Plex watch state
    // must be merged in BEFORE anything decides or the guard stays blind.
    let mut cards: Vec<flinch_archive::ArchiveCard> =
        movies.iter().filter_map(|movie| movie.to_card()).chain(series.iter().flat_map(|show| show.to_cards())).collect();

    let cycle_now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let evidence::Evidence {
        watch,
        watch_targets,
        resolution,
        health,
        plex_history_rows,
        tautulli_rows,
        plex_ids,
        play_keys,
        plex_keeps,
        plex_listed,
    } = evidence::gather(args, http, settings, &movies, &series, &cards, cycle_now).await?;

    // Dry run unless the planner says otherwise; FLINCH_DRY_RUN forces one.
    let forced_dry = std::env::var("FLINCH_DRY_RUN").map(|v| v == "1" || v == "true").unwrap_or(false);
    let dry_run = settings.planner.dry_run || forced_dry;
    // --- Maintainerr, read first: an operator's exclusion pins its item. A dry
    // run reads the live state and prints every write it would send. ---
    let maintainerr = HttpMaintainerr::new(&args.maintainerr_url, &args.maintainerr_key)?;
    let mut api = if dry_run { Sink::DryRun(maintainerr) } else { Sink::Live(maintainerr) };
    let titles = settings.collection_titles();
    let mut owned = OwnedState::read(&state_dir());
    // Plex ids come only from the GUID join; a card without them is never
    // protected or scheduled.
    let sync_items: Vec<SyncItem> = cards
        .iter()
        .map(|card| SyncItem {
            card_id: card.id.clone(),
            kind: card.kind,
            plex: plex_ids.get(&card.id).cloned(),
            // Every copy the GUID join merged, so the hand-off can refuse an
            // item it cannot pin to the copy it judged.
            copies: play_keys
                .get(&card.id)
                .filter(|_| plex_ids.contains_key(&card.id))
                .map(|keys| keys.item_keys().to_vec())
                .unwrap_or_default(),
            bytes: card.size_bytes,
        })
        .collect();
    let observed = mx::observe(&mut api, &sync_items, &titles, &owned).await;
    if let Err(error) = &observed {
        eprintln!("[flinch-arrd] maintainerr unreadable, nothing is synced this cycle: {error}");
    }
    // Where each titled collection leads (route, window), for the item view.
    let destinations = observed.as_ref().map(|observed| mx::destinations(&observed.collections, &titles)).unwrap_or_default();
    // Unreadable Maintainerr: the keeps last observed stand in, so an outage
    // never turns an operator's keeper into a candidate.
    let operator_keeps = match &observed {
        Ok(observed) => {
            let keeps = mx::operator_keeps(&sync_items, observed, &owned);
            if let Err(error) = mx::write_operator_keeps(&state_dir(), &keeps) {
                eprintln!("[flinch-arrd] operator-keeps.json write failed: {error}");
            }
            keeps
        }
        Err(_) => mx::read_operator_keeps(&state_dir()),
    };
    // Every keep the operator set counts alike: their own Maintainerr
    // exclusions, and the keep tag as a Plex label or collection.
    let keeps: std::collections::BTreeSet<String> = operator_keeps.union(&plex_keeps).cloned().collect();
    flinch_archive::daemon::guard_operator_keeps(&mut cards, &keeps);
    flinch_archive::watch::apply(&mut cards, &watch);
    let play_log = flinch_archive::fit::plays::PlayLog::new(&plex_history_rows, &tautulli_rows);
    let joins: HashMap<&str, flinch_archive::plex::PlayJoin> =
        watch_targets.iter().map(|target| (target.id.as_str(), resolution.join(target))).collect();
    // P(watch) runs on the daily fit when one beat the priors, else the priors.
    let (hazard, model_label) = model::hazard(&state_dir(), cycle_now);

    // XC-03: deleting on the strength of *absent* plays needs every watch
    // source read completely, and a Leaving Soon collection to announce in.
    let never_played_hold = NeverPlayedHold::of(&health, &titles);
    let never_played = if !settings.unwatched_reclaim_enabled {
        Some(Exclusion::NeverPlayedOff)
    } else {
        never_played_hold.map(Exclusion::NeverPlayedHeld)
    };
    if let (true, Some(hold)) = (settings.unwatched_reclaim_enabled, never_played_hold) {
        eprintln!("[flinch-arrd] never-played reclaim held off this cycle {}", hold.until());
    }

    let signals = signals::gather(http, args, &cards, cycle_now).await;
    for problem in &signals.problems {
        eprintln!("[flinch-arrd] signals: {problem}");
    }
    let (governance, mut ledger) = storage::govern(&disks, (&movies, &series), &cards, &signals, &settings.capacity, cycle_now);

    let plays: HashMap<String, candidates::Plays> = cards
        .iter()
        .filter_map(|card| {
            let join = joins.get(card.id.as_str())?;
            Some((card.id.clone(), (play_log.item_plays(join), play_log.audience_plays(join))))
        })
        .collect();
    let in_plex: HashSet<String> = plex_ids.keys().cloned().collect();
    let handed: HashSet<String> = owned.scheduled.keys().cloned().collect();
    let candidates = candidates::build(
        &candidates::Library {
            cards: &cards,
            movies: &movies,
            series: &series,
            watch: &watch,
            plays: &plays,
            located: &governance.located,
            in_plex: &in_plex,
            handed: &handed,
            signals: &signals,
            never_played,
            now: cycle_now,
        },
        &settings.planner,
        &hazard,
    );
    let report = reconcile(&candidates, &governance.forecasts, &settings.planner).context("planning")?;
    let plan = &report.plan;
    let gib = |bytes: u64| bytes as f64 / 1_073_741_824.0;
    println!(
        "[flinch-arrd] plan: {} item(s), {:.1} of {:.1} GiB needed, regret {:.2}{}",
        plan.items.len(),
        gib(plan.total_reclaimed_bytes),
        gib(plan.target_bytes),
        plan.total_regret,
        plan.method.map_or(" (healthy: solver skipped)".to_string(), |method| format!(" ({method:?})")),
    );
    if let Some(error) = &plan.solver_error {
        eprintln!("[flinch-arrd] HiGHS failed, plan made greedily: {error}");
    }
    std::fs::create_dir_all(state_dir()).ok();
    let manifest = serde_json::to_vec_pretty(&Manifest::new(plan, &governance.forecasts, dry_run, cycle_now))?;
    flinch_archive::persist::replace(&state_dir().join("eviction-plan.json"), &manifest)?;

    // --- grace streaks, then the hand-off ---
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // A cycle that cannot read Maintainerr is not an appearance: it can hand
    // nothing over and planned without the operator's exclusions, so counting
    // it would let an outage run down an item's grace window.
    let eligible = if observed.is_ok() {
        let candidates_path = state_dir().join("candidates.json");
        let mut cstate = flinch_archive::daemon::read_candidate_state(&candidates_path);
        let grace = flinch_archive::daemon::Grace { runs: settings.grace_runs, interval_s };
        let past_grace = flinch_archive::daemon::advance_streaks(&mut cstate, &report.deleted_ids, grace, now);
        flinch_archive::daemon::write_candidate_state(&candidates_path, &cstate)?;
        plan.releasable(&past_grace, &handed)
    } else {
        Vec::new()
    };
    if dry_run {
        println!("[flinch-arrd] dry run: plan written, nothing handed to Maintainerr");
    }

    let by_id: HashMap<&str, &SyncItem> = sync_items.iter().map(|item| (item.card_id.as_str(), item)).collect();
    let names: HashMap<&str, &str> = cards.iter().map(|card| (card.id.as_str(), card.title.as_str())).collect();
    let handoff = handoff::Handoff {
        items: &by_id,
        names: &names,
        report: &report,
        eligible: &eligible,
        titles: &titles,
        caps: mx::Caps::new(settings.max_items, settings.max_gib),
        enforcing: !dry_run,
        now,
        plex_listed: plex_listed.as_ref(),
        seerr: fetch::maintainerr_seerr_configured(http, args).await,
    };
    let sync = handoff::sync(handoff, observed, &mut api, &mut owned, &governance, &mut ledger, operator_keeps.len()).await;
    for problem in sync.problems.iter().chain(&sync.warnings) {
        eprintln!("[flinch-arrd] maintainerr: {problem}");
    }

    let items = snapshot::build_items(snapshot::ItemInputs {
        cards: &cards,
        candidates: &candidates,
        movies: &movies,
        series: &series,
        governance: &governance,
        owned: &owned,
        titles: &titles,
        destinations: &destinations,
        watch: &watch,
        report: &report,
        plex_ids: &plex_ids,
        play_keys: &play_keys,
    });
    if let Err(error) = ledger.write(&storage::ledger_path()) {
        eprintln!("[flinch-arrd] evictions.json write failed: {error}");
    }

    let (shadow_count, shadow_bytes) = flinch_archive::plan::never_played_preview(&candidates);
    publish::publish(
        publish::Run {
            report: &report,
            sync,
            governance: &governance,
            handed: owned.scheduled.keys().filter_map(|id| by_id.get(id.as_str()).map(|item| (id.as_str(), item.bytes))).collect(),
            dry_run,
            interval_s: (!args.once).then_some(interval_s),
            model: model_label,
            shadow: (shadow_count as u64, gib(shadow_bytes) as f32),
            health,
            never_played_hold,
            never_played_requested: settings.unwatched_reclaim_enabled,
            outside: history::outside(&removals, &ledger, (&movies, &series), now),
        },
        &items,
    )
}
