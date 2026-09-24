//! Maintainerr sync: FLINCH decides, Maintainerr executes.
//!
//! Each item is either kept or evicted. A keep becomes a global Maintainerr
//! exclusion, so the operator's own rules cannot delete it. An eviction becomes
//! a member of its kind's deletion collection, least expected regret first.
//! Maintainerr's schedule then performs the delete.
//!
//! The sync has three layers, and each can be tested on its own:
//! - [`client`] is the typed HTTP port ([`MaintainerrApi`]). [`wire`] holds the
//!   exact request bodies and the response semantics.
//! - [`plan`] is the pure planner. It turns desired, observed and owned state
//!   into ordered [`SyncAction`]s.
//! - [`execute`] observes Maintainerr, runs the actions and verifies each one by
//!   reading it back. Ownership is recorded only after the read-back agrees.
//!
//! Identity: Maintainerr acts only on Plex ratingKeys. A card id such as
//! `radarr-7` or `sonarr-12-s3` is never sent and never parsed. Every target
//! comes from GUID-resolved [`PlexIds`]; a card without them is left alone.
//!
//! Ownership: FLINCH changes only rows it created and verified, as recorded in
//! [`OwnedState`]. Anything else, such as an operator's exclusion or a member a
//! rule added, is read and respected but never removed.

use crate::card::LibraryKind;
use crate::ids::PlexIds;
use serde::{Deserialize, Serialize};
use std::future::Future;
use thiserror::Error;

mod client;
mod execute;
mod plan;
mod state;
mod validate;
mod wire;

#[cfg(test)]
mod tests;

pub use client::HttpMaintainerr;
pub use execute::{execute, observe, Outcome, SyncReport, SyncSummary};
pub use plan::{
    operator_keeps, plan_sync, Blocked, Caps, Desired, Observed, SyncAction, SyncItem, SyncPlan,
};
pub use state::{read_operator_keeps, write_operator_keeps, OwnedState, ProtectedEntry, ScheduledEntry};
pub use validate::{destinations, CollectionProblem, CollectionTitles, Destination, Handover, Misconfigured, Route};
pub use wire::{CollectionInfo, ExclusionRow, MaintainerrVersion};

/// What Maintainerr acts on. It is built only from resolved [`PlexIds`], and
/// persisted as `{rating_key, season_rating_key?}` like [`PlexIds`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(into = "state::Keys", try_from = "state::Keys")]
pub enum MaintainerrTarget {
    Movie { rating_key: String },
    Season { show_rating_key: String, season_rating_key: String },
}

impl MaintainerrTarget {
    /// Returns `None` when the ids cannot name the item: a blank key, or a
    /// season without its own ratingKey.
    pub fn from_plex(ids: &PlexIds, kind: LibraryKind) -> Option<Self> {
        let rating_key = non_blank(&ids.rating_key)?;
        match kind {
            LibraryKind::Movie => Some(Self::Movie { rating_key }),
            LibraryKind::Season => Some(Self::Season {
                show_rating_key: rating_key,
                season_rating_key: non_blank(ids.season_rating_key.as_deref()?)?,
            }),
        }
    }

    pub fn kind(&self) -> LibraryKind {
        match self {
            Self::Movie { .. } => LibraryKind::Movie,
            Self::Season { .. } => LibraryKind::Season,
        }
    }

    /// The `mediaId` Maintainerr expects: the movie, or the season's show.
    /// Exclusion rows are also read by this key: `?mediaServerId=` returns the
    /// rows for the item itself and every row whose parent is this id.
    pub fn media_id(&self) -> &str {
        match self {
            Self::Movie { rating_key } => rating_key,
            Self::Season { show_rating_key, .. } => show_rating_key,
        }
    }

    /// The item's own ratingKey. A collection member carries it, and so does
    /// the item's own exclusion row.
    pub fn item_key(&self) -> &str {
        match self {
            Self::Movie { rating_key } => rating_key,
            Self::Season { season_rating_key, .. } => season_rating_key,
        }
    }

    /// Whether an exclusion row covers this item: the item's own row, or (for
    /// a season) a row that excludes the whole show.
    pub fn is_covered_by(&self, row: &ExclusionRow) -> bool {
        row.media_server_id == self.item_key() || row.media_server_id == self.media_id()
    }
}

fn non_blank(key: &str) -> Option<String> {
    let key = key.trim();
    (!key.is_empty()).then(|| key.to_string())
}

#[derive(Debug, Error)]
pub enum MaintainerrError {
    #[error("{endpoint}: HTTP {status}: {message}")]
    Http { endpoint: &'static str, status: u16, message: String },
    /// A 2xx whose body says the call failed, e.g. `POST /api/rules/exclusion`
    /// answering 201 `{"code":0,"result":"Failed - no metadata"}`.
    #[error("{endpoint}: answered 2xx with code {code}: {message}")]
    Refused { endpoint: &'static str, code: i64, message: String },
    /// An unreadable body. It is never treated as an empty list.
    #[error("{endpoint}: unreadable response: {source}")]
    Parse {
        endpoint: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("{endpoint}: transport failure: {source}")]
    Transport {
        endpoint: &'static str,
        #[source]
        source: reqwest::Error,
    },
}

/// Everything FLINCH reads from or writes to Maintainerr, and nothing more.
/// There is deliberately no bulk delete, no `action: 1` removal and no
/// `DELETE /api/rules/exclusions/{mediaServerId}`: each of those also deletes
/// rows the operator made.
///
/// The futures are `Send`, so any executor can drive the port. The live
/// client, the dry-run sink and the test double all satisfy that.
pub trait MaintainerrApi {
    /// True when writes are only printed (dry-run). The executor then neither
    /// verifies nor records anything as owned.
    fn simulated(&self) -> bool {
        false
    }

    /// `GET /api/app/status`.
    fn version(&mut self) -> impl Future<Output = Result<MaintainerrVersion, MaintainerrError>> + Send;

    /// `GET /api/collections`.
    fn collections(&mut self) -> impl Future<Output = Result<Vec<CollectionInfo>, MaintainerrError>> + Send;

    /// `GET /api/collections/media/?collectionId=`: the members' item keys.
    fn collection_members(
        &mut self,
        collection_id: i64,
    ) -> impl Future<Output = Result<Vec<String>, MaintainerrError>> + Send;

    /// `GET /api/rules/exclusion?mediaServerId=`.
    fn exclusions(
        &mut self,
        media_id: &str,
    ) -> impl Future<Output = Result<Vec<ExclusionRow>, MaintainerrError>> + Send;

    /// `POST /api/rules/exclusion` (a global exclusion; a season carries its context).
    fn add_exclusion(
        &mut self,
        target: &MaintainerrTarget,
    ) -> impl Future<Output = Result<(), MaintainerrError>> + Send;

    /// `DELETE /api/rules/exclusion/{id}`: exactly one row.
    fn remove_exclusion(&mut self, exclusion_id: i64) -> impl Future<Output = Result<(), MaintainerrError>> + Send;

    /// `POST /api/collections/media/add` with `action: 0`.
    fn add_to_collection(
        &mut self,
        collection_id: i64,
        target: &MaintainerrTarget,
    ) -> impl Future<Output = Result<(), MaintainerrError>> + Send;

    /// `DELETE /api/collections/media?mediaId=&collectionId=`. The collection id
    /// is always sent: without it Maintainerr removes the item from every
    /// collection.
    fn remove_from_collection(
        &mut self,
        collection_id: i64,
        item_key: &str,
    ) -> impl Future<Output = Result<(), MaintainerrError>> + Send;
}
