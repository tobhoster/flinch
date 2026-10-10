use super::*;
use crate::arr::{ArrMovie, ArrSeries, SeriesSeason};
use crate::card::ArchiveCard;
use crate::signals::{MediaRef, Request, Signals};
use crate::watch::{WatchEntry, WatchSource};

fn movie_card() -> ArchiveCard {
    ArchiveCard { id: "radarr-1".to_string(), last_watched_days: Some(12.0), ..crate::golden::golden_movie() }
}

fn season_card() -> ArchiveCard {
    ArchiveCard { id: "sonarr-7-s2".to_string(), season_index: Some(2), last_watched_days: None, ..crate::golden::golden_season() }
}

fn movie() -> ArrMovie {
    ArrMovie {
        id: 1,
        tmdb_id: Some(949),
        path: Some("/data/movies/Heat (1995)".to_string()),
        tags: vec![3, 9],
        genres: vec!["Crime".to_string()],
        ..Default::default()
    }
}

fn show() -> ArrSeries {
    ArrSeries {
        id: 7,
        tvdb_id: Some(70),
        path: Some("/data/tv/Andor".to_string()),
        tags: vec![3],
        seasons: [1, 2].map(|n| SeriesSeason { season_number: n, ..Default::default() }).to_vec(),
        ..Default::default()
    }
}

fn gather_with(signals: &Signals, sonarr_tags: Option<&HashMap<u32, String>>) -> HashMap<String, Facts> {
    let cards = [movie_card(), season_card()];
    let (movies, series) = ([movie()], [show()]);
    // Only the movie has watch evidence.
    let watch = HashMap::from([(
        "radarr-1".to_string(),
        WatchEntry { id: "radarr-1".to_string(), last_watched_epoch: None, progress: 1.0, source: WatchSource::Plex },
    )]);
    let library = Library {
        cards: &cards,
        movies: &movies,
        series: &series,
        watch: &watch,
        plays: &HashMap::new(),
        located: &HashMap::new(),
        in_plex: &HashSet::new(),
        handed: &HashSet::new(),
        signals,
        taste: &HashMap::new(),
        cold_themes: &HashMap::new(),
        never_played: None,
        seeding: &HashMap::new(),
        now: 0,
    };
    let radarr_tags = HashMap::from([(3, "kids".to_string()), (9, "4k".to_string())]);
    // A second Radarr whose tag 3 is another label: never the default's.
    let other = HashMap::from([(3, "anime".to_string())]);
    let tags = [
        (crate::capacity::App::Radarr, String::new(), Some(radarr_tags)),
        (crate::capacity::App::Radarr, "4k".to_string(), Some(other)),
        (crate::capacity::App::Sonarr, String::new(), sonarr_tags.cloned()),
    ];
    let plex_ids =
        HashMap::from([("radarr-1".to_string(), PlexIds { rating_key: "5".into(), season_rating_key: None, section_id: Some(2) })]);
    let themes =
        Themes { names: vec!["Heists".to_string()], assignments: [("radarr-1".to_string(), 0)].into_iter().collect(), ..Themes::default() };
    gather(&library, &Sources { plex_ids: &plex_ids, tags: &tags, themes: &themes })
}

#[test]
fn each_card_gets_its_arr_plex_theme_watch_and_request_facts() {
    let signals = Signals {
        requests: vec![Request {
            media: MediaRef::Show { tvdb: Some(70), tmdb: None },
            seasons: vec![2],
            requester: "Ann".to_string(),
            requested_at: Some(100),
        }],
        requests_read: true,
        ..Signals::default()
    };
    let facts = gather_with(&signals, None);
    let heat = &facts["radarr-1"];
    assert_eq!(heat.kind, Some(Kind::Movie));
    assert_eq!(heat.path.as_deref(), Some("/data/movies/Heat (1995)"));
    assert_eq!(heat.tags, Some(vec!["kids".to_string(), "4k".to_string()]));
    assert_eq!((heat.plex_section, heat.theme.as_deref()), (Some(2), Some("Heists")));
    assert_eq!(heat.genres, Some(vec!["Crime".to_string()]));
    assert_eq!((heat.played, heat.last_played_days), (Some(true), Some(12.0)));
    assert_eq!(heat.requests, Some(Vec::new()), "Seerr was read and nobody asked for it");

    let andor = &facts["sonarr-7-s2"];
    assert_eq!((andor.kind, andor.path.as_deref()), (Some(Kind::Season), Some("/data/tv/Andor")));
    assert_eq!(andor.requests, Some(vec![RequestFact { requester: "Ann".to_string(), at: Some(100) }]));
    assert_eq!(andor.tags, None, "Sonarr's tags were unreadable: unknown, not none");
    assert_eq!(
        (andor.played, andor.quality.as_deref(), andor.theme.as_deref()),
        (None, None, None),
        "no watch evidence, no quality, no theme"
    );
}

#[test]
fn requests_are_unknown_until_seerr_was_read_in_full() {
    let facts = gather_with(&Signals::default(), Some(&HashMap::new()));
    assert_eq!(facts["sonarr-7-s2"].requests, None);
    assert_eq!(facts["sonarr-7-s2"].tags, Some(Vec::new()), "read, and the show has no known label");
}

#[test]
fn seasons_rank_newest_first_among_regular_seasons_on_disk() {
    let stats = |files: u32| crate::arr::SeasonStats { episode_file_count: files, ..Default::default() };
    // Specials and an empty season 5 take no place; season 1 is first though
    // only its later seasons are on disk.
    let seasons = [(0, 3), (1, 0), (2, 4), (3, 8), (4, 2), (5, 0)]
        .map(|(number, files)| SeriesSeason { season_number: number, statistics: stats(files), ..Default::default() })
        .to_vec();
    let andor = ArrSeries { seasons, status: Some("continuing".to_string()), ..show() };
    let placed = places(&andor);
    let rank = |number: u32| placed[&number].newest_rank;
    assert_eq!([rank(0), rank(1), rank(2), rank(3), rank(4), rank(5)], [None, None, Some(3), Some(2), Some(1), None]);
    assert!(placed[&1].first && !placed[&2].first && !placed[&0].first);
    assert!(placed.values().all(|place| place.continuing == Some(true)));

    let ended = places(&ArrSeries { status: Some("ended".to_string()), ..andor.clone() });
    assert_eq!(ended[&4].continuing, Some(false));
    assert_eq!(places(&ArrSeries { status: None, ..andor })[&4].continuing, None, "no status is unknown, not ended");
}
