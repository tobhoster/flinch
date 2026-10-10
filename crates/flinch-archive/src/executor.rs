//! FLINCH's own executor: deleting without Maintainerr.
//!
//! The planner decides; something has to act. By default that stays
//! Maintainerr (see [`crate::maintainerr`]), so an existing install keeps
//! working unchanged. With `executor: "native"` FLINCH acts itself, with the
//! same promises the Maintainerr route keeps:
//!
//! - A finished or duplicate item past its grace runs is deleted at once.
//! - An item nobody finished is announced first on the Leaving Soon shelf
//!   ([`shelf`]: a Plex collection per section, [`crate::plex::collections`],
//!   or one Jellyfin/Emby collection, [`crate::jellyfin::collections`], as
//!   `native.leaving_soon_server` says) and deleted only once its window has
//!   run out while it is still selected and still unplayed. A play during the
//!   window, on any watch source, takes it back. Without the shelf's server
//!   there is nowhere to warn, so such items are held, exactly like a blank
//!   Leaving Soon title.
//! - Watch evidence is read again right before every delete ([`recheck`]);
//!   anything short of a clean read keeps the item.
//! - Every write is read back ([`radarr`], [`sonarr`], [`seerr`]); a dry run
//!   prints each write and sends none, and records nothing as done.
//!
//! [`lifecycle`] is the pure planner (tested directly); the daemon's
//! `native` module does the I/O around it.

use serde::{Deserialize, Serialize};

mod http;
pub mod lifecycle;
pub mod radarr;
pub mod recheck;
pub mod seerr;
pub mod shelf;
pub mod sonarr;
pub mod state;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_http;

pub use http::ExecutorError;
pub use shelf::{LeavingSoonShelf, Shelf, ShelfError, ShelfGroup, ShelfServer, Shelved};
pub use state::{DeletedItem, Leaving, NativeState, NativeStatus, RestoreTarget};

/// What a delete did.
#[derive(Debug, Clone, PartialEq)]
pub enum Evicted {
    /// Sent and read back; the target undoes it.
    Done(RestoreTarget),
    /// A dry run: every write printed, none sent.
    Simulated,
}

/// Who acts on the plan's evictions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Executor {
    /// Hand evictions to Maintainerr collections; Maintainerr deletes.
    #[default]
    Maintainerr,
    /// FLINCH announces in Plex and deletes through Radarr and Sonarr itself.
    Native,
}

/// What deleting a movie means in Radarr. Seasons always keep the series:
/// their episode files go and the season is unmonitored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteMode {
    /// Delete the movie's file and unmonitor it: the entry stays, so the
    /// history stays and an undo is one search away.
    #[default]
    FileAndUnmonitor,
    /// Remove the movie from Radarr with its files.
    RemoveEntry,
}

/// The native executor's settings (`settings.native`). Inert while
/// `executor` is Maintainerr.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NativeConfig {
    /// How long an item nobody finished stays on the Leaving Soon shelf before
    /// it may be deleted.
    pub leaving_soon_days: u32,
    pub delete_mode: DeleteMode,
    /// With `remove_entry`, also add Radarr's import exclusion so a list does
    /// not add the movie straight back. Ignored for `file_and_unmonitor`: the
    /// entry stays, so no list re-adds it.
    pub add_import_exclusion: bool,
    /// Clear the title's Seerr request after a delete so it can be requested
    /// again (needs Seerr's URL and key).
    pub seerr_cleanup: bool,
    /// The most deletes in one run, expired Leaving Soon windows included.
    pub max_deletes_per_run: usize,
    /// Draw a "Leaves Oct 23" badge on the poster of each item on the Leaving
    /// Soon shelf (and restore the original before it leaves the shelf).
    /// Off by default: it rewrites posters, which Kometa overlays also do.
    pub poster_overlays: bool,
    /// Where the Leaving Soon shelf lives: a Plex collection per section, or
    /// one Jellyfin/Emby collection (the server in `settings.jellyfin`).
    pub leaving_soon_server: ShelfServer,
}

impl Default for NativeConfig {
    fn default() -> Self {
        Self {
            leaving_soon_days: 14,
            delete_mode: DeleteMode::default(),
            add_import_exclusion: false,
            seerr_cleanup: true,
            max_deletes_per_run: 10,
            poster_overlays: false,
            leaving_soon_server: ShelfServer::Plex,
        }
    }
}

/// A value the Settings page would refuse, in plain words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidNative(pub &'static str);

impl NativeConfig {
    pub fn validate(&self) -> Result<(), InvalidNative> {
        if !(1..=90).contains(&self.leaving_soon_days) {
            return Err(InvalidNative("the Leaving Soon window (native.leaving_soon_days) must be between 1 and 90 days"));
        }
        if !(1..=500).contains(&self.max_deletes_per_run) {
            return Err(InvalidNative("deletes per run (native.max_deletes_per_run) must be between 1 and 500"));
        }
        Ok(())
    }

    pub fn window_secs(&self) -> u64 {
        u64::from(self.leaving_soon_days) * 86_400
    }
}

/// Who acts this cycle, and so whether grace streaks may advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acting {
    Maintainerr,
    Native,
    /// Nobody can act: a cycle that cannot act is not an appearance, so an
    /// outage never runs down an item's grace window.
    Nobody,
}

/// Maintainerr acts only while it is readable: unread, the plan was made
/// without the operator's exclusions. The native executor needs no
/// Maintainerr at all (its keeps, when it has one, stand in from the last
/// read), so its cycles always count.
pub fn acting(executor: Executor, maintainerr_readable: bool) -> Acting {
    match (executor, maintainerr_readable) {
        (Executor::Native, _) => Acting::Native,
        (Executor::Maintainerr, true) => Acting::Maintainerr,
        (Executor::Maintainerr, false) => Acting::Nobody,
    }
}
