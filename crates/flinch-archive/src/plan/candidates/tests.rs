use super::*;
use crate::arr::SeriesSeason;
use crate::daemon::NeverPlayedHold;
use crate::signals::{Request, Watchlisted};
use crate::watch::WatchSource;

const NOW: u64 = 2_000_000_000;

fn season_card(series: u32, season: u32, last_watched_days: Option<f32>) -> ArchiveCard {
    ArchiveCard {
        id: format!("sonarr-{series}-s{season}"),
        season_index: Some(season),
        last_watched_days,
        ..crate::golden::golden_season()
    }
}

fn movie_card(id: u32) -> ArchiveCard {
    ArchiveCard { id: format!("radarr-{id}"), ..crate::golden::golden_movie() }
}

fn show(id: u32, tvdb: u32, seasons: &[u32]) -> ArrSeries {
    ArrSeries {
        id,
        tvdb_id: Some(tvdb),
        seasons: seasons.iter().map(|n| SeriesSeason { season_number: *n, ..Default::default() }).collect(),
        ..Default::default()
    }
}

fn entry(id: &str, progress: f32) -> (String, WatchEntry) {
    (id.to_string(), WatchEntry { id: id.to_string(), last_watched_epoch: None, progress, source: WatchSource::Plex })
}

struct World {
    cards: Vec<ArchiveCard>,
    movies: Vec<ArrMovie>,
    series: Vec<ArrSeries>,
    watch: HashMap<String, WatchEntry>,
    in_plex: HashSet<String>,
    signals: Signals,
    never_played: Option<Exclusion>,
    taste: HashMap<String, Reading>,
    seeding: HashMap<String, crate::torrents::SeedHold>,
}

impl World {
    fn new(cards: Vec<ArchiveCard>) -> Self {
        let watch = cards.iter().map(|card| entry(&card.id, if card.last_watched_days.is_some() { 1.0 } else { 0.0 })).collect();
        let in_plex = cards.iter().map(|card| card.id.clone()).collect();
        Self {
            cards,
            movies: Vec::new(),
            series: Vec::new(),
            watch,
            in_plex,
            signals: Signals::default(),
            never_played: None,
            taste: HashMap::new(),
            seeding: HashMap::new(),
        }
    }

    fn build(&self, config: &PlannerConfig) -> HashMap<String, MediaCandidate> {
        let located = self.cards.iter().map(|card| (card.id.clone(), "disk".to_string())).collect();
        let library = Library {
            cards: &self.cards,
            movies: &self.movies,
            series: &self.series,
            watch: &self.watch,
            plays: &HashMap::new(),
            located: &located,
            in_plex: &self.in_plex,
            handed: &HashSet::new(),
            signals: &self.signals,
            taste: &self.taste,
            cold_themes: &HashMap::new(),
            never_played: self.never_played.clone(),
            seeding: &self.seeding,
            now: NOW,
        };
        build(&library, config, &HazardModel::default()).into_iter().map(|candidate| (candidate.id.clone(), candidate)).collect()
    }
}

#[test]
fn the_first_rule_that_applies_keeps_an_item_out() {
    let favorite = ArchiveCard { is_favorite: true, ..movie_card(1) };
    let unknown_to_plex = movie_card(2);
    let no_evidence = movie_card(3);
    let never_played = ArchiveCard { last_watched_days: None, ..movie_card(4) };
    let played = movie_card(5);
    let mut world = World::new(vec![favorite, unknown_to_plex, no_evidence, never_played, played]);
    world.in_plex.remove("radarr-1");
    world.in_plex.remove("radarr-2");
    world.watch.remove("radarr-3");
    world.never_played = Some(Exclusion::NeverPlayedHeld(NeverPlayedHold::IncompleteEvidence));
    let built = world.build(&PlannerConfig::default());

    assert_eq!(built["radarr-1"].exclusion, Some(Exclusion::Pinned(Pin::Favorite)), "a pin outranks a missing Plex id");
    assert!(built["radarr-1"].protect);
    assert_eq!(built["radarr-2"].exclusion, Some(Exclusion::NotInPlex));
    assert_eq!(built["radarr-3"].exclusion, Some(Exclusion::NoWatchEvidence));
    assert_eq!(built["radarr-4"].exclusion, Some(Exclusion::NeverPlayedHeld(NeverPlayedHold::IncompleteEvidence)));
    assert_eq!(built["radarr-5"].exclusion, None);
    assert!(!built["radarr-5"].announce, "finished: straight to its delete collection");
    assert!(built["radarr-4"].announce, "nobody finished it: announced first");
}

