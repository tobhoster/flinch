//! What the daemon persists between cycles and publishes for the UI: item and
//! status snapshots, candidate streaks, run history. Plain JSON files on the
//! shared volume — no database, and every reader fails safe.

/// What the web UI renders: one row per library item with its decision.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ItemSnapshot {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub size_bytes: u64,
    /// "delete" | "archive" (moves to the archive root, [`crate::archive`]) | "keep"
    pub decision: String,
    pub reason: String,
    /// Expected regret of evicting it (see [`crate::regret`]); `None`
    /// for an item that is not a candidate.
    #[serde(default)]
    pub regret: Option<f64>,
    /// P(someone watches it within the regret horizon).
    #[serde(default)]
    pub p_watch: Option<f64>,
    /// Whether a protective exclusion is already recorded.
    pub protected: bool,
    /// Upstream poster (TMDB/TVDB), browser-loadable; the UI falls back to a
    /// generated tile when absent.
    #[serde(default)]
    pub poster_url: Option<String>,
    #[serde(default)]
    pub year: Option<u32>,
    /// Release quality label from the *arr payload ("Bluray-1080p", …).
    #[serde(default)]
    pub quality: Option<String>,
    /// Season label ("S2") for seasons; `episodes` is the on-disk count.
    #[serde(default)]
    pub season_label: Option<String>,
    #[serde(default)]
    pub episodes: Option<u32>,
    /// A season's episode numbers with a file, when the daemon read them: the
    /// fitter counts past plays against these as the daemon does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episodes_on_disk: Option<Vec<u32>>,
    /// Days since the library added it.
    #[serde(default)]
    pub age_days: Option<f32>,
    /// Days since anyone watched it. `None` means either "never played" or "no
    /// media-server entry" — `watch_source` distinguishes the two, because
    /// "Plex says zero plays" is evidence and "Plex has no idea" is not.
    #[serde(default)]
    pub last_watched_days: Option<f32>,
    /// Fraction of the item played (episodes watched / total for a season;
    /// for a movie 1.0 when finished, 0.0 when never played, and in between
    /// when only started). `None` = no media-server entry.
    #[serde(default)]
    pub watched_fraction: Option<f32>,
    /// Where the watch verdict came from: `"plex"` (matched) or absent when the
    /// item is unknown. The UI must never render an unmatched item as watched.
    #[serde(default)]
    pub watch_source: Option<String>,
    /// How safe evicting it is, 0..1: never above `1 − p_watch`.
    #[serde(default)]
    pub eviction_safety: Option<f64>,
    /// How hard it would be to get back (C_reacq; 1 = an ordinary re-download).
    #[serde(default)]
    pub friction: Option<f64>,
    /// What to do with its quality (see [`crate::quality`]).
    #[serde(default)]
    pub advice: Option<crate::quality::QualityAdvice>,
    /// The *arr web UI's route key for this item (`titleSlug`), so the UI can
    /// link to it. Numeric ids do not resolve in either app's router.
    #[serde(default)]
    pub title_slug: Option<String>,
    /// The library volume the item's files live on (a capacity key); `None`
    /// when no governed mount holds its path — such an item is never evicted.
    #[serde(default)]
    pub volume: Option<String>,
    /// Sonarr series status ("continuing", "ended", "upcoming"); seasons only.
    #[serde(default)]
    pub series_status: Option<String>,
    /// When the series last aired an episode (unix seconds); seasons only.
    #[serde(default)]
    pub last_aired_epoch: Option<u64>,
    /// Where the item lives in Plex, when resolved by GUID: the id Maintainerr
    /// acts on. Absent means FLINCH can neither protect nor schedule it.
    #[serde(default)]
    pub plex: Option<crate::ids::PlexIds>,
    /// How this item's plays were joined in the play logs (ratingKeys), so the
    /// fitter re-joins history exactly as the daemon did.
    #[serde(default)]
    pub play_keys: Option<crate::plex::PlayKeys>,
    /// When it was on disk, from *arr history ([`crate::presence`]), so the
    /// fitter asks about past cuts only where the item really was.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on_disk: Vec<crate::presence::Span>,
    /// How it leaves: the route of the collection it sits in once handed over,
    /// before that the route its eviction will take. `None` for a kept item.
    #[serde(default)]
    pub route: Option<crate::maintainerr::Route>,
    /// When an enforcing cycle's hand-off to Maintainerr was verified (unix
    /// seconds). `None` until then: a dry run hands nothing over.
    #[serde(default)]
    pub handed_at: Option<u64>,
    /// When Maintainerr deletes it: the hand-off plus its collection's window.
    /// `None` when not handed over, or when the collection acts on its next run.
    #[serde(default)]
    pub leaves_at: Option<u64>,
    /// The theme of its movie or show ([`crate::themes`]); `None` without a
    /// vector, before themes exist, and in an item file written before.
    #[serde(default)]
    pub theme: Option<String>,
}

