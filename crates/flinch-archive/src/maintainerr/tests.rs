//! Shared fixtures for the Maintainerr sync tests.

use super::{CollectionInfo, CollectionTitles, ExclusionRow, MaintainerrTarget, MaintainerrVersion, SyncItem};
use crate::card::LibraryKind;
use crate::ids::PlexIds;

mod client;
mod execute;
mod fake;
mod plan;
mod props;
mod state;
mod wire;

pub(super) const GIB: u64 = 1 << 30;
pub(super) const MOVIES: i64 = 10;
pub(super) const SEASONS: i64 = 20;

pub(super) fn movie_ids(rating_key: &str) -> PlexIds {
    PlexIds { rating_key: rating_key.to_string(), season_rating_key: None, section_id: Some(1) }
}

pub(super) fn season_ids(show: &str, season: &str) -> PlexIds {
    PlexIds { rating_key: show.to_string(), season_rating_key: Some(season.to_string()), section_id: Some(2) }
}

pub(super) fn movie(rating_key: &str) -> MaintainerrTarget {
    MaintainerrTarget::Movie { rating_key: rating_key.to_string() }
}

pub(super) fn season(show: &str, season: &str) -> MaintainerrTarget {
    MaintainerrTarget::Season { show_rating_key: show.to_string(), season_rating_key: season.to_string() }
}

pub(super) fn item(card_id: &str, kind: LibraryKind, plex: Option<PlexIds>, bytes: u64) -> SyncItem {
    SyncItem { card_id: card_id.to_string(), kind, plex, copies: Vec::new(), bytes }
}

pub(super) fn collection(id: i64, title: &str, media_type: &str, library_id: &str, is_active: bool, arr_action: i64) -> CollectionInfo {
    CollectionInfo {
        id,
        title: title.to_string(),
        media_type: media_type.to_string(),
        library_id: library_id.to_string(),
        is_active,
        arr_action,
        delete_after_days: None,
        visible_on_home: false,
        visible_on_recommended: false,
        keep_in_maintainerr_only: false,
        overlay_enabled: false,
        force_seerr: false,
    }
}

/// A Leaving Soon collection that really warns: shown on Plex's home screen,
/// acting 14 days after an item joins.
pub(super) fn leaving(id: i64, media_type: &str, library_id: &str) -> CollectionInfo {
    CollectionInfo { delete_after_days: Some(14), visible_on_home: true, ..collection(id, "Leaving Soon", media_type, library_id, true, 0) }
}

pub(super) fn titles() -> CollectionTitles {
    CollectionTitles { movie: "FLINCH Movies".to_string(), season: "FLINCH Seasons".to_string(), leaving: String::new() }
}

/// One valid collection per kind: movies in Plex section 1, seasons in 2.
pub(super) fn valid_collections() -> Vec<CollectionInfo> {
    vec![
        collection(MOVIES, "FLINCH Movies", "movie", "1", true, 0),
        collection(SEASONS, "FLINCH Seasons", "season", "2", true, 0),
    ]
}

/// A global exclusion row for `media_server_id`, created by a call on `parent`.
pub(super) fn row(id: i64, media_server_id: &str, parent: &str) -> ExclusionRow {
    ExclusionRow {
        id,
        media_server_id: media_server_id.to_string(),
        rule_group_id: None,
        parent: Some(parent.to_string()),
        media_type: None,
    }
}

pub(super) fn current() -> MaintainerrVersion {
    MaintainerrVersion::Release { major: 3, minor: 29, patch: 0 }
}
