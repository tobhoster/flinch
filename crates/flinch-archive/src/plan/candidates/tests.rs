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
            never_played: self.never_played,
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
    world.signals.requests = vec![Request { media, seasons: vec![1], requester: "Ann".to_string() }];
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
