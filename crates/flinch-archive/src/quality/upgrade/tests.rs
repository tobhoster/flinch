use super::*;
use crate::capacity::CapacityForecast;
use crate::quality::{QualityAction, QualityAdvice};
use crate::regret::Regret;
use rstest::rstest;
use serde_json::json;

const GIB: u64 = 1 << 30;
const NOW: u64 = 1_800_000_000;

fn movie(id: u32, p_watch: f64) -> MediaCandidate {
    MediaCandidate {
        id: format!("radarr-{id}"),
        title: format!("Movie {id}"),
        size_bytes: 10 * GIB,
        volume: Some("media".to_string()),
        regret: Regret::new(p_watch, 1.0, 1.0),
        reason: String::new(),
        age_days: 400.0,
        exclusion: None,
        sequence: None,
        handed: false,
        announce: false,
        protect: false,
        quality: QualityAdvice {
            action: QualityAction::DowngradeQuality { estimated_reclaim_bytes: 0 },
            marginal_regret_per_gb: 0.0,
            explanation: String::new(),
        },
        eviction_safety: 0.5,
        force: None,
    }
}

/// Unmet movies, each with a 15 GiB release (5 GiB growth) and 100 GiB of headroom.
struct World {
    candidates: Vec<MediaCandidate>,
    items: HashMap<String, ArrItem<'static>>,
    largest: HashMap<String, u64>,
    headroom: HashMap<String, u64>,
    planned: Vec<String>,
    churning: Vec<String>,
}

impl World {
    fn new(movies: &[(u32, f64)]) -> Self {
        let candidates: Vec<MediaCandidate> = movies.iter().map(|&(id, p)| movie(id, p)).collect();
        let items = movies
            .iter()
            .map(|&(id, _)| (format!("radarr-{id}"), ArrItem { app: App::Radarr, instance: "", id, season: None, profile: Some(1) }))
            .collect();
        let largest = movies.iter().map(|&(id, _)| (format!("radarr-{id}"), 15 * GIB)).collect();
        let headroom = HashMap::from([("media".to_string(), 100 * GIB)]);
        Self { candidates, items, largest, headroom, planned: Vec::new(), churning: Vec::new() }
    }

    fn edit(mut self, id: u32, change: impl FnMut(&mut MediaCandidate)) -> Self {
        let card = format!("radarr-{id}");
        self.candidates.iter_mut().filter(|candidate| candidate.id == card).for_each(change);
        self
    }

    fn select(&self, ledger: &SearchLedger, max_per_day: u32) -> Selection {
        let unmet: HashSet<String> = self.items.keys().cloned().collect();
        let planned: HashSet<&str> = self.planned.iter().map(String::as_str).collect();
        let churning: HashSet<&str> = self.churning.iter().map(String::as_str).collect();
        let inputs = Inputs {
            candidates: &self.candidates,
            items: &self.items,
            unmet: &unmet,
            planned: &planned,
            churning: &churning,
            largest_release: &self.largest,
            headroom: &self.headroom,
            now: NOW,
        };
        select(&inputs, ledger, &UpgradeSearchConfig { enabled: true, max_per_day })
    }
}

fn picked(selection: &Selection) -> Vec<&str> {
    selection.picks.iter().map(|pick| pick.card_id.as_str()).collect()
}

fn held(selection: &Selection, card: &str) -> Option<Hold> {
    selection.held.iter().find(|held| held.card_id == card).map(|held| held.hold)
}

#[test]
fn likeliest_watched_go_first_within_the_daily_cap() {
    let selection = World::new(&[(1, 0.1), (2, 0.9), (3, 0.5)]).select(&SearchLedger::default(), 2);
    assert_eq!(picked(&selection), ["radarr-2", "radarr-3"]);
    assert_eq!(held(&selection, "radarr-1"), Some(Hold::DailyCap));
}

#[rstest]
#[case::pinned_or_partway(|c: &mut MediaCandidate| c.protect = true, Hold::Protected)]
#[case::hard_to_replace(|c: &mut MediaCandidate| c.regret = Regret::new(0.9, 3.5, 1.0), Hold::HardToReplace)]
#[case::handed_over(|c: &mut MediaCandidate| c.handed = true, Hold::InPlan)]
fn guarded_items_are_held(#[case] change: fn(&mut MediaCandidate), #[case] hold: Hold) {
    let selection = World::new(&[(1, 0.9)]).edit(1, change).select(&SearchLedger::default(), 5);
    assert!(selection.picks.is_empty());
    assert_eq!(held(&selection, "radarr-1"), Some(hold));
}