/// Everything /api/status reports.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StatusSnapshot {
    pub scanned: usize,
    pub delete_candidates: usize,
    pub kept: usize,
    pub reclaimed_bytes: u64,
    pub protections_added: usize,
    pub protections_skipped_repeat: usize,
    pub dry_run: bool,
    pub ran_at_unix: u64,
    /// Seconds between scheduled runs; 0 when the daemon is a one-shot probe.
    #[serde(default)]
    pub interval_s: u64,
    /// `ran_at_unix + interval_s`; equals `ran_at_unix` when unscheduled.
    #[serde(default)]
    pub next_run_unix: u64,
    #[serde(default)]
    pub movies: u64,
    #[serde(default)]
    pub seasons: u64,
    /// Which hazard produced P(watch) this cycle, in the operator's words. A
    /// model that changes silently is indistinguishable from a bug.
    #[serde(default)]
    pub model: String,
    /// What enabling never-played reclaim would add. Published so a short
    /// candidate list can explain its own size instead of looking broken.
    #[serde(default)]
    pub shadow_items: u64,
    #[serde(default)]
    pub shadow_gib: f32,
    /// Each library volume's forecast against the plan. `None` when the daemon
    /// could not measure diskspace: the UI must then show nothing, not a fake 0%.
    #[serde(default)]
    pub capacity: Option<crate::capacity::CapacityStatus>,
    /// Everything the planner could select on a governed volume.
    #[serde(default)]
    pub eligible_bytes: u64,
    /// Set when the newest cycle failed: this snapshot is then the last good
    /// one, and the UI must say it is stale rather than present it as current.
    /// A successful cycle clears it.
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_error_at: Option<u64>,
    /// Watch evidence that was missing or partial this cycle, in words. While
    /// non-empty, never-played reclaim stays off whatever the settings say.
    #[serde(default)]
    pub evidence_problems: Vec<String>,
    /// Why never-played reclaim is held off this cycle, whatever its switch or
    /// disk pressure asks, so the UI never advises arming a rule that cannot
    /// run. `None` when nothing holds it, and in a status file written before
    /// it was published.
    #[serde(default)]
    pub never_played_hold: Option<super::NeverPlayedHold>,
    /// Whether the operator enabled never-played reclaim. Held and not
    /// enabled, the UI says to lift the hold and then enable the rule.
    #[serde(default)]
    pub never_played_requested: bool,
    /// What each watch source delivered this cycle, so the UI marks a service
    /// down on the daemon's word instead of guessing from item data.
    #[serde(default)]
    pub evidence: crate::watch::EvidenceHealth,
    /// This cycle's Maintainerr sync: what was protected, scheduled, released,
    /// deferred, and why anything was refused.
    #[serde(default)]
    pub sync: crate::maintainerr::SyncSummary,
    /// How many on-disk items each quality action is advised for.
    #[serde(default)]
    pub quality: QualityCounts,
    /// The last daily fit of the household panel, adopted or not, so the page
    /// can say which model runs, how well it has done, and what it still lacks.
    #[serde(default)]
    pub fit: Option<crate::fit::adopt::FitStatus>,
    /// Files Radarr or Sonarr removed lately that FLINCH did not hand over,
    /// newest first, with whether each will download again.
    #[serde(default)]
    pub outside_deletions: Vec<crate::outside::OutsideDeletion>,
    /// The taste vectors: how many titles have one, what this cycle embedded,
    /// and why it embedded less. `None` in a status file written before.
    #[serde(default)]
    pub embedding: Option<crate::embedding::EmbeddingStatus>,
    /// Incoming storage nobody is likely to watch, advice only
    /// ([`crate::inflow`]). Empty in a status file written before.
    #[serde(default)]
    pub inflow: Vec<crate::inflow::Suggestion>,
    /// Storage by theme: bytes, titles, played share and planned evictions
    /// ([`crate::themes`]). `None` before themes exist, and in a status file
    /// written before.
    #[serde(default)]
    pub themes: Option<crate::themes::ThemesStatus>,
    /// The native executor's run: shelf, deletes, holds and the undo list
    /// ([`crate::executor`]). `None` while Maintainerr executes.
    #[serde(default)]
    pub native: Option<crate::executor::NativeStatus>,
    /// This cycle's notifications: messages sent, events waiting for the
    /// hourly budget, failed channels ([`crate::notify`]). `None` with no
    /// channel, and in a status file written before.
    #[serde(default)]
    pub notify: Option<crate::notify::SendReport>,
    /// Moves to the compact profile: this cycle's, the held ones and what
    /// became of earlier ones ([`crate::quality::act`]). `None` while quality
    /// actions are off and none was ever made.
    #[serde(default)]
    pub quality_actions: Option<crate::quality::act::QualityActionsStatus>,
    /// Items grabbed again and again within 30 days ([`crate::quality::churn`]).
    /// `None` while the guard is off, and in a status file written before.
    #[serde(default)]
    pub upgrade_churn: Option<crate::quality::churn::ChurnStatus>,
    /// Upgrade searches for cutoff-unmet items ranked by P(watch)
    /// ([`crate::quality::upgrade`]). `None` while off and none was ever asked.
    #[serde(default)]
    pub upgrade_search: Option<crate::quality::upgrade::UpgradeSearchStatus>,
    /// What the operator's rules kept, forced and fought over this cycle
    /// ([`crate::rules`]). `None` without rules, and in a status file
    /// written before.
    #[serde(default)]
    pub rules: Option<crate::rules::RulesStatus>,
    /// What acting on approved inflow advice did ([`crate::inflow::act`]).
    /// `None` while it is off and FLINCH holds nothing it changed.
    #[serde(default)]
    pub inflow_actions: Option<crate::inflow::act::InflowActionsStatus>,
    /// What the torrent clients hold and what that kept this cycle
    /// ([`crate::torrents`]). `None` with no client configured, and in a
    /// status file written before.
    #[serde(default)]
    pub torrents: Option<crate::torrents::map::TorrentStatus>,
    /// The TRaSH quality sync's last preview and apply ([`crate::trash`]).
    /// `None` while the sync is off, and in a status file written before.
    #[serde(default)]
    pub trash: Option<crate::trash::TrashSummary>,
    /// What the streaming lookups know ([`crate::signals::streaming`]).
    /// `None` while streaming is off, and in a status file written before.
    #[serde(default)]
    pub streaming: Option<crate::signals::streaming::StreamingStatus>,
    /// What each Tracearr/Trakt source read and joined
    /// ([`crate::watch_sources`]). `None` with no source configured, and in a
    /// status file written before.
    #[serde(default)]
    pub watch_sources: Option<crate::watch_sources::WatchSourcesStatus>,
    /// Duplicate copies, their recommendations and the operator's choices
    /// ([`crate::dupes`]). `None` while the finder is off, and in a status
    /// file written before.
    #[serde(default)]
    pub dupes: Option<crate::dupes::DupesStatus>,
    /// Household self-service: whether links can be made, active keeps and
    /// the removal queue ([`crate::requests`]). `None` while it is off, and
    /// in a status file written before.
    #[serde(default)]
    pub household: Option<crate::requests::HouseholdStatus>,
    /// The archive tier: destinations, planned and sent moves
    /// ([`crate::archive`]). `None` while it is off, and in a status file
    /// written before.
    #[serde(default)]
    pub archive: Option<crate::archive::ArchiveStatus>,
    /// Every Radarr and Sonarr instance this cycle talked to, the defaults
    /// first, without keys ([`crate::arr::instances`]): the UI's labels and
    /// Open-in links. Empty in a status file written before.
    #[serde(default)]
    pub arr_instances: Vec<crate::arr::instances::InstanceView>,
}

