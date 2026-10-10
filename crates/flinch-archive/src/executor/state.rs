//! What the native executor remembers between cycles (`native.json`), what it
//! publishes (`status.native`), and the undo queue the web UI fills
//! (`restore/<card id>`, one file per request so the daemon and the web never
//! rewrite each other's file).

use super::DeleteMode;
use crate::card::LibraryKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// How long a native delete can be undone from the UI, and stays listed.
pub const RESTORE_WINDOW_SECS: u64 = 30 * 86_400;

/// An item on the Leaving Soon shelf, verified there by read-back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Leaving {
    pub title: String,
    pub kind: LibraryKind,
    pub bytes: u64,
    pub announced_at: u64,
    /// The window's end: deleted no earlier, and only if still unplayed.
    pub until: u64,
    /// The shelf's title when it was announced; a renamed shelf moves it.
    pub shelf: String,
    /// The collection's key and the member's own key on that server: Plex
    /// ratingKeys (a season's, for a season) or Jellyfin/Emby item ids.
    pub collection: String,
    pub rating_key: String,
    /// The server holding the shelf; files written before Jellyfin shelves read as Plex.
    #[serde(default)]
    pub server: super::ShelfServer,
}

/// How to undo a delete.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "app", rename_all = "lowercase")]
pub enum RestoreTarget {
    Radarr {
        radarr_id: u32,
        tmdb_id: Option<u32>,
        mode: DeleteMode,
        /// Needed to add a removed entry back.
        quality_profile_id: Option<u64>,
        root_folder_path: Option<String>,
    },
    Sonarr {
        series_id: u32,
        season: u32,
    },
}

/// A delete FLINCH made and verified.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeletedItem {
    pub id: String,
    pub title: String,
    pub kind: LibraryKind,
    pub bytes: u64,
    pub deleted_at: u64,
    /// True when it went through the Leaving Soon window.
    pub announced: bool,
    pub target: RestoreTarget,
    /// What the Seerr cleanup did, in words; `None` when not attempted.
    #[serde(default)]
    pub seerr: Option<String>,
    #[serde(default)]
    pub restored_at: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NativeState {
    /// By card id.
    pub leaving: BTreeMap<String, Leaving>,
    /// Newest last; pruned to [`RESTORE_WINDOW_SECS`].
    pub deleted: Vec<DeletedItem>,
    /// Collections FLINCH created (ratingKey → section id). Only these are
    /// ever deleted when they empty: a same-titled collection someone else
    /// made is never FLINCH's to remove.
    pub created: BTreeMap<String, u32>,
}

impl NativeState {
    pub fn path(state_dir: &Path) -> PathBuf {
        state_dir.join("native.json")
    }

    /// A missing file is a first run; an unreadable one starts over, loudly:
    /// the shelf itself still lists what waits, and is re-read every cycle.
    pub fn read(state_dir: &Path) -> Self {
        match std::fs::read_to_string(Self::path(state_dir)) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|error| {
                eprintln!("[flinch-arrd] native.json unreadable, starting over: {error}");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn write(&self, state_dir: &Path) -> std::io::Result<()> {
        crate::persist::replace(&Self::path(state_dir), &serde_json::to_vec_pretty(self)?)
    }

    /// Drop deletes past the undo window.
    pub fn prune(&mut self, now: u64) {
        self.deleted.retain(|item| now.saturating_sub(item.deleted_at) < RESTORE_WINDOW_SECS);
    }

    /// The delete an undo may act on: within the window, not restored yet.
    pub fn restorable(&self, id: &str, now: u64) -> Option<&DeletedItem> {
        self.deleted
            .iter()
            .rev()
            .find(|item| item.id == id && item.restored_at.is_none() && now.saturating_sub(item.deleted_at) < RESTORE_WINDOW_SECS)
    }
}

/// The undo queue: one empty file per requested card id.
pub fn restore_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("restore")
}

/// A card id is a movie or season of an instance ([`crate::ids::ArrRef`]):
/// only those become file names, so a request can never name a path outside
/// the queue (an instance name is `[a-z0-9_]`).
pub fn is_card_id(id: &str) -> bool {
    crate::ids::ArrRef::card(id).is_some()
}

/// The ids waiting in the undo queue.
pub fn pending_restores(state_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(restore_dir(state_dir)) else { return Vec::new() };
    let mut ids: Vec<String> =
        entries.filter_map(|entry| entry.ok()?.file_name().into_string().ok()).filter(|name| is_card_id(name)).collect();
    ids.sort();
    ids
}

/// One Leaving Soon item as the UI lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeavingView {
    pub id: String,
    pub title: String,
    pub bytes: u64,
    #[serde(default)]
    pub announced_at: u64,
    pub until: u64,
}

/// One recent delete as the UI lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeletedView {
    pub id: String,
    pub title: String,
    pub kind: LibraryKind,
    pub bytes: u64,
    pub deleted_at: u64,
    pub announced: bool,
    #[serde(default)]
    pub seerr: Option<String>,
    #[serde(default)]
    pub restored_at: Option<u64>,
    /// An undo is queued and not done yet.
    #[serde(default)]
    pub restore_pending: bool,
}

/// `status.native`: what the native executor did this run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NativeStatus {
    pub dry_run: bool,
    /// Whether items nobody finished can be announced (Plex configured and a
    /// Leaving Soon title set).
    pub leaving_route: bool,
    pub window_days: u32,
    pub announced: usize,
    pub announced_bytes: u64,
    pub deleted: usize,
    pub deleted_bytes: u64,
    pub withdrawn: usize,
    pub deferred: usize,
    pub failures: usize,
    /// Writes printed in a dry run.
    pub simulated: usize,
    /// Why items wait, in words, one line per item.
    pub held: Vec<String>,
    pub problems: Vec<String>,
    pub leaving: Vec<LeavingView>,
    /// Native deletes of the last 30 days, newest first.
    pub recent: Vec<DeletedView>,
}

impl NativeStatus {
    /// The shelf and the undo list, from the state as it stands.
    pub fn lists(&mut self, state: &NativeState, pending: &[String]) {
        self.leaving = state
            .leaving
            .iter()
            .map(|(id, item)| LeavingView {
                id: id.clone(),
                title: item.title.clone(),
                bytes: item.bytes,
                announced_at: item.announced_at,
                until: item.until,
            })
            .collect();
        self.leaving.sort_by_key(|item| item.until);
        self.recent = state
            .deleted
            .iter()
            .rev()
            .map(|item| DeletedView {
                id: item.id.clone(),
                title: item.title.clone(),
                kind: item.kind,
                bytes: item.bytes,
                deleted_at: item.deleted_at,
                announced: item.announced,
                seerr: item.seerr.clone(),
                restored_at: item.restored_at,
                restore_pending: item.restored_at.is_none() && pending.contains(&item.id),
            })
            .collect();
    }
}
