//! The cycle's torrent read: every configured client listed once, each card
//! tied to its torrents ([`flinch_archive::torrents::map`]), hardlinks checked
//! only where they decide anything, and the holds the planner applies.
//! Nothing is written here; removal is the native executor's, after a delete.

use flinch_archive::arr::{ArrMovie, ArrSeries};
use flinch_archive::plan::knapsack::Force;
use flinch_archive::plan::MediaCandidate;
use flinch_archive::torrents::map::{self, ClientStatus, LibraryLinks, MatchedBy, TorrentStatus};
use flinch_archive::torrents::{Holding, SeedHold, Torrents, TorrentsConfig};
use flinch_archive::ArchiveCard;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Default)]
pub(super) struct Gathered {
    /// The clients, for removing a torrent after a delete; `None` when none
    /// is configured.
    pub(super) session: Option<Torrents>,
    /// Card id → the torrents holding it.
    pub(super) holdings: HashMap<String, Vec<Holding>>,
    /// Card id → why its torrents keep it this cycle.
    pub(super) holds: HashMap<String, SeedHold>,
    /// Cards below the desired ratio: drawn on last ([`spare`]).
    pub(super) spared: HashSet<String>,
    pub(super) status: Option<TorrentStatus>,
}

/// The library side of the read: the *arr items, the download ids history
/// tied to each card, and the cards with their sizes.
pub(super) struct Library<'a> {
    pub(super) movies: &'a [ArrMovie],
    pub(super) series: &'a [ArrSeries],
    pub(super) downloads: &'a BTreeMap<String, Vec<String>>,
    pub(super) cards: &'a [ArchiveCard],
}

/// Read the clients and decide the holds. `torrent_goes`: the executor
/// removes a deleted item's torrents ([`TorrentsConfig::may_remove`]).
pub(super) async fn gather(
    http: &reqwest::Client,
    config: &TorrentsConfig,
    torrent_goes: bool,
    library: Library<'_>,
    dry_run: bool,
) -> Gathered {
    if config.clients.is_empty() {
        return Gathered::default();
    }
    let session = Torrents::new(http, config, dry_run);
    let listings = session.list().await;
    let mut status = TorrentStatus { torrents_go_with_items: torrent_goes, ..TorrentStatus::default() };
    for (client, listing) in config.clients.iter().zip(&listings) {
        if let Err(error) = listing {
            eprintln!("[flinch-arrd] torrents: {error} — the items it may hold are kept");
        }
        status.clients.push(ClientStatus {
            kind: client.kind,
            host: host(&client.url),
            torrents: listing.as_ref().map_or(0, Vec::len),
            error: listing.as_ref().err().map(ToString::to_string),
        });
    }

    let folders = folders(library.movies, library.series);
    let matches = map::match_cards(library.downloads, &folders, &listings, config);
    let mut cards_of: HashMap<&str, Vec<String>> = HashMap::new();
    for (card, torrents) in &matches.cards {
        for (_, torrent, _) in torrents {
            cards_of.entry(torrent.hash.as_str()).or_default().push(card.clone());
        }
        match torrents.first().map(|(_, _, how)| *how) {
            Some(MatchedBy::History) => status.by_history += 1,
            Some(MatchedBy::Path) => status.by_path += 1,
            None => {}
        }
    }

    // Files are listed per torrent, so only where a link decides anything: a
    // torrent below a goal that is respected keeps its items whatever its
    // links, and one that goes with its only item frees its bytes either way.
    let decides = |torrent: &flinch_archive::torrents::Torrent| {
        let meets = config.meets_goal(torrent);
        let below_and_respected = !meets && config.respect_seed_goals;
        let goes = torrent_goes && config.removes(meets);
        let single = cards_of.get(torrent.hash.as_str()).is_some_and(|cards| cards.len() == 1);
        !below_and_respected && !(goes && single)
    };
    let mut files: HashMap<&str, Option<Vec<String>>> = HashMap::new();
    for (index, torrent, _) in matches.cards.values().flatten() {
        if files.contains_key(torrent.hash.as_str()) || !decides(torrent) {
            continue;
        }
        let listed = match session.files(*index, &torrent.hash).await {
            Ok(listed) => Some(listed),
            Err(error) => {
                eprintln!("[flinch-arrd] torrents: files of {} unreadable, its links count as unverified: {error}", torrent.hash);
                None
            }
        };
        files.insert(torrent.hash.as_str(), listed);
    }

    let mut links = LibraryLinks::default();
    let mut holdings: HashMap<String, Vec<Holding>> = HashMap::new();
    for (card, torrents) in &matches.cards {
        let folder = folders.get(card).map_or("", String::as_str);
        for (index, torrent, _) in torrents {
            // Unchecked links read as unverified, which only matters where they decide.
            let checked = match files.get(torrent.hash.as_str()) {
                Some(listed) => map::hardlinks(listed.as_deref(), folder, config, &mut links),
                None => (Vec::new(), false),
            };
            let cards = cards_of.get(torrent.hash.as_str()).cloned().unwrap_or_default();
            let kind = config.clients.get(*index).map(|client| client.kind);
            if let Some(kind) = kind {
                holdings.entry(card.clone()).or_default().push(map::holding(*index, kind, torrent, cards, checked, config));
            }
        }
    }
    let holds = map::holds(&holdings, &matches.unreadable, config, torrent_goes);
    let spared = map::spared(&holdings, &holds, config);
    let sizes: HashMap<&str, u64> = library.cards.iter().map(|card| (card.id.as_str(), card.size_bytes)).collect();
    status.count(&holds, |card| sizes.get(card).copied().unwrap_or(0));
    for card in &spared {
        status.spared.items += 1;
        status.spared.bytes = status.spared.bytes.saturating_add(sizes.get(card.as_str()).copied().unwrap_or(0));
    }
    println!(
        "[flinch-arrd] torrents: {} item(s) held by torrents; kept: {} below their seed goal, {} hardlinked to a torrent that stays, {} with unverified links, {} on an unreadable client; {} spared below the desired ratio",
        holdings.len(),
        status.below_goal.items,
        status.held_by_torrent.items,
        status.links_unverified.items,
        status.client_unreadable.items,
        status.spared.items,
    );
    Gathered { session: Some(session), holdings, holds, spared, status: Some(status) }
}

/// Spare every candidate below the desired ratio that nothing excludes and
/// nothing protects: [`Force::Spare`], set before the rules so an operator's
/// rule still outranks it, and published with the rules' inputs so a preview
/// plans the same way.
pub(super) fn spare(candidates: &mut [MediaCandidate], spared: &HashSet<String>) {
    for candidate in candidates.iter_mut().filter(|candidate| spared.contains(&candidate.id)) {
        if candidate.exclusion.is_none() && !candidate.protect && candidate.force.is_none() {
            candidate.force = Some(Force::Spare);
        }
    }
}

/// Card id → library folder: a movie's own, a season's show's.
fn folders(movies: &[ArrMovie], series: &[ArrSeries]) -> HashMap<String, String> {
    let movies = movies.iter().filter_map(|movie| Some((movie.card_id(), movie.path.clone()?)));
    let seasons = series.iter().flat_map(|show| {
        show.path
            .iter()
            .flat_map(move |path| show.seasons.iter().map(move |season| (show.season_card_id(season.season_number), path.clone())))
    });
    movies.chain(seasons).collect()
}

/// The URL's host and port only: never a path, query or credential.
fn host(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(|host| url.port().map_or(host.to_string(), |port| format!("{host}:{port}"))))
        .unwrap_or_default()
}
