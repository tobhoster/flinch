use super::*;
use crate::plan::Pin;
use crate::quality::QualityAdvice;
use crate::regret::Regret;
use rstest::rstest;

const GIB: u64 = 1 << 30;
const NOW: u64 = 1_800_000_000;
const RADARR_COMPACT: u32 = 9;
const SONARR_COMPACT: u32 = 19;

fn advised(id: &str, gib: u64) -> MediaCandidate {
    MediaCandidate {
        id: id.to_string(),
        title: id.to_string(),
        size_bytes: gib * GIB,
        volume: Some("media".to_string()),
        regret: Regret::new(0.3, 1.0, 1.0),
        reason: String::new(),
        age_days: 400.0,
        exclusion: None,
        sequence: None,
        handed: false,
        announce: false,
        protect: false,
        quality: QualityAdvice {
            action: QualityAction::DowngradeQuality { estimated_reclaim_bytes: gib * GIB * 4 / 5 },
            marginal_regret_per_gb: 0.01,
            explanation: String::new(),
        },
        eviction_safety: 0.5,
        force: None,
    }
}

/// A library of advised items, each on profile 1 with a release of its own size.
struct World {
    candidates: Vec<MediaCandidate>,
    items: HashMap<String, ArrItem<'static>>,
    smallest: HashMap<String, u64>,
    evicting: Vec<String>,
    compact: Compact,
}

impl World {
    fn new() -> Self {
        let mut compact = Compact::default();
        compact.set(App::Radarr, "", RADARR_COMPACT);
        compact.set(App::Sonarr, "", SONARR_COMPACT);
        Self { candidates: Vec::new(), items: HashMap::new(), smallest: HashMap::new(), evicting: Vec::new(), compact }
    }

    fn add(mut self, id: String, item: ArrItem<'static>, gib: u64, release_gib: Option<u64>) -> Self {
        self.candidates.push(advised(&id, gib));
        if let Some(release) = release_gib {
            self.smallest.insert(id.clone(), release * GIB);
        }
        self.items.insert(id, item);
        self
    }

    fn movie(self, id: u32, gib: u64, release_gib: Option<u64>) -> Self {
        self.add(format!("radarr-{id}"), ArrItem { app: App::Radarr, instance: "", id, season: None, profile: Some(1) }, gib, release_gib)
    }

    fn season(self, series: u32, season: u32, gib: u64, release_gib: Option<u64>) -> Self {
        self.season_in("", series, season, gib, release_gib)
    }

    fn season_in(self, instance: &'static str, series: u32, season: u32, gib: u64, release_gib: Option<u64>) -> Self {
        let item = ArrItem { app: App::Sonarr, instance, id: series, season: Some(season), profile: Some(2) };
        self.add(flinch_ids_season(instance, series, season), item, gib, release_gib)
    }

    fn edit(mut self, id: &str, change: impl FnOnce(&mut MediaCandidate)) -> Self {
        if let Some(candidate) = self.candidates.iter_mut().find(|candidate| candidate.id == id) {
            change(candidate);
        }
        self
    }

    fn select(&self, ledger: &ActionLedger, config: &QualityActionsConfig) -> Selection {
        let evicting: HashSet<&str> = self.evicting.iter().map(String::as_str).collect();
        let inputs = Inputs {
            candidates: &self.candidates,
            items: &self.items,
            evicting: &evicting,
            smallest_release: &self.smallest,
            grace_days: 30,
            compact: self.compact.clone(),
            now: NOW,
        };
        select(&inputs, ledger, config)
    }
}

fn config() -> QualityActionsConfig {
    QualityActionsConfig { enabled: true, ..QualityActionsConfig::default() }
}

fn moved(selection: &Selection) -> Vec<&str> {
    selection.moves.iter().flat_map(|movement| movement.cards.iter().map(|card| card.card_id.as_str())).collect()
}

fn holds(selection: &Selection) -> Vec<(&str, Hold)> {
    selection.held.iter().map(|held| (held.card_id.as_str(), held.hold)).collect()
}

