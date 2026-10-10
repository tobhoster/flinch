//! Viewers whose plays are not household evidence (`settings.json`
//! `ignore_viewers`): a guest account, a kid's profile that plays everything
//! for a minute, the operator testing a file. Their plays count as no play.
//!
//! Only the *plays* go; the record's health stays as it was read. A source
//! that was read in full with one viewer's rows set aside is still a full
//! read, so an item only an ignored viewer played reads as never played, and
//! a source read only partly still proves nothing about absence. Rows are set
//! aside right after each source is read, before anything derives evidence or
//! persists plays, so the daemon, the fitter and the item view agree.
//!
//! Each source names its viewers its own way, and a name is matched there
//! only (case-insensitively): a Plex account's name, a Tautulli user (its
//! name as history reports it), a Jellyfin or Emby user's name, a Tracearr
//! username, a Trakt source's name. A viewer whose name a source did not give
//! stays counted. Plex's own item state belongs to the account whose token
//! FLINCH borrows and cannot be split by viewer: it stays too, the safe
//! direction.

use crate::fit::plays::Viewer;
use crate::jellyfin::JellyfinRead;
use crate::plex::PlexMetadata;
use crate::tautulli::TautulliRow;
use crate::watch_sources::SourceRead;
use std::collections::HashMap;

/// Up to this many names; it is a guest list, not a user directory.
pub const MAX_VIEWERS: usize = 50;

/// An `ignore_viewers` list outside its bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidViewers(pub &'static str);

pub fn validate(names: &[String]) -> Result<(), InvalidViewers> {
    if names.len() > MAX_VIEWERS {
        return Err(InvalidViewers("ignore_viewers takes up to 50 names"));
    }
    if names.iter().any(|name| name.trim().is_empty() || name.chars().count() > 100) {
        return Err(InvalidViewers("every ignored viewer needs a name of 1 to 100 characters"));
    }
    Ok(())
}

/// The ignore list, ready to match.
#[derive(Debug, Clone, Default)]
pub struct IgnoredViewers {
    names: Vec<String>,
}

impl IgnoredViewers {
    pub fn new(names: &[String]) -> Self {
        Self { names: names.iter().map(|name| name.trim().to_lowercase()).filter(|name| !name.is_empty()).collect() }
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub fn is_ignored(&self, name: &str) -> bool {
        let name = name.trim().to_lowercase();
        !name.is_empty() && self.names.contains(&name)
    }

    /// Drop Plex history rows by an ignored account. `accounts` is the
    /// server's account id → name; a row whose account has no known name
    /// stays. Returns how many went.
    pub fn plex(&self, rows: &mut Vec<PlexMetadata>, accounts: &HashMap<u64, String>) -> usize {
        let before = rows.len();
        rows.retain(|row| !row.account_id.and_then(|id| accounts.get(&id)).is_some_and(|name| self.is_ignored(name)));
        before - rows.len()
    }

    /// Drop Tautulli streams by an ignored user.
    pub fn tautulli(&self, rows: &mut Vec<TautulliRow>) -> usize {
        let before = rows.len();
        rows.retain(|row| !self.is_ignored(&row.user));
        before - rows.len()
    }

    /// Drop an ignored Jellyfin or Emby user's whole state. The read's
    /// `complete` stays: every user was still read.
    pub fn jellyfin(&self, read: &mut JellyfinRead) -> usize {
        let before = read.users.len();
        read.users.retain(|user| !self.is_ignored(&user.name));
        before - read.users.len()
    }

    /// Drop a Tracearr user's plays (by username) or a Trakt account's (by
    /// the source's name). The record's coverage (`epochs`) stays as read.
    pub fn source(&self, read: &mut SourceRead) -> usize {
        let before = read.plays.len();
        let usernames = &read.usernames;
        read.plays.retain(|play| match &play.viewer {
            Viewer::TracearrUser(id) => !usernames.get(id).is_some_and(|name| self.is_ignored(name)),
            Viewer::TraktUser(name) => !self.is_ignored(name),
            _ => true,
        });
        before - read.plays.len()
    }
}

#[cfg(test)]
mod tests;
