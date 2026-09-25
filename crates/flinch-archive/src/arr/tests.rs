use super::dwell::now_epoch;
use super::*;

#[test]
fn radarr_movie_maps_to_a_card_with_file_only() {
    let movie = ArrMovie {
        id: 7,
        title: "Arrival".to_string(),
        title_slug: Some("329865".to_string()),
        year: Some(2016),
        size_on_disk: 5_000_000_000,
        has_file: true,
        added: Some("2024-03-01T00:00:00Z".to_string()),
        images: vec![ArrImage { cover_type: "poster".into(), remote_url: Some("https://image.tmdb.org/t/p/w300/x.jpg".into()), url: None }],
        movie_file: Some(MovieFile { quality: Some(Quality { quality: QualityName { name: "Bluray-1080p".into() } }), date_added: None }),
        path: Some("/media/movies/Arrival (2016)".to_string()),
        ..Default::default()
    };
    let card = movie.to_card().expect("file present");
    assert_eq!(card.id, "radarr-7");
    assert_eq!(card.size_bytes, 5_000_000_000);
    assert!(card.last_watched_days.is_none(), "watch state is external");

    let mut missing = movie;
    missing.has_file = false;
    assert!(missing.to_card().is_none(), "no file, no card");
}

#[test]
fn sonarr_series_flattens_to_one_card_per_season_and_marks_newest() {
    let series = ArrSeries {
        id: 11,
        title: "Witcher".to_string(),
        title_slug: Some("the-witcher".to_string()),
        year: Some(2019),
        series_type: "standard".to_string(),
        added: Some("2023-01-01T00:00:00Z".to_string()),
        seasons: vec![
            SeriesSeason {
                season_number: 1,
                statistics: SeasonStats { episode_file_count: 8, episode_count: 8, total_episode_count: 8, size_on_disk: 1_000_000_000 },
                ..Default::default()
            },
            SeriesSeason {
                season_number: 2,
                statistics: SeasonStats { episode_file_count: 8, episode_count: 8, total_episode_count: 8, size_on_disk: 1_200_000_000 },
                ..Default::default()
            },
            SeriesSeason {
                season_number: 3,
                statistics: SeasonStats { episode_file_count: 0, episode_count: 8, total_episode_count: 8, size_on_disk: 0 },
                ..Default::default()
            },
        ],
        images: vec![],
        path: None,
        status: Some("ended".to_string()),
        previous_airing: Some("2023-12-01T08:00:00Z".to_string()),
        ..Default::default()
    };
    let cards = series.to_cards();
    assert_eq!(cards.len(), 2, "empty season 3 is skipped");
    let s1 = &cards[0];
    assert_eq!(s1.id, "sonarr-11-s1");
    assert_eq!(s1.is_newest_season, Some(false));
    assert_eq!(cards[1].is_newest_season, Some(true), "S2 is the newest with files");
    assert!(s1.season_index == Some(1));
}

#[test]
fn a_future_dated_added_string_maps_to_a_huge_age_not_a_panic() {
    let movie = ArrMovie {
        id: 1,
        title: "x".to_string(),
        title_slug: None,
        year: None,
        size_on_disk: 1,
        has_file: true,
        added: Some("9999-12-31".to_string()),
        images: vec![],
        movie_file: None,
        path: None,
        ..Default::default()
    };
    let card = movie.to_card().expect("card");
    assert!(card.added_days_ago.is_finite(), "chrono_lite must not panic on wild dates");
}

#[test]
fn the_null_date_sentinel_reads_as_absent_not_as_an_underflow() {
    // Sonarr/Radarr write 0001-01-01 for "no date"; `year - 1970` on a u64
    // used to panic inside the daemon loop.
    assert_eq!(chrono_lite("0001-01-01T00:00:00Z"), None);
    assert_eq!(chrono_lite("1969-12-31T00:00:00Z"), None);
    assert!(chrono_lite("2023-12-01T08:00:00Z").is_some());
}