#[test]
fn planned_churning_and_unsized_items_are_held() {
    let mut world = World::new(&[(1, 0.9), (2, 0.8), (3, 0.7), (4, 0.1)]);
    world.planned.push("radarr-1".to_string());
    world.churning.push("radarr-2".to_string());
    world.largest.remove("radarr-3");
    let selection = world.select(&SearchLedger::default(), 5);
    assert_eq!(picked(&selection), ["radarr-4"]);
    assert_eq!(held(&selection, "radarr-1"), Some(Hold::InPlan));
    assert_eq!(held(&selection, "radarr-2"), Some(Hold::Churning));
    assert_eq!(held(&selection, "radarr-3"), Some(Hold::SizeUnknown));
}

#[test]
fn headroom_is_spent_by_each_search_in_turn() {
    let mut world = World::new(&[(1, 0.9), (2, 0.8), (3, 0.7)]);
    world.headroom.insert("media".to_string(), 8 * GIB);
    world.largest.insert("radarr-3".to_string(), 10 * GIB);
    let selection = world.select(&SearchLedger::default(), 5);
    // The first takes 5 of 8 GiB; the next 5 does not fit the 3 left, a same-size release does.
    assert_eq!(picked(&selection), ["radarr-1", "radarr-3"]);
    assert_eq!(held(&selection, "radarr-2"), Some(Hold::NoHeadroom));
}

#[test]
fn an_item_without_a_forecast_volume_is_held() {
    let selection = World::new(&[(1, 0.9)]).edit(1, |c| c.volume = None).select(&SearchLedger::default(), 5);
    assert_eq!(held(&selection, "radarr-1"), Some(Hold::NoHeadroom));
}

fn searched(world: &World, id: u32, at: u64, result: Result<(), String>) -> SearchLedger {
    let mut ledger = SearchLedger::default();
    let pick = world.select(&SearchLedger::default(), 5).picks.into_iter().find(|pick| pick.arr_id == id);
    ledger.record(&pick.expect("picked"), at, result);
    ledger
}

#[rstest]
#[case::pending_blocks(NOW - 40 * DAY_SECS, Ok(()), false)]
#[case::recent_failure_blocks(NOW - 3_600, Err("refused".to_string()), false)]
#[case::old_failure_retries(NOW - 2 * DAY_SECS, Err("refused".to_string()), true)]
fn the_ledger_holds_back_repeats(#[case] at: u64, #[case] result: Result<(), String>, #[case] searches: bool) {
    let world = World::new(&[(1, 0.9)]);
    let ledger = searched(&world, 1, at, result);
    assert_eq!(!world.select(&ledger, 50).picks.is_empty(), searches);
}

#[test]
fn the_cap_counts_the_last_day_of_searches() {
    let world = World::new(&[(1, 0.9), (2, 0.8)]);
    let ledger = searched(&world, 1, NOW - 3_600, Ok(()));
    assert!(world.select(&ledger, 1).picks.is_empty());
}

#[rstest]
#[case::reached_the_cutoff(false, 3 * DAY_SECS, Some(SearchOutcome::Upgraded { bytes: 10 * GIB, at: NOW }))]
#[case::still_waiting(true, 3 * DAY_SECS, Some(SearchOutcome::Pending))]
#[case::nothing_found(true, 15 * DAY_SECS, Some(SearchOutcome::NothingFound { at: NOW }))]
#[case::unread_cutoff_settles_nothing(true, 15 * DAY_SECS, None)]
fn settling_reads_the_cutoff_list(#[case] still_unmet: bool, #[case] age: u64, #[case] outcome: Option<SearchOutcome>) {
    let world = World::new(&[(1, 0.9)]);
    let mut ledger = searched(&world, 1, NOW - age, Ok(()));
    let unmet: HashSet<String> = if still_unmet { HashSet::from(["radarr-1".to_string()]) } else { HashSet::new() };
    let sizes = HashMap::from([("radarr-1", 10 * GIB)]);
    ledger.settle(outcome.as_ref().map(|_| &unmet), &sizes, NOW);
    assert_eq!(ledger.searches[0].outcome, outcome.unwrap_or(SearchOutcome::Pending));
}

