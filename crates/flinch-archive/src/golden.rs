//! Golden fixture cards for tests and the `--demo` mode.
//!
//! Sized to be obviously realistic: the MVP season is ~1.5 GB (a 6-episode
//! 1080p season), the rewatch movie 4.2 GB BR, a 2160p season 8.1 GB.

use crate::card::{ArchiveCard, LibraryKind, SeasonState, SeriesType};

pub fn golden_season() -> ArchiveCard {
    ArchiveCard {
        id: "season-mvp".to_string(),
        title: "The MVP Sessions".to_string(),
        kind: LibraryKind::Season,
        size_bytes: 1_500_000_000,
        added_days_ago: 500.0,
        last_watched_days: Some(200.0),
        in_keep_collection: false,
        is_favorite: false,
        duplicate_count: 0,
        series_type: Some(SeriesType::Standard),
        season_state: Some(SeasonState::Completed),
        season_index: Some(1),
        is_newest_season: Some(false),
        episodes_total: Some(6),
        episodes_watched: Some(6),
        is_watched: None,
        rewatch_score: None,
        movie_year: Some(2024),
        show_title: Some("The MVP Sessions".to_string()),
    }
}

pub fn golden_movie() -> ArchiveCard {
    ArchiveCard {
        id: "movie-rewatch".to_string(),
        title: "The Rewatchable BR".to_string(),
        kind: LibraryKind::Movie,
        size_bytes: 4_200_000_000,
        added_days_ago: 900.0,
        last_watched_days: Some(310.0),
        in_keep_collection: false,
        is_favorite: false,
        duplicate_count: 0,
        series_type: None,
        season_state: None,
        season_index: None,
        is_newest_season: None,
        episodes_total: None,
        episodes_watched: None,
        is_watched: Some(true),
        rewatch_score: Some(0.2),
        movie_year: Some(2016),
        show_title: None,
    }
}
