//! Opt-in deletion of quality profiles nobody needs: not managed by the sync
//! and holding no movie or series (`delete_unused_profiles`).
//!
//! Why: a guide migration or an abandoned Recyclarr config leaves profiles
//! behind that the next import list or operator may pick by mistake. A
//! profile in use is never offered, and none is while the library's use is
//! unknown (an unread profile id on any item, or no library read at all);
//! nor is the quality actions' fallback profile, which FLINCH moves items into
//! by name. Radarr and Sonarr refuse to delete a profile an item or import
//! list uses, a second guard at apply time.

use super::client::Live;
use super::diff::{change_id, Action, Change, FieldChange, Kind, Owned};
use super::impact::Usage;
use crate::capacity::App;

pub struct Unused<'a> {
    pub app: App,
    pub live: &'a Live,
    /// Profiles the sync manages: never offered.
    pub managed: &'a [u32],
    pub owned: &'a Owned,
    pub usage: Option<&'a Usage>,
    /// A profile FLINCH needs by name.
    pub keep: Option<&'a str>,
}

/// One delete change per unused, unmanaged profile.
pub fn unused_profiles(cx: &Unused<'_>) -> Vec<Change> {
    let Some(usage) = cx.usage.filter(|usage| usage.complete) else { return Vec::new() };
    cx.live
        .profiles
        .iter()
        .map(|profile| &profile.typed)
        .filter(|profile| !cx.managed.contains(&profile.id) && usage.items(profile.id) == 0)
        .filter(|profile| !cx.keep.is_some_and(|keep| keep.eq_ignore_ascii_case(&profile.name)))
        .map(|profile| {
            let trash_id = cx.owned.profiles.iter().find(|(_, id)| **id == profile.id).map(|(trash_id, _)| trash_id.clone());
            let why = if trash_id.is_some() {
                "created by FLINCH, no longer synced, and no item uses it"
            } else {
                "not synced and no item uses it; offered because delete_unused_profiles is on"
            };
            Change {
                id: change_id(cx.app, "profile-delete", &profile.id.to_string()),
                app: cx.app,
                kind: Kind::QualityProfile,
                action: Action::Delete,
                name: profile.name.clone(),
                trash_id,
                arr_id: Some(profile.id),
                fields: vec![FieldChange { field: "reason".to_string(), from: None, to: Some(why.to_string()) }],
                requires: Vec::new(),
            }
        })
        .collect()
}
