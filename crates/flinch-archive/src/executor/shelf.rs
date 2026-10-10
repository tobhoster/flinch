//! The Leaving Soon shelf, whichever server shows it. The native executor
//! needs four things from a shelf: put items on it (creating it when missing),
//! list it, take an item off, and know where an item belongs. Plex keeps one
//! collection per library section and can promote it to home; Jellyfin/Emby
//! keep one collection (BoxSet) for the whole server. Both read every write
//! back and print instead of writing in a dry run.

use crate::card::LibraryKind;
use crate::jellyfin::{JellyfinCollections, JellyfinError};
use crate::plex::collections::{CollectionError, PlexCollections};
use serde::{Deserialize, Serialize};

/// Which server shows the shelf (`native.leaving_soon_server`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShelfServer {
    #[default]
    Plex,
    /// Jellyfin or Emby, as `settings.jellyfin.kind` says.
    Jellyfin,
}

impl ShelfServer {
    pub fn label(self) -> &'static str {
        match self {
            Self::Plex => "Plex",
            Self::Jellyfin => "Jellyfin/Emby",
        }
    }
}

/// Where a batch of items goes: Plex needs the section; the kind decides
/// what a new Plex collection holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ShelfGroup {
    pub section: Option<u32>,
    pub season: bool,
}

impl ShelfGroup {
    pub fn kind(self) -> LibraryKind {
        if self.season {
            LibraryKind::Season
        } else {
            LibraryKind::Movie
        }
    }
}

/// A shelf the server holds, after a [`LeavingSoonShelf::shelve`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shelved {
    pub collection: String,
    /// This call created the collection.
    pub created: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ShelfError {
    #[error(transparent)]
    Plex(#[from] CollectionError),
    #[error(transparent)]
    Jellyfin(#[from] JellyfinError),
    #[error("a Plex shelf needs the item's library section")]
    NoSection,
}

/// What the native executor asks of a shelf. Used through [`Shelf`], so the
/// futures stay concrete (and `Send` where the server's client is).
#[allow(async_fn_in_trait)]
pub trait LeavingSoonShelf {
    /// Adds `members` to the group's shelf titled `title`, creating it when
    /// missing; each member is read back. `Ok(None)` when it does not exist
    /// and could not be made (a dry run never creates one).
    async fn shelve(&self, group: ShelfGroup, title: &str, members: &[String]) -> Result<Option<Shelved>, ShelfError>;
    async fn members(&self, collection: &str) -> Result<Vec<String>, ShelfError>;
    async fn remove(&self, collection: &str, member: &str) -> Result<(), ShelfError>;

    /// Takes a member off if it is still on: an item already gone must not
    /// fail every run.
    async fn take_off(&self, collection: &str, member: &str) -> Result<(), ShelfError> {
        if self.members(collection).await?.iter().any(|key| key == member) {
            self.remove(collection, member).await?;
        }
        Ok(())
    }
}

impl LeavingSoonShelf for PlexCollections<'_> {
    async fn shelve(&self, group: ShelfGroup, title: &str, members: &[String]) -> Result<Option<Shelved>, ShelfError> {
        let section = group.section.ok_or(ShelfError::NoSection)?;
        if let Some(collection) = self.find(section, title).await? {
            self.add(&collection, members).await?;
            return Ok(Some(Shelved { collection, created: false }));
        }
        let Some(collection) = self.ensure(section, title, group.kind(), members).await? else { return Ok(None) };
        if let Err(error) = self.promote_home(&collection, section).await {
            eprintln!("[flinch-arrd] native: Leaving Soon created in section {section} but not shown on home: {error}");
        }
        Ok(Some(Shelved { collection, created: true }))
    }

    async fn members(&self, collection: &str) -> Result<Vec<String>, ShelfError> {
        Ok(PlexCollections::members(self, collection).await?)
    }

    async fn remove(&self, collection: &str, member: &str) -> Result<(), ShelfError> {
        Ok(PlexCollections::remove(self, collection, member).await?)
    }
}

impl LeavingSoonShelf for JellyfinCollections<'_> {
    async fn shelve(&self, _group: ShelfGroup, title: &str, members: &[String]) -> Result<Option<Shelved>, ShelfError> {
        Ok(self.ensure(title, members).await?.map(|(collection, created)| Shelved { collection, created }))
    }

    async fn members(&self, collection: &str) -> Result<Vec<String>, ShelfError> {
        Ok(JellyfinCollections::members(self, collection).await?)
    }

    async fn remove(&self, collection: &str, member: &str) -> Result<(), ShelfError> {
        Ok(JellyfinCollections::remove(self, collection, member).await?)
    }
}

/// The configured shelf, borrowed from the clients the executor holds.
pub enum Shelf<'s, 'a> {
    Plex(&'s PlexCollections<'a>),
    Jellyfin(&'s JellyfinCollections<'a>),
}

impl LeavingSoonShelf for Shelf<'_, '_> {
    async fn shelve(&self, group: ShelfGroup, title: &str, members: &[String]) -> Result<Option<Shelved>, ShelfError> {
        match self {
            Self::Plex(shelf) => shelf.shelve(group, title, members).await,
            Self::Jellyfin(shelf) => shelf.shelve(group, title, members).await,
        }
    }

    async fn members(&self, collection: &str) -> Result<Vec<String>, ShelfError> {
        match self {
            Self::Plex(shelf) => LeavingSoonShelf::members(*shelf, collection).await,
            Self::Jellyfin(shelf) => LeavingSoonShelf::members(*shelf, collection).await,
        }
    }

    async fn remove(&self, collection: &str, member: &str) -> Result<(), ShelfError> {
        match self {
            Self::Plex(shelf) => LeavingSoonShelf::remove(*shelf, collection, member).await,
            Self::Jellyfin(shelf) => LeavingSoonShelf::remove(*shelf, collection, member).await,
        }
    }
}