#[test]
fn a_torrent_hold_keeps_an_otherwise_eligible_item_and_a_pin_still_names_itself() {
    use crate::torrents::SeedHold;
    let below = SeedHold::BelowGoal { ratio_centi: 42, seeding_days: 3 };
    let mut world = World::new(vec![movie_card(1), ArchiveCard { is_favorite: true, ..movie_card(2) }, movie_card(3)]);
    world.seeding = HashMap::from([("radarr-1".to_string(), below), ("radarr-2".to_string(), SeedHold::HeldByTorrent)]);
    let built = world.build(&PlannerConfig::default());
    assert_eq!(built["radarr-1"].exclusion, Some(Exclusion::Seeding(below)));
    assert_eq!(
        built["radarr-1"].exclusion.as_ref().map(ToString::to_string).as_deref(),
        Some("Seeding: ratio 0.42 after 3 day(s), below its seed goal")
    );
    assert_eq!(built["radarr-2"].exclusion, Some(Exclusion::Pinned(Pin::Favorite)));
    assert_eq!(built["radarr-3"].exclusion, None);
}

#[test]
fn seasons_join_their_show_in_order_and_movies_stand_alone() {
    let world = World::new(vec![season_card(7, 2, None), season_card(7, 1, Some(100.0)), movie_card(1)]);
    let built = world.build(&PlannerConfig::default());
    assert_eq!(built["sonarr-7-s1"].sequence, Some(Sequence { group: "sonarr-7".to_string(), index: 1, played: true }));
    assert_eq!(built["sonarr-7-s2"].sequence, Some(Sequence { group: "sonarr-7".to_string(), index: 2, played: false }));
    assert_eq!(built["radarr-1"].sequence, None);
}

#[test]
fn a_user_who_requested_and_watchlisted_a_season_claims_it_once_with_both() {
    let mut world = World::new(vec![season_card(7, 1, Some(300.0)), season_card(7, 2, Some(300.0))]);
    world.series = vec![show(7, 70, &[1, 2])];
    let media = MediaRef::Show { tvdb: Some(70), tmdb: None };
    world.signals.requests = vec![Request { media, seasons: vec![1], requester: "Ann".to_string(), requested_at: None }];
    world.signals.watchlists = vec![Watchlisted { media, user: "ann".to_string() }, Watchlisted { media, user: "Bo".to_string() }];
    let config = PlannerConfig { user_weights: BTreeMap::from([("ANN".to_string(), 2.0)]), ..PlannerConfig::default() };
    let built = world.build(&config);
    // Seerr names users case-insensitively too: "Ann" and "ann" are one user.
    assert_eq!(built["sonarr-7-s1"].regret.household, 1.0 + 2.0 * 3.5, "requested season: watchlist + request");
    assert_eq!(built["sonarr-7-s2"].regret.household, 1.0 + 2.0 * 2.0, "the request named season 1 only");
}

#[test]
fn a_never_played_title_with_a_taste_names_the_titles_it_resembles() {
    let unplayed = ArchiveCard { last_watched_days: None, ..movie_card(4) };
    let paddington = ArchiveCard { title: "Paddington".to_string(), ..movie_card(8) };
    let mut world = World::new(vec![unplayed, paddington, movie_card(5)]);
    world.movies = vec![ArrMovie { id: 7, title: "Hot Fuzz".to_string(), ..Default::default() }];
    let like =
        crate::taste::Likeness { subjects: ["radarr-99", "radarr-7", "radarr-8", "radarr-5"].map(String::from).to_vec(), played: true };
    let reading = Reading { taste: 0.8, like: Some(like) };
    world.taste = HashMap::from([("radarr-4".to_string(), reading.clone()), ("radarr-5".to_string(), reading)]);
    let built = world.build(&PlannerConfig::default());

    assert!(built["radarr-4"].reason.ends_with(" · like Hot Fuzz, Paddington (played here)"), "{}", built["radarr-4"].reason);
    assert!(!built["radarr-5"].reason.contains("like"), "played: taste does not speak for it: {}", built["radarr-5"].reason);
}

#[test]
fn a_season_of_a_show_that_streams_costs_less_to_lose_and_says_where() {
    use crate::signals::streaming::{Stream, Title};
    let mut world = World::new(vec![season_card(7, 1, Some(300.0)), season_card(8, 1, Some(300.0))]);
    world.series = vec![ArrSeries { tmdb_id: Some(77), ..show(7, 70, &[1]) }, ArrSeries { tmdb_id: Some(88), ..show(8, 80, &[1]) }];
    let before = world.build(&PlannerConfig::default());
    world.signals.streams = HashMap::from([(Title::Tv(77), Stream { provider: "Netflix".to_string(), region: "DE".to_string() })]);
    let after = world.build(&PlannerConfig::default());
    assert!(after["sonarr-7-s1"].regret.friction < before["sonarr-7-s1"].regret.friction);
    assert!(after["sonarr-7-s1"].regret.friction >= crate::regret::MIN_FRICTION);
    assert!(after["sonarr-7-s1"].reason.ends_with(" · streams on Netflix (DE)"), "{}", after["sonarr-7-s1"].reason);
    assert_eq!(after["sonarr-8-s1"].regret, before["sonarr-8-s1"].regret, "unknown availability: no discount");
    assert!(!after["sonarr-8-s1"].reason.contains("streams"));
}
