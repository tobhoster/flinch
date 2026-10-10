//! Acting on the lifecycle's plan: announce on the Leaving Soon shelf (Plex or
//! Jellyfin/Emby, [`LeavingSoonShelf`]), take back, delete. Each delete takes
//! a last look at the media servers first ([`super::look`]), is read back in
//! the *arr, then booked in the eviction ledger (so the freed bytes are
//! credited as on the Maintainerr route), followed by the Seerr cleanup and
//! the torrent removal when the settings allow.

use super::Cycle;
use flinch_archive::capacity::{App, EvictionLedger, HandedOver};
use flinch_archive::executor::lifecycle::{Action, Route, Withdrawn};
use flinch_archive::executor::radarr::Radarr;
use flinch_archive::executor::seerr::Seerr;
use flinch_archive::executor::sonarr::Sonarr;
use flinch_archive::executor::{DeletedItem, Evicted, ExecutorError, Leaving, LeavingSoonShelf, NativeState, NativeStatus};
use flinch_archive::executor::{Shelf, ShelfGroup, ShelfServer};
use flinch_archive::ids::ArrRef;
use flinch_archive::jellyfin::{JellyfinClient, JellyfinCollections};
use flinch_archive::plex::collections::PlexCollections;
use flinch_archive::{ArchiveCard, LibraryKind};
use std::collections::{BTreeMap, HashMap, HashSet};

pub(super) struct Plex<'a> {
    collections: PlexCollections<'a>,
    pub(super) base: &'a str,
    pub(super) token: &'a str,
}

pub(super) struct Jellyfin<'a> {
    collections: JellyfinCollections<'a>,
    pub(super) client: &'a JellyfinClient,
}

/// One item on its way onto a shelf.
struct Shelving {
    id: String,
    until: u64,
    rating_key: String,
}

pub(super) struct Actor<'a> {
    cycle: &'a Cycle<'a>,
    seerr: Option<Seerr<'a>>,
    pub(super) plex: Option<Plex<'a>>,
    pub(super) jellyfin: Option<Jellyfin<'a>>,
    /// The Leaving Soon title when announcing is possible, and its server.
    shelf: (Option<&'a str>, ShelfServer),
    pub(super) cards: HashMap<&'a str, &'a ArchiveCard>,
    /// This run's clock; the cycle's evidence was read at `library.now`.
    now: u64,
}

impl<'a> Actor<'a> {
    pub(super) fn new(
        cycle: &'a Cycle<'a>,
        plex: Option<(&'a str, &'a str)>,
        jellyfin: Option<&'a JellyfinClient>,
        shelf: (Option<&'a str>, ShelfServer),
        now: u64,
    ) -> Self {
        let (args, http, dry_run) = (cycle.args, cycle.http, cycle.dry_run);
        let seerr_on = cycle.settings.native.seerr_cleanup && !args.seerr_url.trim().is_empty() && !args.seerr_key.trim().is_empty();
        Self {
            cycle,
            seerr: seerr_on.then(|| Seerr::new(http, &args.seerr_url, &args.seerr_key, dry_run)),
            plex: plex.map(|(base, token)| Plex { collections: PlexCollections::new(http, base, token, dry_run), base, token }),
            jellyfin: jellyfin.map(|client| Jellyfin { collections: JellyfinCollections::new(client, dry_run), client }),
            shelf,
            cards: cycle.library.cards.iter().map(|card| (card.id.as_str(), card)).collect(),
            now,
        }
    }

    /// The Radarr instance `instance` (empty: the default); `None` when it is
    /// not configured this cycle.
    pub(super) fn radarr(&self, instance: &str) -> Option<Radarr<'a>> {
        let cycle = self.cycle;
        cycle.args.arr(App::Radarr, instance).map(|arr| Radarr::new(cycle.http, &arr.base, &arr.key, cycle.dry_run))
    }

