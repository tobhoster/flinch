//! `/api/v3/history` records, reduced to what [`crate::presence`] needs.
//!
//! Wire shape, from the Radarr and Sonarr sources (`develop`, read 2026-09-23):
//! the API serialises enums as camelCase strings and dictionary keys camelCase
//! (`STJson.cs` lines 29-33), so a file deletion reads
//! `"eventType":"movieFileDeleted"` with `"data":{"reason":"MissingFromDisk"}`
//! (the reason is `DeleteMediaFileReason.ToString()`). Only new downloads write
//! an import record (`HistoryService.cs`: Radarr line 182, Sonarr line 192);
//! a disk scan or an import of a file already in the library writes none.
//! `movieFolderImported` is "not used yet" (Radarr `History.cs` line 47) and
//! Sonarr never writes `seriesFolderImported`; both read as imports anyway.

use crate::presence::{self, Change, FileEvent};
use serde::Deserialize;

/// One page of `/api/v3/history` (`PagingResource<HistoryResource>`). Records
/// stay raw so one malformed row is skipped, not the page.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    #[serde(default)]
    pub total_records: u64,
    #[serde(default)]
    pub records: Vec<serde_json::Value>,
}

/// One `HistoryResource`. Radarr fills `movieId`; Sonarr fills `seriesId` and
/// `episodeId`, and `episode` when asked with `includeEpisode=true`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRecord {
    pub id: u64,
    pub date: String,
    pub event_type: EventType,
    #[serde(default)]
    pub movie_id: Option<u32>,
    #[serde(default)]
    pub series_id: Option<u32>,
    #[serde(default)]
    pub episode_id: Option<u32>,
    #[serde(default)]
    pub episode: Option<HistoryEpisode>,
    #[serde(default)]
    pub data: Option<HistoryData>,
}

/// `MovieHistoryEventType` / `EpisodeHistoryEventType`, the members that move files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventType {
    DownloadFolderImported,
    MovieFolderImported,
    SeriesFolderImported,
    MovieFileDeleted,
    EpisodeFileDeleted,
    /// Grabs, failures, renames, ignored downloads: no file moved.
    #[serde(other)]
    Other,
}

/// `DeleteMediaFileReason`, identical in both apps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum DeleteReason {
    MissingFromDisk,
    Manual,
    Upgrade,
    NoLinkedEpisodes,
    ManualOverride,
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEpisode {
    pub season_number: u32,
}

#[derive(Debug, Deserialize)]
pub struct HistoryData {
    #[serde(default)]
    pub reason: Option<DeleteReason>,
}

impl HistoryRecord {
    /// The card this record is about and what it did to that card's files;
    /// `None` when it moved no file or cannot be placed or dated.
    pub fn file_event(&self) -> Option<(String, FileEvent)> {
        let change = match self.event_type {
            EventType::DownloadFolderImported | EventType::MovieFolderImported | EventType::SeriesFolderImported => {
                Change::Imported
            }
            EventType::MovieFileDeleted | EventType::EpisodeFileDeleted => match self.data.as_ref().and_then(|data| data.reason) {
                // The old file goes as the new one lands (`UpgradeMediaFileService`),
                // or a re-import takes over its record (`ManualOverride`).
                Some(DeleteReason::Upgrade | DeleteReason::ManualOverride) => Change::Replaced,
                Some(DeleteReason::MissingFromDisk | DeleteReason::Manual | DeleteReason::NoLinkedEpisodes | DeleteReason::Other)
                | None => Change::Removed,
            },
            EventType::Other => return None,
        };
        let at = presence::parse_utc(&self.date)?;
        let (card, episode) = match (self.movie_id.filter(|id| *id > 0), self.series_id.filter(|id| *id > 0)) {
            (Some(movie), _) => (format!("radarr-{movie}"), None),
            (None, Some(series)) => {
                let season = self.episode.as_ref()?.season_number;
                (format!("sonarr-{series}-s{season}"), Some(self.episode_id?))
            }
            (None, None) => return None,
        };
        Some((card, FileEvent { at, record: self.id, episode, change }))
    }
}
