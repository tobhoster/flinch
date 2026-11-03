//! Why a Radarr or Sonarr queue item is stuck, and whether removing it is
//! safe.
//!
//! Wire shape (`GET /api/v3/queue?includeSeries=true` / `includeMovie=true`,
//! Sonarr v4 and Radarr v5): a `PagingResource<QueueResource>`. Each record
//! carries the download client's `status` (`downloading`, `completed`,
//! `failed`, `warning`, …), the app's `trackedDownloadState`
//! (`downloading`, `importBlocked`, `importPending`, `importing`,
//! `failedPending`, `failed`) and `trackedDownloadStatus` (`ok`, `warning`,
//! `error`), and `statusMessages[].messages`: the reasons in words, such as
//! "Not enough free space". Sonarr lists one record per episode of a
//! download, each with its own id; removing one removes the download.

use crate::capacity::App;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Messages that mean the item can never import as it is. Matched in lower
/// case anywhere in a status message or the client's error.
const UNIMPORTABLE: &[&str] = &[
    "no files found are eligible",
    "no files eligible",
    "executable",
    "sample",
    "unknown series",
    "unknown movie",
    "not enough repair blocks",
    "repair failed",
    "missing files",
    "files are missing",
    "file is missing",
    "no video files",
];

/// Why an item is listed. Only [`Reason::Unimportable`] may be removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// The library disk is full; the app imports it once there is room.
    NoSpace,
    /// Missing or fake files, a sample only, an unknown title, a failed
    /// download: it will never import.
    Unimportable,
    /// Downloading, with no progress for longer than the setting allows.
    StalledDownload,
    /// The app flags it for a reason FLINCH does not judge.
    Other,
}

impl Reason {
    /// Removal (with blocklisting) is safe: nothing is lost that could still
    /// import.
    pub fn removable(self) -> bool {
        match self {
            Reason::Unimportable => true,
            Reason::NoSpace | Reason::StalledDownload | Reason::Other => false,
        }
    }

    /// The state in words, for problems and refusals.
    pub fn describe(self) -> &'static str {
        match self {
            Reason::NoSpace => "wait for free space and import once the disk has room",
            Reason::Unimportable => "can never import (missing or fake files, unknown title, failed download)",
            Reason::StalledDownload => "stopped downloading",
            Reason::Other => "are flagged by the app",
        }
    }
}

/// One `QueueResource`, the fields the fleet reads. Any may be null.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct QueueRecord {
    pub id: u64,
    pub title: Option<String>,
    /// The download client's state.
    pub status: Option<String>,
    pub tracked_download_status: Option<String>,
    pub tracked_download_state: Option<String>,
    pub status_messages: Option<Vec<StatusMessage>>,
    pub error_message: Option<String>,
    pub size: Option<f64>,
    pub sizeleft: Option<f64>,
    pub added: Option<String>,
    pub series_id: Option<u64>,
    pub movie_id: Option<u64>,
    pub series: Option<LibraryItem>,
    pub movie: Option<LibraryItem>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StatusMessage {
    pub messages: Option<Vec<String>>,
}

/// The series or movie a record belongs to (`includeSeries`/`includeMovie`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LibraryItem {
    pub title: Option<String>,
    pub path: Option<String>,
    pub root_folder_path: Option<String>,
}

impl QueueRecord {
    /// Every reason the app and the download client give, in order.
    pub fn messages(&self) -> Vec<String> {
        let listed = self.status_messages.iter().flatten().flat_map(|message| message.messages.iter().flatten());
        listed.chain(self.error_message.iter()).map(|message| message.trim().to_string()).filter(|message| !message.is_empty()).collect()
    }

    fn state(&self) -> &str {
        self.tracked_download_state.as_deref().unwrap_or("")
    }

    fn download_status(&self) -> &str {
        self.status.as_deref().unwrap_or("")
    }

    /// The app flags it: a warning or error, or a state that waits on
    /// someone.
    fn flagged(&self) -> bool {
        matches!(self.tracked_download_status.as_deref(), Some("warning" | "error"))
            || matches!(self.state(), "importBlocked" | "failedPending" | "failed")
            || self.download_status() == "failed"
    }

    fn failed(&self) -> bool {
        matches!(self.state(), "failedPending" | "failed") || self.download_status() == "failed"
    }

    /// Still in the download client, which could be making progress.
    fn downloading(&self) -> bool {
        matches!(self.state(), "" | "downloading") && matches!(self.download_status(), "downloading" | "warning")
    }

    /// Completed, but the app cannot tell which series or movie it is.
    fn unknown_item(&self) -> bool {
        let unknown = self.series_id.unwrap_or(0) == 0 && self.movie_id.unwrap_or(0) == 0 && self.series.is_none() && self.movie.is_none();
        unknown && matches!(self.state(), "importPending" | "importBlocked")
    }

    fn item(&self) -> Option<&LibraryItem> {
        self.series.as_ref().or(self.movie.as_ref())
    }