    /// The Sonarr instance `instance` (see [`Actor::radarr`]).
    pub(super) fn sonarr(&self, instance: &str) -> Option<Sonarr<'a>> {
        let cycle = self.cycle;
        cycle.args.arr(App::Sonarr, instance).map(|arr| Sonarr::new(cycle.http, &arr.base, &arr.key, cycle.dry_run))
    }

    fn title(&self, id: &str, state: &NativeState) -> String {
        self.cards
            .get(id)
            .map(|card| card.title.clone())
            .or_else(|| state.leaving.get(id).map(|entry| entry.title.clone()))
            .unwrap_or_else(|| id.to_string())
    }

    /// The Plex item the card is: a season's own ratingKey for a season.
    pub(super) fn plex_item(&self, id: &str) -> Option<(String, u32)> {
        let ids = self.cycle.plex_ids.get(id)?;
        let key = match self.cards.get(id)?.kind {
            LibraryKind::Movie => ids.rating_key.clone(),
            LibraryKind::Season => ids.season_rating_key.clone()?,
        };
        Some((key, ids.section_id?))
    }

    /// The server's shelf, when its client is configured.
    fn shelf_on(&self, server: ShelfServer) -> Option<Shelf<'_, 'a>> {
        match server {
            ShelfServer::Plex => self.plex.as_ref().map(|plex| Shelf::Plex(&plex.collections)),
            ShelfServer::Jellyfin => self.jellyfin.as_ref().map(|jellyfin| Shelf::Jellyfin(&jellyfin.collections)),
        }
    }

    /// Where the card goes on `server`'s shelf, and under which key.
    fn shelf_item(&self, id: &str, server: ShelfServer) -> Option<(String, ShelfGroup)> {
        let season = self.cards.get(id)?.kind == LibraryKind::Season;
        match server {
            ShelfServer::Plex => self.plex_item(id).map(|(key, section)| (key, ShelfGroup { section: Some(section), season })),
            ShelfServer::Jellyfin => self.jellyfin_item(id).map(|key| (key.to_string(), ShelfGroup { section: None, season })),
        }
    }

    /// The Jellyfin/Emby item the card is: the movie, or the season.
    pub(super) fn jellyfin_item(&self, id: &str) -> Option<&'a str> {
        self.cycle.jellyfin_ids.get(id).map(String::as_str)
    }

    pub(super) fn http(&self) -> &'a reqwest::Client {
        self.cycle.http
    }

    pub(super) fn cycle_settings(&self) -> &'a flinch_archive::daemon::RuntimeSettings {
        self.cycle.settings
    }

    fn book(&self, ledger: &mut EvictionLedger, id: &str, title: &str) {
        let (Some(card), Some(volume)) = (self.cards.get(id), self.cycle.governance.volume_for(id)) else { return };
        let app = match card.kind {
            LibraryKind::Movie => App::Radarr,
            LibraryKind::Season => App::Sonarr,
        };
        ledger.record(HandedOver { id, title, app, volume: &volume, bytes: card.size_bytes }, self.now);
    }

    pub(super) async fn execute(
        &self,
        actions: Vec<Action>,
        state: &mut NativeState,
        ledger: &mut EvictionLedger,
        status: &mut NativeStatus,
    ) {
        let mut announce = Vec::new();
        let mut deleted = Vec::new();
        for action in actions {
            match action {
                Action::Withdraw { id, why } => self.withdraw(&id, why, state, ledger, status).await,
                Action::Delete { id, route } => {
                    if self.delete(&id, route, state, ledger, status).await {
                        deleted.push(id);
                    }
                }
                Action::Announce { id, until } => announce.push((id, until)),
            }
        }
        self.announce(announce, state, ledger, status).await;
        self.remove_torrents(&deleted, state, status).await;
    }

    async fn withdraw(&self, id: &str, why: Withdrawn, state: &mut NativeState, ledger: &mut EvictionLedger, status: &mut NativeStatus) {
        let Some(entry) = state.leaving.get(id).cloned() else { return };
        if let Some(shelf) = self.shelf_on(entry.server) {
            if let Err(error) = shelf.take_off(&entry.collection, &entry.rating_key).await {
                // An item gone from the library may be gone from the server too.
                if why != Withdrawn::Gone {
                    status.failures += 1;
                    status.problems.push(format!("{}: taking it back from Leaving Soon failed, retried next run: {error}", entry.title));
                    return;
                }
            }
        }
        status.withdrawn += 1;
        if self.cycle.dry_run {
            status.simulated += 1;
            return;
        }
        println!("[flinch-arrd] native: {} taken back from Leaving Soon: {why}", entry.title);
        state.leaving.remove(id);
        ledger.forget(id);
    }

    /// Announce in batches: one shelf per Plex section, or the one
    /// Jellyfin/Emby shelf, created (seeded with its first items) when missing.
    async fn announce(&self, batch: Vec<(String, u64)>, state: &mut NativeState, ledger: &mut EvictionLedger, status: &mut NativeStatus) {
        let (Some(title), server) = self.shelf else { return };
        let Some(shelf) = self.shelf_on(server) else { return };
        let mut groups: BTreeMap<ShelfGroup, Vec<Shelving>> = BTreeMap::new();
        for (id, until) in batch {
            match self.shelf_item(&id, server) {
                Some((rating_key, group)) => groups.entry(group).or_default().push(Shelving { id, until, rating_key }),
                None => {
                    status.held.push(format!("{}: not matched in {}, so it cannot be announced", self.title(&id, state), server.label()))
                }
            }
        }
        for (group, members) in groups {
            let place = match group.section {
                Some(section) => format!("Leaving Soon in Plex section {section}"),
                None => format!("Leaving Soon in {}", server.label()),
            };
            let keys: Vec<String> = members.iter().map(|member| member.rating_key.clone()).collect();
            let landed = shelf.shelve(group, title, &keys).await;
            let bytes: u64 = members.iter().filter_map(|member| self.cards.get(member.id.as_str()).map(|card| card.size_bytes)).sum();
            match landed {
                // A season the server will not hold fails its read-back: held, never deleted unannounced.
                Err(error) => {
                    status.failures += members.len();
                    status.problems.push(format!("{place}: {error}"));
                }
                Ok(_) if self.cycle.dry_run => {
                    status.simulated += members.len();
                    status.announced += members.len();
                    status.announced_bytes += bytes;
                }
                Ok(None) => status.problems.push(format!("{place}: the collection could not be created")),
                Ok(Some(shelved)) => {
                    if let (true, Some(section)) = (shelved.created, group.section) {
                        state.created.insert(shelved.collection.clone(), section);
                    }
                    status.announced += members.len();
                    status.announced_bytes += bytes;
                    for Shelving { id, until, rating_key } in members {
                        let Some(card) = self.cards.get(id.as_str()) else { continue };
                        println!("[flinch-arrd] native: {} on Leaving Soon until {until}", card.title);
                        self.book(ledger, &id, &card.title);
                        let entry = Leaving {
                            title: card.title.clone(),
                            kind: card.kind,
                            bytes: card.size_bytes,
                            announced_at: self.now,
                            until,
                            shelf: title.to_string(),
                            collection: shelved.collection.clone(),
                            rating_key,
                            server,
                        };
                        state.leaving.insert(id, entry);
                    }
                }
            }
        }
    }

    /// One delete: last look, the *arr write read back, then the bookkeeping.
    /// True once deleted (or, in a dry run, once it would have been).
    async fn delete(
        &self,
        id: &str,
        route: Route,
        state: &mut NativeState,
        ledger: &mut EvictionLedger,
        status: &mut NativeStatus,
    ) -> bool {
        let title = self.title(id, state);
        let (Some(card), Some(target)) = (self.cards.get(id).copied(), ArrRef::card(id)) else {
            status.problems.push(format!("{title}: not a Radarr movie or Sonarr season on disk"));
            return false;
        };
        let baseline = match route {
            Route::LeavingSoon => state.leaving.get(id).map_or(self.cycle.library.now, |entry| entry.announced_at),
            Route::Finished => self.cycle.library.now,
        };
        if let Err(why) = super::look::last_look(self, id, baseline).await {
            status.held.push(format!("{title}: kept, {why}"));
            return false;
        }
        if route == Route::LeavingSoon && !self.still_shelved(id, &title, state, ledger, status).await {
            return false;
        }
        let config = &self.cycle.settings.native;
        let evicted = match (target.app, target.season) {
            (App::Radarr, _) => match self.radarr(target.instance) {
                Some(radarr) => radarr.evict(target.id, config.delete_mode, config.add_import_exclusion).await,
                None => return self.unconfigured(&title, target, status),
            },
            (App::Sonarr, season) => match (self.sonarr(target.instance), season) {
                (Some(sonarr), Some(season)) => sonarr.evict(target.id, season).await,
                _ => return self.unconfigured(&title, target, status),
            },
        };
        let restore = match evicted {
            Err(error) => {
                status.failures += 1;
                status.problems.push(format!("{title}: delete failed, retried next run: {error}"));
                return false;
            }
            Ok(Evicted::Simulated) => None,
            Ok(Evicted::Done(restore)) => Some(restore),
        };
        status.deleted += 1;
        status.deleted_bytes += card.size_bytes;
        let seerr = self.clear_seerr(target).await;
        let Some(restore) = restore else {
            status.simulated += 1;
            return true;
        };
        println!("[flinch-arrd] native: deleted {title} ({route:?})");
        self.book(ledger, id, &title);
        if let Some(entry) = state.leaving.remove(id) {
            if let Some(Err(error)) = self.take_off(&entry).await {
                eprintln!("[flinch-arrd] native: {title} deleted but still on the Leaving Soon shelf: {error}");
            }
        }
        state.deleted.push(DeletedItem {
            id: id.to_string(),
            title,
            kind: card.kind,
            bytes: card.size_bytes,
            deleted_at: self.now,
            announced: route == Route::LeavingSoon,
            target: restore,
            seerr,
            restored_at: None,
        });
        true
    }

    /// An item whose instance this cycle does not read stays where it is.
    fn unconfigured(&self, title: &str, target: ArrRef<'_>, status: &mut NativeStatus) -> bool {
        let instance = flinch_archive::ids::instance_key(target.app, target.instance);
        status.problems.push(format!("{title}: {instance} is not configured, kept"));
        false
    }

    async fn take_off(&self, entry: &Leaving) -> Option<Result<(), flinch_archive::executor::ShelfError>> {
        Some(self.shelf_on(entry.server)?.take_off(&entry.collection, &entry.rating_key).await)
    }

    /// A Leaving Soon delete needs the warning to have stood: an item someone
    /// took off the shelf by hand is forgotten there, so it is announced again
    /// with a fresh window rather than deleted unannounced.
    async fn still_shelved(
        &self,
        id: &str,
        title: &str,
        state: &mut NativeState,
        ledger: &mut EvictionLedger,
        status: &mut NativeStatus,
    ) -> bool {
        let Some(entry) = state.leaving.get(id) else { return false };
        let Some(shelf) = self.shelf_on(entry.server) else { return false };
        match shelf.members(&entry.collection).await {
            Ok(members) if members.contains(&entry.rating_key) => true,
            Ok(_) => {
                status.problems.push(format!(
                    "{title}: no longer on the Leaving Soon shelf in {}; announced again, its window restarts",
                    entry.server.label()
                ));
                if !self.cycle.dry_run {
                    state.leaving.remove(id);
                    ledger.forget(id);
                }
                false
            }
            Err(error) => {
                status.held.push(format!("{title}: kept, the Leaving Soon shelf could not be read: {error}"));
                false
            }
        }
    }

    async fn clear_seerr(&self, target: ArrRef<'_>) -> Option<String> {
        let seerr = self.seerr.as_ref()?;
        let library = self.cycle.library;
        let result = match (target.app, target.season) {
            (App::Radarr, _) => {
                let movie = library.movies.iter().find(|row| row.id == target.id && row.instance == target.instance);
                match movie.and_then(|row| row.tmdb_id) {
                    Some(tmdb) => seerr.clear_movie(tmdb).await,
                    None => return Some("no TMDB id, Seerr left as it is".into()),
                }
            }
            (App::Sonarr, season) => {
                let show = library.series.iter().find(|row| row.id == target.id && row.instance == target.instance);
                match (show.and_then(|row| row.tmdb_id), season) {
                    (Some(tmdb), Some(season)) => seerr.clear_season(tmdb, season).await,
                    _ => return Some("no TMDB id, Seerr left as it is".into()),
                }
            }
        };
        Some(result.map_or_else(|error: ExecutorError| format!("Seerr cleanup failed: {error}"), |cleared| cleared.to_string()))
    }

    /// Torrents of what left, once every card each one holds is gone and its
    /// seed goal allows; the data goes with them.
    async fn remove_torrents(&self, deleted: &[String], state: &NativeState, status: &mut NativeStatus) {
        let (Some(session), holdings) = self.cycle.torrents else { return };
        let config = &self.cycle.settings.torrents;
        let gone: HashSet<&str> = deleted.iter().map(String::as_str).chain(state.deleted.iter().map(|item| item.id.as_str())).collect();
        let mut done: HashSet<(usize, &str)> = HashSet::new();
        for holding in deleted.iter().filter_map(|id| holdings.get(id)).flatten() {
            if !config.may_remove(holding) || !holding.cards.iter().all(|card| gone.contains(card.as_str())) {
                continue;
            }
            if !done.insert((holding.client_index, holding.hash.as_str())) {
                continue;
            }
            if self.cycle.dry_run {
                println!("[dry-run] would remove torrent {} from {} with its data", holding.hash, holding.client);
                status.simulated += 1;
                continue;
            }
            if let Err(error) = session.remove(holding, true).await {
                status.problems.push(format!("torrent {}: removal failed: {error}", holding.hash));
            }
        }
    }
}
