//! The cycle's notifications: what this cycle published, turned into
//! [`notify::Event`]s and handed to the configured channels. Everything here
//! reads what the cycle already decided; nothing is decided for a message.
//!
//! - Leaving Soon: every item whose verified hand-over put it in the Leaving
//!   Soon window (any executor that fills `route`/`handed_at`/`leaves_at`).
//! - Deletions: hand-overs to a delete collection, and items the eviction
//!   ledger saw leave the library in the last two days.
//! - Problems: evidence gaps and service failures, told once seen on
//!   [`notify::PERSIST_CYCLES`] cycles in a row; a failed cycle is one too.
//! - Digest: once a UTC day, from the configured hour.
//! - Household ([`super::household`]): requesters, posters and no-login keep
//!   links on Leaving Soon titles, the weekly newsletter, and each
//!   requester's own copy on their own addresses.
//!
//! The channels' dedupe keys make every event once-only, so each cycle simply
//! offers everything current.

use flinch_archive::capacity::EvictionLedger;
use flinch_archive::govern::Governance;
use flinch_archive::maintainerr::{Route, SyncSummary};
use flinch_archive::notify::{self, Digest, DiskLine, Event, Notifier, SendReport, Stage, TopCandidate};
use flinch_archive::plan::EvictionPlan;
use flinch_archive::watch::EvidenceHealth;
use flinch_archive::ItemSnapshot;
use std::time::Duration;

/// How far back a deletion is still news: a channel enabled today is not told
/// of every deletion the ledger still tracks.
const GONE_NEWS_SECS: u64 = 2 * 86_400;
const DAY_SECS: u64 = 86_400;
/// Titles the digest names as next to go.
const DIGEST_TOP: usize = 5;
/// How often the daemon looks for a test the Settings page asked for.
const TEST_POLL: Duration = Duration::from_secs(2);

/// What one cycle offers the channels.
pub(super) struct Cycle<'a> {
    pub(super) settings: &'a flinch_archive::daemon::RuntimeSettings,
    pub(super) items: &'a [ItemSnapshot],
    pub(super) ledger: &'a EvictionLedger,
    pub(super) governance: &'a Governance,
    pub(super) plan: &'a EvictionPlan,
    pub(super) dry_run: bool,
    /// (stable key, words) of every problem this cycle saw.
    pub(super) problems: Vec<(String, String)>,
    pub(super) now: u64,
    /// The household's links and recipients; `None` while those are off.
    pub(super) household: Option<&'a super::household::Household>,
}

/// The problems a notification may tell, keyed so the same fault keeps its
/// key from cycle to cycle. Optional sources that are simply not configured
/// are the operator's choice, not a fault.
pub(super) fn problems(health: &EvidenceHealth, sync: &SyncSummary, signals: &[String], plan: &EvictionPlan) -> Vec<(String, String)> {
    let mut seen: Vec<(String, String)> =
        health.problems().into_iter().map(|problem| (format!("evidence:{problem}"), format!("Watch evidence: {problem}"))).collect();
    // The error's own words can name a host; the key and message stay generic.
    if sync.error.is_some() {
        seen.push(("maintainerr".to_string(), "Maintainerr is unreadable: nothing is protected or handed over".to_string()));
    }
    seen.extend(sync.problems.iter().map(|problem| (format!("sync:{problem}"), format!("Maintainerr: {problem}"))));
    seen.extend(
        signals
            .iter()
            .filter(|problem| !problem.contains(" not configured"))
            .map(|problem| (format!("signals:{problem}"), problem.clone())),
    );
    if plan.solver_error.is_some() {
        seen.push(("solver".to_string(), "The HiGHS solver failed: plans are made greedily".to_string()));
    }
    seen
}

pub(super) fn display_title(item: &ItemSnapshot) -> String {
    match &item.season_label {
        Some(season) => format!("{} {season}", item.title),
        None => item.title.clone(),
    }
}

fn events(cycle: &Cycle<'_>) -> Vec<Event> {
    let mut events = Vec::new();
    for item in cycle.items {
        let Some(handed_at) = item.handed_at else { continue };
        let (id, title, bytes) = (item.id.clone(), display_title(item), item.size_bytes);
        match item.route {
            Some(Route::LeavingSoon) => events.push(match cycle.household {
                Some(household) => household.leaving(cycle.settings, item, handed_at, title),
                None => Event::LeavingSoon {
                    id,
                    title,
                    bytes,
                    handed_at,
                    leaves_at: item.leaves_at,
                    requesters: Vec::new(),
                    poster: None,
                    keep_url: None,
                },
            }),
            Some(Route::Delete) => events.push(Event::Deleted { id, title, bytes, handed_at, stage: Stage::Handed }),
            None => {}
        }
    }
    // A move to the archive is credited in the ledger too, but nothing was deleted.
    for (id, eviction) in cycle.ledger.entries.iter().filter(|(_, eviction)| !eviction.archived) {
        if eviction.gone_at.is_some_and(|gone| cycle.now.saturating_sub(gone) < GONE_NEWS_SECS) {
            events.push(Event::Deleted {
                id: id.clone(),
                title: eviction.title.clone(),
                bytes: eviction.bytes,
                handed_at: eviction.handed_at,
                stage: Stage::Gone,
            });
        }
    }
    let hour = (cycle.now % DAY_SECS) / 3_600;
    if hour >= u64::from(cycle.settings.notify.digest_hour_utc) {
        events.push(Event::Digest(digest(cycle)));
    }
    let left = super::household::Household::left_this_week(cycle.ledger, cycle.now);
    if let Some(letter) = cycle.household.and_then(|household| household.newsletter(cycle.settings, cycle.items, left, "household")) {
        events.push(Event::Newsletter(letter));
    }
    events
}

