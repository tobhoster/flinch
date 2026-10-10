//! The daemon: inventory -> forecast -> plan -> hand-off, on a tick.
//!
//! Run this as a long-lived process. Each cycle pulls the *arr libraries,
//! merges media-server watch state, forecasts every library volume, plans the
//! least-regret evictions that fit each forecast, and writes the plan to
//! `state/eviction-plan.json`. With `dry_run` off it hands the plan to
//! Maintainerr, or with `executor: native` deletes itself (see `native`).

use anyhow::{Context, Result};
use clap::Parser;
use flinch_archive::daemon::{reconcile, NeverPlayedHold};
use flinch_archive::maintainerr::{self as mx, HttpMaintainerr, OwnedState, SyncItem};
use flinch_archive::plan::{candidates, Exclusion};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

mod archive;
mod args;
mod arr_write;
mod dupes;
mod embeddings;
mod evidence;
mod fetch;
mod handoff;
mod history;
mod household;
mod inflow_act;
mod media;
mod model;
mod native;
mod notices;
mod publish;
mod quality_act;
mod rules;
mod signals;
mod sink;
mod snapshot;
mod storage;
mod streaming;
mod themes;
mod torrents;
mod trash_sync;
mod upgrade_guard;
mod upgrade_search;

#[cfg(test)]
mod tests;

