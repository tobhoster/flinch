//! The last look before a delete, from Jellyfin/Emby: every user's state on
//! the item, read again the moment before the *arr is told to delete it. The
//! server keeps no history, only each user's latest `UserData`, so the item's
//! newest `LastPlayedDate` across users is its last play, and any user's
//! `PlaybackPositionTicks` means someone is partway. The answer feeds the same
//! verdict as Plex's ([`crate::executor::recheck::judge`]).
//!
//! A movie is read with `Ids={item}`, a season as its episodes
//! (`ParentId={season}&IncludeItemTypes=Episode&Recursive=true`), through each
//! user's listing (see [`super::JellyfinClient`] for the endpoints). A failed
//! user fails the look, so the item is kept.

use super::{JellyfinClient, JellyfinError, JellyfinItem};
use crate::card::LibraryKind;
use crate::executor::recheck::PlexWatch;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Listing {
    #[serde(default)]
    items: Vec<JellyfinItem>,
}

/// `None` when no user lists the item (or any of the season's episodes):
/// nothing proves it unplayed. A tick is 100 ns.
pub async fn last_look(client: &JellyfinClient, item_id: &str, kind: LibraryKind) -> Result<Option<PlexWatch>, JellyfinError> {
    const ENDPOINT: &str = "last look";
    let users = client.users().await?;
    if users.is_empty() {
        return Err(JellyfinError::Unexpected { endpoint: ENDPOINT, detail: "no users listed".into() });
    }
    let mut listed = false;
    let mut watch = PlexWatch::default();
    for user in &users {
        let request = client.user_listing(&user.id).query(&[("EnableUserData", "true"), ("EnableImages", "false")]);
        let request = match kind {
            LibraryKind::Movie => request.query(&[("Ids", item_id)]),
            LibraryKind::Season => request.query(&[("ParentId", item_id), ("IncludeItemTypes", "Episode"), ("Recursive", "true")]),
        };
        let listing: Listing = JellyfinClient::json(ENDPOINT, request).await?;
        for item in listing.items.iter().filter(|item| kind == LibraryKind::Season || item.id == item_id) {
            listed = true;
            let Some(data) = &item.user_data else { continue };
            if !data.played && data.playback_position_ticks > 0 {
                watch.view_offset_ms = watch.view_offset_ms.max(data.playback_position_ticks / 10_000).max(1);
            }
            let played = data.last_played_date.as_deref().and_then(super::parse_utc);
            watch.last_viewed_at = watch.last_viewed_at.max(played);
        }
    }
    Ok(listed.then_some(watch))
}