    /// Where the import goes: the root folder, else the folder holding the
    /// item's own.
    pub fn target_path(&self) -> Option<String> {
        let item = self.item()?;
        let root = item.root_folder_path.as_deref().map(str::trim).filter(|root| !root.is_empty());
        let parent = || {
            let path = item.path.as_deref()?.trim().trim_end_matches('/');
            path.rsplit_once('/').map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
        };
        root.or_else(parent).map(str::to_string)
    }
}

/// Why `record` is stuck, or `None` while it is not. `stalled_secs` is how
/// long its download has made no progress. Free space outranks everything:
/// such an item imports by itself and is never offered for removal.
pub fn classify(record: &QueueRecord, stalled_secs: u64, stalled_after_secs: u64) -> Option<Reason> {
    let messages: Vec<String> = record.messages().iter().map(|message| message.to_lowercase()).collect();
    let says = |patterns: &[&str]| messages.iter().any(|message| patterns.iter().any(|pattern| message.contains(pattern)));
    if says(&["not enough free space"]) {
        return Some(Reason::NoSpace);
    }
    if record.failed() || record.unknown_item() || (record.flagged() && says(UNIMPORTABLE)) {
        return Some(Reason::Unimportable);
    }
    if record.downloading() && stalled_secs >= stalled_after_secs {
        return Some(Reason::StalledDownload);
    }
    record.flagged().then_some(Reason::Other)
}

/// One instance's queue as published.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QueueStatus {
    /// Every record the app lists, stuck or not.
    pub total: u64,
    pub stalled: Vec<StalledItem>,
}

/// One stuck queue record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StalledItem {
    /// The app's queue id: what a removal names.
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub series_or_movie: Option<String>,
    /// `trackedDownloadState`.
    pub state: String,
    /// `trackedDownloadStatus`.
    pub status: String,
    pub messages: Vec<String>,
    pub reason: Reason,
    pub removable: bool,
    /// Unix seconds: queued at, or for a stalled download its last progress.
    pub since: u64,
    pub size_bytes: u64,
    #[serde(default)]
    pub target_path: Option<String>,
}

/// One download's progress as last seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub size_left: u64,
    /// Unix seconds of the last read that found fewer bytes left.
    pub progressed_at: u64,
}

/// `fleet-queue.json`: each instance's queue records and when each last
/// progressed, so a stalled download is told from a slow one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct QueueMemory {
    /// Instance key (`sonarr`, `radarr@4k`) → queue id → progress.
    pub instances: BTreeMap<String, BTreeMap<u64, Progress>>,
}

impl QueueMemory {
    /// Classify one instance's freshly read queue and remember it in place of
    /// the last read. An instance left unread keeps its memory.
    pub fn judge(&mut self, instance_key: &str, records: &[QueueRecord], stalled_after_secs: u64, now: u64) -> Vec<StalledItem> {
        let last = self.instances.remove(instance_key).unwrap_or_default();
        let mut seen = BTreeMap::new();
        let mut stalled = Vec::new();
        for record in records {
            // A float-to-int `as` saturates: negative or NaN reads as 0.
            let size_left = record.sizeleft.unwrap_or(0.0) as u64;
            let progressed_at = match last.get(&record.id) {
                Some(before) if size_left >= before.size_left => before.progressed_at,
                _ => now,
            };
            seen.insert(record.id, Progress { size_left, progressed_at });
            if let Some(reason) = classify(record, now.saturating_sub(progressed_at), stalled_after_secs) {
                stalled.push(stalled_item(record, reason, progressed_at, now));
            }
        }
        self.instances.insert(instance_key.to_string(), seen);
        stalled
    }
}

fn stalled_item(record: &QueueRecord, reason: Reason, progressed_at: u64, now: u64) -> StalledItem {
    let queued_at = record.added.as_deref().and_then(crate::presence::parse_utc).unwrap_or(now);
    StalledItem {
        id: record.id,
        title: record.title.clone().unwrap_or_default(),
        series_or_movie: record.item().and_then(|item| item.title.clone()),
        state: record.state().to_string(),
        status: record.tracked_download_status.clone().unwrap_or_default(),
        messages: record.messages(),
        reason,
        removable: reason.removable(),
        since: if reason == Reason::StalledDownload { progressed_at } else { queued_at },
        size_bytes: record.size.unwrap_or(0.0) as u64,
        target_path: record.target_path(),
    }
}

/// The `/api/v3/queue` query of page `page`, with each record's series or
/// movie and the records the app cannot place (those never import).
pub fn queue_query(app: App, page: usize, page_size: usize) -> Vec<(&'static str, String)> {
    let (include, unknown) = match app {
        App::Radarr => ("includeMovie", "includeUnknownMovieItems"),
        App::Sonarr => ("includeSeries", "includeUnknownSeriesItems"),
    };
    vec![("page", page.to_string()), ("pageSize", page_size.to_string()), (include, "true".into()), (unknown, "true".into())]
}
