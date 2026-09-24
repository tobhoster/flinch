//! The swarm daemon: inventory -> cards -> plan -> protect, on a tick.
//!
//! Run this as a long-lived process (container/CronJob in a homelab). Each
//! cycle pulls the *arr libraries, merges media-server watch state, decides
//! what is safe to reclaim, and adds Maintainerr exclusions for everything it
//! keeps. It never deletes; the plan file is the operator-facing output.

use anyhow::Result;
use clap::Parser;
use flinch_archive::daemon::reconcile;
use flinch_archive::maintainerr::{self as mx, HttpMaintainerr, OwnedState, SyncItem};
use flinch_archive::capacity::CapacityAction;
use flinch_archive::ArchivePolicy;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

mod evidence;
mod fetch;
mod handoff;
mod history;
mod media;
mod model;
mod publish;
mod sink;
mod snapshot;
mod storage;
mod taste;

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
    /// JSON export of media-server watch state (see examples/watch-state.json).
    #[arg(long)]
    watch_state: Option<PathBuf>,
    /// Run once and exit instead of looping.
    #[arg(long)]
    once: bool,
    /// Hand schedule-eligible candidates to Maintainerr (deletion happens on
    /// Maintainerr's own schedule). Default OFF: flip on after one verified run.
    #[arg(long)]
    enforce: bool,
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
    if args.radarr_key.is_empty() {
        args.radarr_key = env_or("RADARR_API_KEY", String::new());
    }
    if args.sonarr_key.is_empty() {
        args.sonarr_key = env_or("SONARR_API_KEY", String::new());
    }
    if args.maintainerr_key.is_empty() {
        args.maintainerr_key = env_or("MAINTAINERR_API_KEY", String::new());
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
    if std::env::var("FLINCH_ENFORCE").map(|v| v == "1" || v == "true").unwrap_or(false) {
        args.enforce = true;
    }
}

fn state_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("FLINCH_STATE_DIR").unwrap_or_else(|_| "state".to_string()))
}

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
    let http = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;

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
        let result = cycle(args, &http, &settings, interval_s).await;
        if let Err(error) = &result {
            eprintln!("[flinch-arrd] cycle failed: {error:#}");
            flinch_archive::daemon::record_cycle_error(state_dir().join("status.json").as_path(), &format!("{error:#}"));
        }
        if args.once {
            // A one-shot probe (CronJob) reports failure through its exit code.
            return result;
        }
        // Short sleeps so a manual trigger is noticed promptly without busy-waiting.
        let step = std::time::Duration::from_secs(5);
        let mut slept = std::time::Duration::ZERO;
        while slept < interval {
            if state_dir().join("run.now").exists() {
                break;
            }
            tokio::time::sleep(step).await;
            slept += step;
        }
    }
}