#[test]
fn an_advised_movie_with_a_smaller_release_moves_to_the_compact_profile() {
    let selection = World::new().movie(1, 40, Some(8)).select(&ActionLedger::default(), &config());
    let movement = &selection.moves[0];
    assert_eq!((movement.app, movement.id, movement.from_profile, movement.to_profile), (App::Radarr, 1, 1, RADARR_COMPACT));
    assert_eq!(movement.cards[0].release_bytes, Some(8 * GIB));
    assert!(selection.held.is_empty());
}

/// Items the household or the plan holds are never touched, and say nothing.
#[rstest]
#[case::partway_or_pinned(|c: &mut MediaCandidate| c.protect = true)]
#[case::on_a_keep_list(|c: &mut MediaCandidate| c.exclusion = Some(Exclusion::Pinned(Pin::KeepList)))]
#[case::no_watch_evidence(|c: &mut MediaCandidate| c.exclusion = Some(Exclusion::NoWatchEvidence))]
#[case::not_in_plex(|c: &mut MediaCandidate| c.exclusion = Some(Exclusion::NotInPlex))]
#[case::a_rule_forbids(|c: &mut MediaCandidate| c.exclusion = Some(Exclusion::Rule("kids".to_string())))]
#[case::in_its_grace_period(|c: &mut MediaCandidate| c.exclusion = Some(Exclusion::Grace { days: 30 }))]
#[case::newer_than_the_grace_period(|c: &mut MediaCandidate| c.age_days = 12.0)]
#[case::already_handed_over(|c: &mut MediaCandidate| c.handed = true)]
#[case::advised_to_keep(|c: &mut MediaCandidate| c.quality.action = QualityAction::EligibleForEviction)]
fn held_items_are_never_selected(#[case] change: fn(&mut MediaCandidate)) {
    let selection = World::new().movie(1, 40, Some(8)).edit("radarr-1", change).select(&ActionLedger::default(), &config());
    assert_eq!(selection, Selection::default());
}

#[test]
fn an_item_the_plan_evicts_is_not_downgraded() {
    let mut world = World::new().movie(1, 40, Some(8));
    world.evicting.push("radarr-1".to_string());
    assert_eq!(world.select(&ActionLedger::default(), &config()), Selection::default());
}

#[test]
fn never_played_items_keep_their_title_and_may_move() {
    let world = World::new().movie(1, 40, Some(8)).edit("radarr-1", |c| c.exclusion = Some(Exclusion::NeverPlayedOff));
    assert_eq!(moved(&world.select(&ActionLedger::default(), &config())), ["radarr-1"]);
}

/// Advised and movable, but not this cycle: why.
#[rstest]
#[case::release_only_20_percent_smaller(Some(32), Some(RADARR_COMPACT), Some(1), Hold::NoSmallerRelease)]
#[case::never_searched(None, Some(RADARR_COMPACT), Some(1), Hold::NoSmallerRelease)]
#[case::no_compact_profile(Some(8), None, Some(1), Hold::NoCompactProfile)]
#[case::already_compact(Some(8), Some(1), Some(1), Hold::AlreadyCompact)]
#[case::profile_unknown(Some(8), Some(RADARR_COMPACT), None, Hold::ProfileUnknown)]
fn advised_items_wait_with_a_reason(
    #[case] release_gib: Option<u64>,
    #[case] compact: Option<u32>,
    #[case] profile: Option<u32>,
    #[case] hold: Hold,
) {
    let mut world = World::new().movie(1, 40, release_gib);
    world.compact = Compact::default();
    if let Some(compact) = compact {
        world.compact.set(App::Radarr, "", compact);
    }
    world.items.insert("radarr-1".to_string(), ArrItem { app: App::Radarr, instance: "", id: 1, season: None, profile });
    let selection = world.select(&ActionLedger::default(), &config());
    assert_eq!(holds(&selection), [("radarr-1", hold)]);
    assert!(selection.moves.is_empty());
}