fn digest(cycle: &Cycle<'_>) -> Digest {
    let window_days = cycle.settings.capacity.sliding_window_days;
    let disks = cycle
        .governance
        .forecasts
        .iter()
        .map(|forecast| DiskLine {
            volume: forecast.volume.clone(),
            used_bytes: forecast.forecast.current_used_bytes,
            capacity_bytes: forecast.forecast.max_capacity_bytes,
            projected_used_bytes: forecast.forecast.projected_used_bytes,
            window_days,
            target_reclaim_bytes: forecast.forecast.target_reclaim_bytes,
            emergency: forecast.forecast.is_emergency,
        })
        .collect();
    let freed: Vec<u64> = cycle
        .ledger
        .entries
        .values()
        .filter(|eviction| eviction.gone_at.is_some_and(|gone| cycle.now.saturating_sub(gone) < DAY_SECS))
        .map(|eviction| eviction.bytes)
        .collect();
    let top = cycle
        .plan
        .items
        .iter()
        .take(DIGEST_TOP)
        .map(|item| TopCandidate { id: item.id.clone(), title: item.title.clone(), bytes: item.size_bytes, regret: item.regret })
        .collect();
    Digest { day: cycle.now / DAY_SECS, dry_run: cycle.dry_run, disks, freed_items: freed.len(), freed_bytes: freed.iter().sum(), top }
}

/// Offer the cycle's events to every channel, and each requester their own
/// copy. `None` with no channel and nobody to tell.
pub(super) async fn notify(http: &reqwest::Client, cycle: Cycle<'_>) -> Option<SendReport> {
    let config = &cycle.settings.notify;
    let people = cycle.household.map_or(&[][..], |household| household.people.as_slice());
    if config.channels.is_empty() && people.is_empty() {
        return None;
    }
    let dir = super::state_dir();
    let notifier = Notifier::new(http, config, &dir).with_recipients(people);
    let mut events = events(&cycle);
    match notifier.problems(&cycle.problems, true, cycle.now) {
        Ok(persisting) => events.extend(persisting),
        Err(error) => eprintln!("[flinch-arrd] {} write failed, problem streaks start over: {error}", notify::STATE_FILE),
    }
    let mut report = notifier.send_at(&events, cycle.now).await;
    if let Some(household) = cycle.household {
        let left = super::household::Household::left_this_week(cycle.ledger, cycle.now);
        let personal = household.personal(cycle.settings, cycle.items, &events, left);
        let own = notifier.send_personal(&personal, cycle.now).await;
        report.messages += own.messages;
        report.deferred += own.deferred;
        report.failures.extend(own.failures);
    }
    log(&report);
    Some(report)
}

/// A failed cycle is a problem of its own; it neither advances nor breaks
/// the others' streaks. Its words stay generic: the error can name a host.
pub(super) async fn cycle_failed(http: &reqwest::Client, settings: &flinch_archive::daemon::RuntimeSettings) {
    if settings.notify.channels.is_empty() {
        return;
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let dir = super::state_dir();
    let notifier = Notifier::new(http, &settings.notify, &dir);
    let failed = [("cycle".to_string(), "FLINCH cycles keep failing: see the status page or the daemon log".to_string())];
    match notifier.problems(&failed, false, now) {
        Ok(persisting) if !persisting.is_empty() => log(&notifier.send_at(&persisting, now).await),
        Ok(_) => {}
        Err(error) => eprintln!("[flinch-arrd] {} write failed, problem streaks start over: {error}", notify::STATE_FILE),
    }
}

fn log(report: &SendReport) {
    if report.messages > 0 || report.deferred > 0 {
        println!("[flinch-arrd] notify: {} message(s) sent, {} event(s) wait for the hourly budget", report.messages, report.deferred);
    }
    for failure in &report.failures {
        eprintln!("[flinch-arrd] notify: {failure}");
    }
}

/// Answer the Settings page's "Send test" for as long as the daemon runs,
/// between and during cycles. Only the daemon holds the channels' secrets.
pub(super) async fn answer_tests(http: reqwest::Client) {
    loop {
        if let Some(result) = notify::answer_test(&http, &super::state_dir()).await {
            let delivered = result.channels.iter().filter(|channel| channel.ok).count();
            println!("[flinch-arrd] notify test: {delivered} of {} channel(s) delivered", result.channels.len());
            for channel in result.channels.iter().filter(|channel| !channel.ok) {
                eprintln!("[flinch-arrd] notify test: {}: {}", channel.name, channel.detail);
            }
            if let Some(error) = &result.error {
                eprintln!("[flinch-arrd] notify test: {error}");
            }
        }
        tokio::time::sleep(TEST_POLL).await;
    }
}