#[test]
fn null_statistics_read_as_empty_instead_of_failing_the_library() {
    let movie: ArrMovie = serde_json::from_str(r#"{"id":3,"title":"Unmeasured","year":2024,"sizeOnDisk":null,"hasFile":null}"#)
        .expect("a null-statistics row still parses");
    assert!(movie.to_card().is_none(), "unmeasured means no file: nothing to reclaim");

    let show: ArrSeries =
        serde_json::from_str(r#"{"id":4,"title":"New Show","seriesType":"standard","seasons":[{"seasonNumber":1,"statistics":null}]}"#)
            .expect("a null season statistics block still parses");
    assert!(show.to_cards().is_empty(), "no files measured, no season card");
}

#[test]
fn a_movie_ages_from_when_its_file_arrived_not_when_it_was_requested() {
    let movie = ArrMovie {
        id: 5,
        title: "Waited Years".to_string(),
        size_on_disk: 1,
        has_file: true,
        added: Some("2020-01-01T00:00:00Z".to_string()),
        movie_file: Some(MovieFile { quality: None, date_added: Some("2026-12-31T00:00:00Z".to_string()) }),
        ..Default::default()
    };
    let card = movie.to_card().expect("file present");
    let requested_days = (now_epoch() - chrono_lite("2020-01-01").expect("date")) as f32 / 86_400.0;
    assert!(card.added_days_ago < requested_days - 1000.0, "dwell starts at the file, got {}", card.added_days_ago);
}

#[test]
fn an_unknown_arrival_is_fresh_so_dwell_cannot_be_assumed() {
    let movie = ArrMovie { id: 6, title: "No Date".to_string(), size_on_disk: 1, has_file: true, ..Default::default() };
    assert_eq!(movie.to_card().expect("file present").added_days_ago, 0.0);
}

#[test]
fn seasons_age_from_their_own_newest_file_and_count_only_episodes_on_disk() {
    let show = ArrSeries {
        id: 12,
        title: "Late Grab".to_string(),
        series_type: "standard".to_string(),
        added: Some("2023-01-01T00:00:00Z".to_string()),
        seasons: vec![SeriesSeason {
            season_number: 4,
            statistics: SeasonStats { episode_file_count: 6, episode_count: 8, total_episode_count: 10, size_on_disk: 5 },
            files_added: Some("2026-12-01T00:00:00Z".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let card = &show.to_cards()[0];
    assert!(card.added_days_ago < 100.0, "S4 arrived recently, not with the 2023 series");
    assert_eq!(card.episodes_total, Some(6), "unaired and missing episodes never count");
}

#[test]
fn dwell_runs_from_the_span_covering_now_else_from_the_current_file() {
    use crate::presence::Span;
    let day = 86_400;
    // Upgraded five days ago; on disk for 200 days.
    let mut movie = ArrMovie {
        id: 9,
        title: "Upgraded".to_string(),
        size_on_disk: 1,
        has_file: true,
        movie_file: Some(MovieFile { quality: None, date_added: Some("2027-01-10T00:00:00Z".to_string()) }),
        on_disk: vec![Span { from: now_epoch() - 200 * day, to: None }],
        ..Default::default()
    };
    assert_eq!(movie.to_card().expect("file present").added_days_ago, 200.0);
    // A span that ended long ago says nothing about the file on disk now.
    movie.on_disk = vec![Span { from: now_epoch() - 300 * day, to: Some(now_epoch() - 250 * day) }];
    assert!(movie.to_card().expect("file present").added_days_ago < 10.0, "dwell starts at the current file");
}

#[test]
fn the_operators_keep_tag_is_a_hard_guard() {
    let movie = ArrMovie { id: 8, title: "Kept".to_string(), size_on_disk: 1, has_file: true, keep: true, ..Default::default() };
    assert!(movie.to_card().expect("file present").is_favorite);
}