#[test]
fn a_waived_release_check_moves_without_a_search_result() {
    let world = World::new().movie(1, 40, None);
    let selection = world.select(&ActionLedger::default(), &QualityActionsConfig { require_smaller_release: false, ..config() });
    assert_eq!(moved(&selection), ["radarr-1"]);
}

#[test]
fn a_show_moves_whole_when_every_season_on_disk_is_advised() {
    let world = World::new().season(5, 1, 30, Some(6)).season(5, 2, 30, Some(6));
    let selection = world.select(&ActionLedger::default(), &config());
    assert_eq!(selection.moves.len(), 1, "one profile move for the series");
    assert_eq!((selection.moves[0].id, selection.moves[0].to_profile), (5, SONARR_COMPACT));
    assert_eq!(moved(&selection), ["sonarr-5-s1", "sonarr-5-s2"]);
}

#[test]
fn a_season_advised_to_keep_holds_its_whole_show() {
    let world = World::new()
        .season(5, 1, 30, Some(6))
        .season(5, 2, 30, Some(6))
        .edit("sonarr-5-s2", |c| c.quality.action = QualityAction::EligibleForEviction);
    let selection = world.select(&ActionLedger::default(), &config());
    assert!(selection.moves.is_empty());
    assert_eq!(holds(&selection), [("sonarr-5-s1", Hold::OtherSeasonKeeps)]);
}

#[test]
fn the_same_series_id_in_two_instances_is_two_shows() {
    // Series 5 in the default Sonarr has one season advised; series 5 in
    // anime has both: only anime's moves, to anime's own compact profile.
    let mut world = World::new()
        .season(5, 1, 30, Some(6))
        .season(5, 2, 30, Some(6))
        .edit("sonarr-5-s2", |c| c.quality.action = QualityAction::EligibleForEviction)
        .season_in("anime", 5, 1, 30, Some(6))
        .season_in("anime", 5, 2, 30, Some(6));
    world.compact.set(App::Sonarr, "anime", 42);
    let selection = world.select(&ActionLedger::default(), &config());
    assert_eq!(selection.moves.len(), 1);
    assert_eq!((selection.moves[0].instance.as_str(), selection.moves[0].id, selection.moves[0].to_profile), ("anime", 5, 42));
    assert_eq!(moved(&selection), ["sonarr@anime-5-s1", "sonarr@anime-5-s2"]);
    assert_eq!(holds(&selection), [("sonarr-5-s1", Hold::OtherSeasonKeeps)]);
}

fn flinch_ids_season(instance: &str, series: u32, season: u32) -> String {
    crate::ids::season_card_id(instance, series, season)
}

fn action(card_id: &str, at: u64, outcome: Outcome) -> Action {
    Action {
        card_id: card_id.to_string(),
        title: card_id.to_string(),
        app: App::Radarr,
        arr_id: 100,
        season: None,
        at,
        from_bytes: 40 * GIB,
        from_profile: 1,
        to_profile: RADARR_COMPACT,
        release_bytes: None,
        searched: true,
        outcome,
    }
}

#[test]
fn the_daily_cap_counts_the_last_24_hours_and_fills_with_what_fits() {
    // Two moves today and one yesterday: one of three left.
    let ledger = ActionLedger {
        actions: vec![
            action("radarr-90", NOW - 3_600, Outcome::Pending),
            action("radarr-91", NOW - 7_200, Outcome::Pending),
            action("radarr-92", NOW - DAY_SECS - 1, Outcome::Pending),
        ],
    };
    let world = World::new().season(5, 1, 50, Some(6)).season(5, 2, 50, Some(6)).movie(1, 40, Some(8)).movie(2, 20, Some(4));
    let selection = world.select(&ledger, &config());
    // The show is biggest but needs two; the biggest movie fits.
    assert_eq!(moved(&selection), ["radarr-1"]);
    let mut capped: Vec<&str> = holds(&selection).into_iter().filter(|(_, hold)| *hold == Hold::DailyCap).map(|(id, _)| id).collect();
    capped.sort_unstable();
    assert_eq!(capped, ["radarr-2", "sonarr-5-s1", "sonarr-5-s2"]);
}

