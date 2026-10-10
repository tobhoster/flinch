//! Keeping the Plex Leaving Soon shelf readable after each cycle's moves:
//! members sorted soonest-leaving first, the window's dates in the summary,
//! opt-in poster badges (restored before an item leaves the shelf), and the
//! removal of collections FLINCH created once they are empty. Plex-only: the
//! calls are Plex endpoints. A dry run prints each write and records nothing.

use super::state_dir;
use flinch_archive::daemon::RuntimeSettings;
use flinch_archive::executor::lifecycle::Action;
use flinch_archive::executor::{NativeState, NativeStatus, ShelfServer};
use flinch_archive::ids::PlexIds;
use flinch_archive::overlay::{self, OverlayState};
use flinch_archive::plex::collections::{CollectionError, PlexCollections};
use flinch_archive::plex::shelf::{dates_line, shelf_order};
use std::collections::{BTreeMap, HashMap};

pub(super) struct PlexShelf<'a> {
    pub(super) plex: PlexCollections<'a>,
    pub(super) settings: &'a RuntimeSettings,
    pub(super) plex_ids: &'a HashMap<String, PlexIds>,
    pub(super) now: u64,
    pub(super) dry_run: bool,
}

impl PlexShelf<'_> {
    /// Puts back the original posters of the shelf members this cycle takes
    /// back (kept, played, gone) or deletes, before their membership changes.
    /// A failure is reported and retried by [`PlexShelf::tidy`]; it never
    /// holds up the item itself.
    pub(super) async fn before_leaving(&self, actions: &[Action], state: &NativeState, status: &mut NativeStatus) {
        let keys: Vec<&str> = actions
            .iter()
            .filter_map(|action| match action {
                Action::Withdraw { id, .. } | Action::Delete { id, .. } => state.leaving.get(id),
                Action::Announce { .. } => None,
            })
            .filter(|entry| entry.server == ShelfServer::Plex)
            .map(|entry| entry.rating_key.as_str())
            .collect();
        let mut overlays = match OverlayState::read(&state_dir()) {
            Ok(overlays) => overlays,
            Err(error) => return status.problems.push(format!("poster badges: {error}")),
        };
        let badged: Vec<&str> = keys.into_iter().filter(|key| overlays.posters.contains_key(*key)).collect();
        if badged.is_empty() {
            return;
        }
        for key in badged {
            if let Err(error) = overlay::restore(&self.plex, &mut overlays, key).await {
                status.problems.push(format!("plex item {key}: restoring its poster failed, retried next run: {error}"));
            }
        }
        self.save(&overlays, status);
    }

    fn save(&self, overlays: &OverlayState, status: &mut NativeStatus) {
        if !self.dry_run {
            if let Err(error) = overlays.write(&state_dir()) {
                status.problems.push(format!("poster badges: {error}"));
            }
        }
    }

    pub(super) async fn tidy(&self, state: &mut NativeState, status: &mut NativeStatus) {
        let mut shelves: BTreeMap<&str, Vec<(String, u64, String)>> = BTreeMap::new();
        let mut sections: HashMap<&str, u32> = state.created.iter().map(|(key, section)| (key.as_str(), *section)).collect();
        for (id, entry) in state.leaving.iter().filter(|(_, entry)| entry.server == ShelfServer::Plex) {
            shelves.entry(entry.collection.as_str()).or_default().push((entry.rating_key.clone(), entry.until, entry.title.clone()));
            if let Some(section) = self.plex_ids.get(id).and_then(|ids| ids.section_id) {
                sections.entry(entry.collection.as_str()).or_insert(section);
            }
        }
        // With household keep links on, their summary (which leads with the
        // same dates line) is the one written.
        let summary_ours = !flinch_archive::requests::links_on(&self.settings.household, &self.settings.notify.ui_url);
        for (collection, members) in &shelves {
            if let Err(error) = self.plex.order(collection, &shelf_order(members)).await {
                status.problems.push(format!("Leaving Soon {collection}: ordering by leave date failed: {error}"));
            }
            let untils: Vec<u64> = members.iter().map(|(_, until, _)| *until).collect();
            if let (true, Some(line), Some(section)) = (summary_ours, dates_line(&untils), sections.get(collection)) {
                if let Err(error) = self.plex.edit_summary(*section, collection, &line).await {
                    status.problems.push(format!("Leaving Soon {collection}: writing its dates failed: {error}"));
                }
            }
        }
        let on_shelf: Vec<(String, u64)> = shelves.values().flatten().map(|(key, until, _)| (key.clone(), *until)).collect();
        self.badges(&on_shelf, status).await;
        let empty: Vec<String> = state.created.keys().filter(|key| !shelves.contains_key(key.as_str())).cloned().collect();
        for collection in empty {
            match self.plex.delete_if_empty(&collection).await {
                Ok(false) => {}
                Ok(true) if self.dry_run => {}
                Ok(true) | Err(CollectionError::Http { status: 404, .. }) => {
                    println!("[flinch-arrd] native: empty Leaving Soon collection {collection} removed");
                    state.created.remove(&collection);
                }
                Err(error) => status.problems.push(format!("Leaving Soon {collection}: removing the empty collection failed: {error}")),
            }
        }
    }

    /// Badges every shelf member (when enabled) and restores every poster no
    /// longer on the shelf.
    async fn badges(&self, on_shelf: &[(String, u64)], status: &mut NativeStatus) {
        let mut overlays = match OverlayState::read(&state_dir()) {
            Ok(overlays) => overlays,
            Err(error) => return status.problems.push(format!("poster badges: {error}")),
        };
        let planned = overlay::plan(&overlays, on_shelf, self.settings.native.poster_overlays);
        for key in &planned.restore {
            if let Err(error) = overlay::restore(&self.plex, &mut overlays, key).await {
                status.problems.push(format!("plex item {key}: restoring its poster failed, retried next run: {error}"));
            }
        }
        for (key, text) in &planned.apply {
            if let Err(error) = overlay::apply(&self.plex, &mut overlays, key, text, self.now).await {
                status.problems.push(format!("plex item {key}: drawing its Leaving Soon badge failed: {error}"));
            }
        }
        if !planned.apply.is_empty() || !planned.restore.is_empty() {
            self.save(&overlays, status);
        }
    }
}