/// One full pass: inventory → evidence → score → govern → plan → publish.
async fn cycle(
    args: &Args,
    http: &reqwest::Client,
    settings: &flinch_archive::daemon::RuntimeSettings,
    interval_s: u64,
) -> Result<()> {
    let grace_runs = settings.grace_runs;
    let max_items = settings.max_items;
    let max_gib = settings.max_gib;
    let enforce = settings.enforce || args.enforce;
    let Fetched { movies, series, removals } = fetch_inventory(&http, args, &settings.keep_tag).await?;
    let disks = fetch_disks(&http, args).await;

    // Cards first: the same set decides and displays, and Plex watch state
    // must be merged in BEFORE the policy runs or the guard stays blind.
    let mut cards: Vec<flinch_archive::ArchiveCard> =
        movies.iter().filter_map(|movie| movie.to_card()).chain(series.iter().flat_map(|show| show.to_cards())).collect();

    let cycle_now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
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

    // --- Maintainerr, read first: an operator's exclusion is a hard keep guard
    // for the score and the plan alike (XC-02). ---
    let dry_run = std::env::var("FLINCH_DRY_RUN").map(|v| v == "1" || v == "true").unwrap_or(false);
    // Enforcement off (or FLINCH_DRY_RUN) means no write reaches Maintainerr: the
    // dry-run sink reads the live state and prints every write it would send.
    let enforcing = enforce && !dry_run;
    let maintainerr = HttpMaintainerr::new(&args.maintainerr_url, &args.maintainerr_key)?;
    let mut api = if enforcing { Sink::Live(maintainerr) } else { Sink::DryRun(maintainerr) };
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
    // Merge watch state into the cards the score sees. Without this the
    // score would read every item as "no evidence" while the snapshot showed
    // a match: the display and the decision would disagree again.
    flinch_archive::watch::apply(&mut cards, &watch);
    let play_log = flinch_archive::fit::plays::PlayLog::new(&plex_history_rows, &tautulli_rows);
    let joins: HashMap<&str, flinch_archive::plex::PlayJoin> =
        watch_targets.iter().map(|target| (target.id.as_str(), resolution.join(target))).collect();
    let ended_by_show: HashMap<&str, bool> = series
        .iter()
        .map(|s| {
            let ended = flinch_archive::score::series_ended_as_of(s.status.as_deref(), s.last_aired_epoch(), cycle_now);
            (s.title.as_str(), ended)
        })
        .collect();

    let activity = flinch_archive::score::ShowActivity::new(&cards, &watch);
    let settings_for_score = &settings;
    let scoring = model::Scoring { cards: &cards, watch: &watch, activity: &activity, play_log: &play_log, joins: &joins, ended_by_show: &ended_by_show, movies: &movies, series: &series, now: cycle_now };
    let (scored, model_label) = model::score_cycle(settings, &state_dir(), scoring);

    let verdicts: HashMap<String, flinch_archive::policy::ScoreVerdict> = cards
        .iter()
        .zip(scored.iter())
        .map(|(card, score)| {
            (
                card.id.clone(),
                flinch_archive::policy::ScoreVerdict {
                    p_safe: score.p_safe,
                    hard_guard: score.hard_guard.is_some(),
                    // Another season of the same show is being watched: the
                    // household picks shows up as a whole, so an unplayed
                    // season of an active show is catch-up queue, not litter.
                    sibling_played: {
                        let siblings = activity.siblings(card, &watch);
                        siblings.played || siblings.completed
                    },
                },
            )
        })
        .collect();

    let mut policy = ArchivePolicy::default();
    policy.unwatched_reclaim = flinch_archive::policy::UnwatchedReclaim {
        enabled: settings_for_score.unwatched_reclaim_enabled,
        floor: settings_for_score.unwatched_reclaim_floor,
        min_dwell_days: settings_for_score.unwatched_reclaim_dwell_days,
    };
    policy.score_floor = settings_for_score.score_floor;

    // XC-03: a partly read watch record never arms never-played reclaim — not
    // by the operator's switch, and not by capacity pressure. Deleting on the
    // strength of *absent* plays needs every source read completely.
    let never_played_safe = health.never_played_reclaim_safe();
    if !never_played_safe && policy.unwatched_reclaim.enabled {
        eprintln!("[flinch-arrd] never-played reclaim held off this cycle: {}", health.problems().join("; "));
    }
    policy.unwatched_reclaim.enabled &= never_played_safe;
    let governing = flinch_archive::daemon::RuntimeSettings {
        capacity_arm_never_played: settings_for_score.capacity_arm_never_played && never_played_safe,
        ..settings.clone()
    };

    // Capacity: measure the library volumes, decide per volume, set the goal;
    // what Maintainerr already holds is taken first, so no window restarts.
    let (mut governance, mut ledger) =
        storage::govern(&disks, (&movies, &series), &cards, &plex_ids, &governing, &mut policy, cycle_now);
    governance.take_handed_first(owned.scheduled.keys().cloned().collect());
    // Never-played reclaim the settings (or disk pressure) would run now, held
    // only because the watch evidence is incomplete: the items must say so.
    let never_played_held = !never_played_safe
        && (settings_for_score.unwatched_reclaim_enabled
            || (settings_for_score.capacity_arm_never_played
                && matches!(governance.decision.action, CapacityAction::Evict { .. })));

    // What arming the never-played rule would add. Computed in the library
    // (tested), by card id — never by pairing iteration orders.
    let (shadow_count, shadow_bytes) = flinch_archive::shadow::preview(&cards, &verdicts, &policy);
    let shadow_gib = shadow_bytes as f32 / 1_073_741_824.0;
    println!(
        "[flinch-arrd] never-played preview: {shadow_count} item(s) / {shadow_gib:.1} GiB would additionally qualify if armed (floor {:.2}, dwell {:.0} d)",
        policy.unwatched_reclaim.floor, policy.unwatched_reclaim.min_dwell_days
    );

    let report = reconcile(&movies, &series, &watch, &keeps, &policy, &verdicts, &governance.goal);
    std::fs::create_dir_all(state_dir()).ok();

    // --- automation: grace window, caps, then hand off to Maintainerr ---
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // A cycle that cannot read Maintainerr is not an appearance: it can hand
    // nothing over and planned without the operator's exclusions, so counting
    // it would let an outage run down an item's grace window.
    let eligible = if observed.is_ok() {
        let candidates_path = state_dir().join("candidates.json");
        let mut cstate = flinch_archive::daemon::read_candidate_state(&candidates_path);
        let eligible = flinch_archive::daemon::advance_streaks(&mut cstate, &report.deleted_ids, grace_runs, now);
        flinch_archive::daemon::write_candidate_state(&candidates_path, &cstate)?;
        eligible
    } else {
        Vec::new()
    };

    println!(
        "[flinch-arrd] {} item(s) selected from {:.1} GiB eligible (policy ∧ P(safe) floor {:.2})",
        report.deleted_ids.len(),
        report.eligible_bytes as f64 / 1_073_741_824.0,
        settings_for_score.score_floor
    );

    // --- Maintainerr sync: keeps become exclusions, evictions past the grace
    // window become collection members, least regret first. ---
    let by_id: HashMap<&str, &SyncItem> = sync_items.iter().map(|item| (item.card_id.as_str(), item)).collect();
    let names: HashMap<&str, &str> = cards.iter().map(|card| (card.id.as_str(), card.title.as_str())).collect();
    let handoff = handoff::Handoff {
        items: &by_id,
        names: &names,
        report: &report,
        eligible: &eligible,
        titles: &titles,
        caps: mx::Caps::new(max_items, max_gib),
        enforcing,
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
        scored: &scored,
        movies: &movies,
        series: &series,
        policy: &policy,
        verdicts: &verdicts,
        governance: &governance,
        owned: &owned,
        titles: &titles,
        destinations: &destinations,
        watch: &watch,
        report: &report,
        plex_ids: &plex_ids,
        play_keys: &play_keys,
        never_played_held,
    });
    if let Err(error) = ledger.write(&storage::ledger_path()) {
        eprintln!("[flinch-arrd] evictions.json write failed: {error}");
    }

    publish::publish(
        publish::Run {
            report: &report,
            sync,
            governance: &governance,
            handed: owned.scheduled.keys().filter_map(|id| by_id.get(id.as_str()).map(|item| (id.as_str(), item.bytes))).collect(),
            enforcing,
            interval_s: (!args.once).then_some(interval_s),
            model: model_label,
            shadow: (shadow_count as u64, shadow_gib),
            health,
            outside: history::outside(&removals, &ledger, (&movies, &series), now),
        },
        &items,
    )
}