/// Quality advice across the library, for the status page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QualityCounts {
    pub keep: usize,
    pub downgrade: usize,
    pub evict: usize,
}

impl QualityCounts {
    pub fn of(items: &[ItemSnapshot]) -> Self {
        items.iter().filter_map(|item| item.advice.as_ref()).fold(Self::default(), |mut counts, advice| {
            match advice.action {
                crate::quality::QualityAction::KeepOriginal { .. } => counts.keep += 1,
                crate::quality::QualityAction::DowngradeQuality { .. } => counts.downgrade += 1,
                crate::quality::QualityAction::EligibleForEviction => counts.evict += 1,
            }
            counts
        })
    }
}

/// Per-candidate run streak, so automation can require an item to stay a
/// candidate across consecutive runs before it is handed to Maintainerr. That
/// grace window is the difference between "hands-free" and "trigger-happy".
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CandidateState {
    pub streaks: std::collections::HashMap<String, u32>,
    /// When each streak began (unix seconds). A file written before this
    /// existed restarts the clock at the next run, so it waits, never skips.
    #[serde(default)]
    pub streak_started: std::collections::HashMap<String, u64>,
    pub last_run_unix: u64,
}

/// The grace window: consecutive runs, and the time those runs take at the
/// normal cadence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grace {
    pub runs: u32,
    pub interval_s: u64,
}