#[rstest]
#[case::moved_and_waiting(Outcome::Pending, NOW - 3 * DAY_SECS, false)]
#[case::moved_and_landed(Outcome::Landed { bytes: GIB, at: NOW }, NOW - 30 * DAY_SECS, false)]
#[case::nothing_landed(Outcome::NothingSmaller { at: NOW }, NOW - 30 * DAY_SECS, false)]
#[case::failed_today(Outcome::Failed { reason: "HTTP 500".to_string() }, NOW - 3_600, false)]
#[case::failed_yesterday(Outcome::Failed { reason: "HTTP 500".to_string() }, NOW - DAY_SECS, true)]
fn an_item_moves_once_and_a_failed_move_waits_a_day(#[case] outcome: Outcome, #[case] at: u64, #[case] moves: bool) {
    let ledger = ActionLedger { actions: vec![action("radarr-1", at, outcome)] };
    let config = QualityActionsConfig { max_per_day: 10, ..config() };
    let selection = World::new().movie(1, 40, Some(8)).select(&ledger, &config);
    assert_eq!(!selection.moves.is_empty(), moves);
}

/// What became of a move, from the size on disk today.
#[rstest]
#[case::a_smaller_file_landed(Some(12 * GIB), 3, Outcome::Landed { bytes: 12 * GIB, at: NOW })]
#[case::still_waiting(Some(39 * GIB), 3, Outcome::Pending)]
#[case::nothing_smaller_in_two_weeks(Some(39 * GIB), 15, Outcome::NothingSmaller { at: NOW })]
#[case::left_the_library(None, 3, Outcome::Gone { at: NOW })]
fn moves_settle_against_the_size_on_disk(#[case] bytes: Option<u64>, #[case] days_ago: u64, #[case] outcome: Outcome) {
    let mut ledger = ActionLedger { actions: vec![action("radarr-1", NOW - days_ago * DAY_SECS, Outcome::Pending)] };
    let mut sizes: HashMap<&str, u64> = HashMap::from([("radarr-2", GIB)]);
    if let Some(bytes) = bytes {
        sizes.insert("radarr-1", bytes);
    }
    ledger.settle(&sizes, NOW);
    assert_eq!(ledger.actions[0].outcome, outcome);
}

#[test]
fn an_empty_inventory_settles_nothing_and_landed_bytes_add_up() {
    let mut ledger = ActionLedger {
        actions: vec![
            action("radarr-1", NOW - 3 * DAY_SECS, Outcome::Pending),
            action("radarr-2", NOW - 3 * DAY_SECS, Outcome::Landed { bytes: 10 * GIB, at: NOW }),
        ],
    };
    ledger.settle(&HashMap::new(), NOW);
    assert_eq!(ledger.actions[0].outcome, Outcome::Pending);
    assert_eq!(ledger.reclaimed_bytes(), 30 * GIB);
}

#[test]
fn a_recorded_move_names_each_season_and_a_failure_its_reason() {
    let world = World::new().season(5, 1, 30, Some(6)).season(5, 2, 30, Some(6));
    let selection = world.select(&ActionLedger::default(), &config());
    let mut ledger = ActionLedger::default();
    ledger.record(&selection.moves[0], NOW, Ok(true));
    ledger.record(&selection.moves[0], NOW + 1, Err("profile did not change".to_string()));
    let seasons: Vec<(Option<u32>, &Outcome)> = ledger.actions.iter().map(|action| (action.season, &action.outcome)).collect();
    let failed = Outcome::Failed { reason: "profile did not change".to_string() };
    assert_eq!(seasons, [(Some(1), &Outcome::Pending), (Some(2), &Outcome::Pending), (Some(1), &failed), (Some(2), &failed)]);
    assert_eq!(ledger.acted_since(NOW), 4);
}