use args::Args;
use fetch::{fetch_disks, fetch_inventory, Fetched};
use sink::Sink;

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
    args.resolve();
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
    // The Settings page's "Send test" is answered between and during cycles.
    tokio::spawn(notices::answer_tests(http.clone()));

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
            notices::cycle_failed(&http, &settings).await;
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
    // Every *arr instance of this cycle: settings may add one without a restart.
    let args = &args.for_cycle(&settings.instances);
    // Quality profiles before anything else writes: an inventory outage must
    // not hold back an apply, and a library read sizes each change.
    let fetched = fetch_inventory(http, args, &settings.keep_tag).await;
    trash_sync::run(http, args, settings, fetched.as_ref().ok().map(|f| (f.movies.as_slice(), f.series.as_slice()))).await;
    let Fetched { movies, series, removals, downloads, tags } = fetched?;
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
        jellyfin_ids,
        play_keys,
        plex_keeps,
        plex_listed,
        plex_content,
        jellyfin_plays,
    } = evidence::gather(args, http, settings, &movies, &series, &mut cards, cycle_now).await?;
    // Taste vectors of every movie and show, refreshed within today's budget.
    let library = embeddings::Library { movies: &movies, series: &series, plex_content: &plex_content, plex_ids: &plex_ids };
    let embeddings::Refresh { vectors, status: embedding } = embeddings::refresh(&settings.embedding, library, cycle_now).await;

    // Dry run unless the planner says otherwise; FLINCH_DRY_RUN forces one.
    let forced_dry = std::env::var("FLINCH_DRY_RUN").map(|v| v == "1" || v == "true").unwrap_or(false);
    let dry_run = settings.planner.dry_run || forced_dry;
    // Torrents still seeding or holding an item's bytes keep it this cycle.
    let torrent_goes = settings.executor == flinch_archive::executor::Executor::Native;
    let torrent_library = torrents::Library { movies: &movies, series: &series, downloads: &downloads, cards: &cards };
    let torrents = torrents::gather(http, &settings.torrents, torrent_goes, torrent_library, dry_run).await;
    // --- Maintainerr, read first: an operator's exclusion pins its item. A dry
    // run reads the live state and prints every write it would send. ---
    let maintainerr = HttpMaintainerr::new(&args.maintainerr_url, &args.maintainerr_key)?;
    let mut api = if dry_run { Sink::DryRun(maintainerr) } else { Sink::Live(maintainerr) };
    let titles = settings.collection_titles();
    let mut owned = OwnedState::read(&state_dir());
    // Plex ids come only from the GUID join; a confirmed duplicate choice
    // settles an item's several-copies hold onto the copy kept.
    let mut sync_items: Vec<SyncItem> = handoff::sync_items(&cards, &plex_ids, &play_keys);
    let dupes_plex = dupes::Plex { content: &plex_content, play_keys: &play_keys, history: &plex_history_rows };
    let dupes_found = dupes::find(http, args, &settings.dupes, &movies, dupes_plex, &mut sync_items).await;
    let observed = mx::observe(&mut api, &sync_items, &titles, &owned).await;
    if let (Err(error), flinch_archive::executor::Executor::Maintainerr) = (&observed, settings.executor) {
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
    let running = model::hazard(&state_dir(), cycle_now);

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

    let mut signals = signals::gather(http, args, &cards, cycle_now).await;
    let streaming_status = streaming::gather(http, &settings.streaming, (&movies, &series), cycle_now, &mut signals).await;
    for problem in &signals.problems {
        eprintln!("[flinch-arrd] signals: {problem}");
    }
    let (governance, mut ledger) = storage::govern(&disks, (&movies, &series), &cards, &signals, &settings.capacity, cycle_now);

    let plays: HashMap<String, candidates::Plays> = cards
        .iter()
        .filter_map(|card| {
            let join = joins.get(card.id.as_str())?;
            let logged = (play_log.item_plays(join), play_log.audience_plays(join));
            Some((card.id.clone(), evidence::jellyfin::with_plays(logged, jellyfin_plays.get(&card.id))))
        })
        .collect();
    // What the executor can act on: a Plex match, or a Jellyfin/Emby match when
    // the native executor's Leaving Soon shelf lives there.
    let jellyfin_shelf = settings.executor == flinch_archive::executor::Executor::Native
        && settings.native.leaving_soon_server == flinch_archive::executor::ShelfServer::Jellyfin;
    let in_plex: HashSet<String> = plex_ids.keys().chain(jellyfin_ids.keys().filter(|_| jellyfin_shelf)).cloned().collect();
    let handed = native::handed(settings.executor, &owned);
    // Taste has weight only in an adopted fit, and is asked with the outcomes
    // that fit learned from; under the priors it would move nothing.
    let taste = running.outcomes.as_ref().map(|record| flinch_archive::taste::read_cards(&cards, &vectors, record)).unwrap_or_default();
    // Inflow advice asks taste too: the adopted fit's outcomes, else the household's.
    let household = flinch_archive::inflow::household_record(&cards, cycle_now);
    let inflow = flinch_archive::inflow::suggest(&flinch_archive::inflow::Inputs {
        cards: &cards,
        series: &series,
        movies: &movies,
        requests: &signals.requests,
        streams: &signals.streams,
        vectors: &vectors,
        record: running.outcomes.as_ref().unwrap_or(&household),
    });
    // Themes of the taste vectors: storage by theme, and downgrade advice in cold ones.
    let themed = themes::refresh(&vectors, (&movies, &series), &cards, &plays, cycle_now);
    let library = candidates::Library {
        cards: &cards,
        movies: &movies,
        series: &series,
        watch: &watch,
        plays: &plays,
        located: &governance.located,
        in_plex: &in_plex,
        handed: &handed,
        signals: &signals,
        taste: &taste,
        cold_themes: &themed.cold,
        never_played,
        seeding: &torrents.holds,
        now: cycle_now,
    };
    let mut candidates = candidates::build(&library, &settings.planner, &running.hazard);
    // Torrents below the desired ratio are drawn on last; a rule outranks that.
    torrents::spare(&mut candidates, &torrents.spared);
    let (mut household, rules_in_force) = household::prepare(settings, &library, &signals.contacts, &mut candidates, cycle_now);
    let sources = flinch_archive::rules::facts::Sources { plex_ids: &plex_ids, tags: &tags, themes: &themed.themes };
    let rule_status = rules::apply(&rules_in_force, &mut candidates, &library, &sources, &governance.forecasts, cycle_now);
    let archive_to = archive::destinations(args, settings, &governance);
    let report = reconcile(&candidates, &governance.forecasts, &settings.planner, &archive_to.0).context("planning")?;
    let plan = &report.plan;
    let gib = |bytes: u64| bytes as f64 / 1_073_741_824.0;
    publish::plan(plan, &governance.forecasts, dry_run, cycle_now)?;

    // --- grace streaks, then the hand-off or the native executor ---
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let eligible = native::eligible(settings, observed.is_ok(), &report, &handed, interval_s, now, dry_run)?;

    let by_id: HashMap<&str, &SyncItem> = sync_items.iter().map(|item| (item.card_id.as_str(), item)).collect();
    let (sync, native_status) = if settings.executor == flinch_archive::executor::Executor::Native {
        let cycle = native::Cycle {
            args,
            http,
            settings,
            library: &library,
            report: &report,
            eligible: &eligible,
            plex_ids: &plex_ids,
            jellyfin_ids: &jellyfin_ids,
            health: &health,
            governance: &governance,
            torrents: (torrents.session.as_ref(), &torrents.holdings),
            dry_run,
        };
        (mx::SyncSummary { dry_run, ..Default::default() }, Some(native::run(cycle, &mut ledger).await))
    } else {
        let handoff = handoff::Handoff {
            items: &by_id,
            cards: &cards,
            report: &report,
            eligible: &eligible,
            titles: &titles,
            caps: mx::Caps::new(settings.max_items, settings.max_gib),
            enforcing: !dry_run,
            now,
            plex_listed: plex_listed.as_ref(),
            seerr: fetch::maintainerr_seerr_configured(http, args).await,
        };
        (handoff::sync(handoff, observed, &mut api, &mut owned, &governance, &mut ledger, operator_keeps.len()).await, None)
    };
    // Moves are FLINCH's own writes, whichever executor deletes.
    let archive_cycle = archive::Cycle { http, args, settings, report: &report, destinations: archive_to, dry_run, interval_s, now };
    let archive_status = archive::run(archive_cycle, &mut ledger).await;
    household::shelves(household.as_mut(), http, settings, &plex_ids, dry_run).await;

    let mut items = snapshot::build_items(snapshot::ItemInputs {
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
        themes: &themed.themes,
    });
    native::annotate(&mut items, native_status.as_ref());
    if let Err(error) = ledger.write(&storage::ledger_path()) {
        eprintln!("[flinch-arrd] evictions.json write failed: {error}");
    }
    let problems = notices::problems(&health, &sync, &signals.problems, plan);
    let household = household.as_ref();
    let told =
        notices::Cycle { settings, items: &items, ledger: &ledger, governance: &governance, plan, dry_run, problems, now, household };
    let notify = notices::notify(http, told).await;
    let library = quality_act::Library { candidates: &candidates, arr: (&movies, &series), report: &report, signals: &signals };
    let quality = upgrade_search::quality_cycle(http, args, settings, (library, &governance), dry_run, now).await;
    // Approved inflow advice, acted on only while a disk is over its target.
    let inflow_actions = inflow_act::run(http, args, settings, &inflow, &governance.forecasts, dry_run, now).await;

    let (shadow_count, shadow_bytes) = flinch_archive::plan::never_played_preview(&candidates);
    publish::publish(
        publish::Run {
            report: &report,
            sync,
            governance: &governance,
            handed: native::handed_bytes(settings.executor, &owned, &by_id),
            native: native_status,
            dry_run,
            interval_s: (!args.once).then_some(interval_s),
            model: running.label,
            shadow: (shadow_count as u64, gib(shadow_bytes) as f32),
            health,
            never_played_hold,
            never_played_requested: settings.unwatched_reclaim_enabled,
            outside: history::outside(&removals, &ledger, (&movies, &series), now),
            embedding,
            inflow,
            inflow_actions,
            notify,
            themes: themes::status(&themed, &cards, &plays, plan, cycle_now),
            torrents: torrents.status,
            rules: rule_status,
            quality,
            streaming: streaming_status,
            dupes: dupes::act(http, args, settings, dupes_found, &candidates, dry_run, now).await,
            household: household.map(household::Household::status),
            archive: archive_status,
            arrs: &args.arrs,
        },
        &items,
    )
}