#[test]
fn a_gone_item_settles_as_gone() {
    let world = World::new(&[(1, 0.9)]);
    let mut ledger = searched(&world, 1, NOW - DAY_SECS, Ok(()));
    ledger.settle(Some(&HashSet::new()), &HashMap::from([("radarr-9", GIB)]), NOW);
    assert_eq!(ledger.searches[0].outcome, SearchOutcome::Gone { at: NOW });
}

#[test]
fn cutoff_pages_name_movies_and_seasons() {
    let radarr = json!({"page": 1, "totalRecords": 2, "records": [{"id": 7}, {"title": "no id"}]});
    assert_eq!(parse_cutoff_page(App::Radarr, &radarr), (vec![(7, None)], 2));
    let sonarr = json!({"totalRecords": 3, "records": [{"seriesId": 4, "seasonNumber": 2}, {"seriesId": 4}]});
    assert_eq!(parse_cutoff_page(App::Sonarr, &sonarr), (vec![(4, Some(2))], 3));
    assert_eq!(parse_cutoff_page(App::Radarr, &json!({})), (vec![], 0));
}

#[test]
fn unmet_rows_map_to_card_ids() {
    let items = HashMap::from([
        ("radarr-7".to_string(), ArrItem { app: App::Radarr, instance: "", id: 7, season: None, profile: None }),
        ("radarr@4k-7".to_string(), ArrItem { app: App::Radarr, instance: "4k", id: 7, season: None, profile: None }),
        ("sonarr-4-s2".to_string(), ArrItem { app: App::Sonarr, instance: "", id: 4, season: Some(2), profile: None }),
        ("sonarr-4-s3".to_string(), ArrItem { app: App::Sonarr, instance: "", id: 4, season: Some(3), profile: None }),
    ]);
    let cards = unmet_cards(&[(App::Radarr, "", 7, None), (App::Sonarr, "", 4, Some(2)), (App::Radarr, "", 8, None)], &items);
    assert_eq!(cards, HashSet::from(["radarr-7".to_string(), "sonarr-4-s2".to_string()]), "4k's movie 7 is another item");
    assert_eq!(unmet_cards(&[(App::Radarr, "4k", 7, None)], &items), HashSet::from(["radarr@4k-7".to_string()]));
}

#[test]
fn search_commands_follow_the_documented_shapes() {
    let world = World::new(&[(1, 0.9)]);
    let mut pick = world.select(&SearchLedger::default(), 5).picks.remove(0);
    assert_eq!(command(&pick), json!({"name": "MoviesSearch", "movieIds": [1]}));
    pick.season = Some(2);
    assert_eq!(command(&pick), json!({"name": "SeasonSearch", "seriesId": 1, "seasonNumber": 2}));
}

#[test]
fn headroom_is_the_room_below_the_target() {
    let forecast = CapacityForecast {
        current_used_bytes: 0,
        max_capacity_bytes: 1_000 * GIB,
        current_utilization: 0.0,
        daily_ingest_rate_bytes: 0,
        queue_bytes: 0,
        in_flight_bytes: 0,
        projected_used_bytes: 700 * GIB,
        target_reclaim_bytes: 0,
        is_emergency: false,
    };
    let full = CapacityForecast { projected_used_bytes: 900 * GIB, ..forecast.clone() };
    let volumes = [VolumeForecast { volume: "a".to_string(), forecast }, VolumeForecast { volume: "b".to_string(), forecast: full }];
    let room = volume_headroom(&volumes, 0.8, 50 * GIB);
    assert_eq!(room["a"], 50 * GIB);
    assert_eq!(room["b"], 0);
}

#[rstest]
#[case(0, false)]
#[case(5, true)]
#[case(51, false)]
fn the_daily_cap_is_bounded(#[case] max_per_day: u32, #[case] ok: bool) {
    assert_eq!(UpgradeSearchConfig { enabled: true, max_per_day }.validate().is_ok(), ok);
}