impl Grace {
    /// How long a streak must have lasted before it counts: the first run
    /// starts it, each later run adds one interval. Triggered runs come faster
    /// than the cadence, so counting runs alone would let them cut the window.
    fn min_age_s(self) -> u64 {
        u64::from(self.runs.max(1) - 1).saturating_mul(self.interval_s)
    }
}

/// Advance streaks by one run and return the ids that have survived at least
/// `grace.runs` consecutive appearances over at least [`Grace::min_age_s`] —
/// in `candidates` order, which is the plan's eviction order: the caps meet
/// the least-regret items first, and an item deferred by a cap keeps its place
/// for the next run.
pub fn advance_streaks(state: &mut CandidateState, candidates: &[String], grace: Grace, now: u64) -> Vec<String> {
    let fresh: std::collections::HashSet<&String> = candidates.iter().collect();
    state.streaks.retain(|id, _| fresh.contains(id));
    state.streak_started.retain(|id, _| fresh.contains(id));
    for id in candidates {
        *state.streaks.entry(id.clone()).or_insert(0) += 1;
        state.streak_started.entry(id.clone()).or_insert(now);
    }
    state.last_run_unix = now;
    let ripe = |id: &String| {
        let runs = state.streaks.get(id).is_some_and(|streak| *streak >= grace.runs.max(1));
        // A clock stepped back reads as no time passed: the item waits.
        let aged = state.streak_started.get(id).is_some_and(|since| now.saturating_sub(*since) >= grace.min_age_s());
        runs && aged
    };
    candidates.iter().filter(|id| ripe(id)).cloned().collect()
}

/// Read candidate streak state; a missing or corrupt file resets to empty
/// (everyone re-earns their grace window) rather than crashing a run.
pub fn read_candidate_state(path: &std::path::Path) -> CandidateState {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn write_candidate_state(path: &std::path::Path, state: &CandidateState) -> std::io::Result<()> {
    crate::persist::replace(path, &serde_json::to_vec(state)?)
}

/// One point of run-to-run history for the UI's time-series. No database —
/// an ordered JSON array capped at 500 points; old points are dropped by
/// retention, never by corruption.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HistoryPoint {
    pub ran_at_unix: u64,
    pub scanned: u64,
    pub delete_candidates: u64,
    pub reclaimed_bytes: u64,
    pub protections_added: u64,
    /// Whether this run was observation-only. Older rows predate the field and
    /// read as unknown in the UI rather than being guessed.
    #[serde(default)]
    pub dry_run: Option<bool>,
    /// Disk utilization of the *arr volumes (fraction in [0, 1]); `None` when
    /// the run could not measure capacity. Rows from before capacity existed
    /// read as unknown rather than 0.
    #[serde(default)]
    pub utilization: Option<f32>,
}

pub const HISTORY_CAP: usize = 500;

/// Append the latest run to `history.json`, keeping the newest `HISTORY_CAP`
/// points. Missing or corrupt history resets to just this point — a chart gap
/// is honest, a crash-looping parser is not.
pub fn append_history(path: &std::path::Path, point: &HistoryPoint) -> std::io::Result<()> {
    let mut history: Vec<HistoryPoint> =
        std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
    history.push(point.clone());
    if history.len() > HISTORY_CAP {
        history.drain(0..history.len() - HISTORY_CAP);
    }
    crate::persist::replace(path, &serde_json::to_vec(&history)?)
}

/// Persist a run's visible state so the web UI never touches the *arrs itself.
pub fn write_snapshots(
    status_path: &std::path::Path,
    items_path: &std::path::Path,
    status: &StatusSnapshot,
    items: &[ItemSnapshot],
) -> std::io::Result<()> {
    for (path, payload) in [(status_path, serde_json::to_vec(status)?), (items_path, serde_json::to_vec(items)?)] {
        crate::persist::replace(path, &payload)?;
    }
    Ok(())
}

/// Stamp a failed cycle onto status.json, keeping the last good snapshot
/// beside it. Nothing published yet (or unreadable) leaves the log as the only
/// record — inventing a snapshot would claim numbers no cycle produced.
pub fn record_cycle_error(status_path: &std::path::Path, error: &str) {
    let Some(mut status) = std::fs::read_to_string(status_path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .filter(serde_json::Value::is_object)
    else {
        return;
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    status["last_error"] = serde_json::Value::from(error);
    status["last_error_at"] = serde_json::Value::from(now);
    if let Ok(bytes) = serde_json::to_vec(&status) {
        if let Err(write_error) = crate::persist::replace(status_path, &bytes) {
            eprintln!("[flinch-arrd] could not record the cycle error in status.json: {write_error}");
        }
    }
}
