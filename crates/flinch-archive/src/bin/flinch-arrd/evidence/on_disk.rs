//! Which episodes of a season are on disk, read from Sonarr for the seasons
//! whose plays would otherwise read complete.
//!
//! The play logs remember episodes deleted since; counted against the files
//! left they made a season read complete and leave unannounced (see
//! `flinch_archive::plex::season`). Without its episode numbers such a season
//! reads just short of complete, so these reads only ever let a season the
//! household really finished read complete again.

use crate::fetch::fetch_series_episodes;
use crate::Args;
use flinch_archive::arr::ArrSeries;
use flinch_archive::plex::season::UNVERIFIED;
use flinch_archive::plex::{history, PlexMetadata, Resolution, SonarrEpisodes, WatchTarget};
use flinch_archive::tautulli::{self, TautulliRow};
use flinch_archive::{ArchiveCard, LibraryKind};
use std::collections::{BTreeSet, HashMap};

/// Set `episodes_on_disk` on every season on disk whose Plex history or
/// Tautulli plays reach the cap a season with unknown episodes stops at.
///
/// Request bound: at most one `/api/v3/episode` read per series per cycle, and
/// only for a series with such a season — never more than the per-series file
/// date reads the inventory makes every cycle. `read` holds the series already
/// read this cycle (keyed by series id) and is reused. A read that fails leaves
/// its seasons unknown, and so capped below complete: announced through
/// Leaving Soon, never deleted unannounced. Skipping a read is always the safe
/// side, which is why no read is retried.
pub(super) async fn attach(
    http: &reqwest::Client,
    args: &Args,
    series: &[ArrSeries],
    targets: &mut [WatchTarget],
    (resolution, plex_rows, tautulli_rows): (&Resolution, &[PlexMetadata], &[TautulliRow]),
    read: &mut HashMap<u32, SonarrEpisodes>,
) {
    let from_history = history::history_entries(targets, resolution, plex_rows);
    let from_tautulli = tautulli::plays_by_target(targets, resolution, tautulli_rows);
    let capped = |id: &str| [&from_history, &from_tautulli].iter().any(|entries| entries.get(id).is_some_and(|e| e.progress >= UNVERIFIED));
    let waiting: BTreeSet<String> = targets
        .iter()
        .filter(|target| target.kind == LibraryKind::Season && target.on_disk && target.episodes_on_disk.is_none())
        .map(|target| target.id.clone())
        .filter(|id| capped(id))
        .collect();
    if waiting.is_empty() {
        return;
    }
    let mut seasons_of: HashMap<&str, (u32, u32)> = HashMap::new();
    for series_item in series {
        for season in &series_item.seasons {
            let id = series_item.season_card_id(season.season_number);
            if let Some(id) = waiting.get(&id) {
                seasons_of.insert(id.as_str(), (series_item.id, season.season_number));
            }
        }
    }
    let unread: BTreeSet<u32> = seasons_of.values().map(|(series_id, _)| *series_id).filter(|id| !read.contains_key(id)).collect();
    for series_id in unread {
        match fetch_series_episodes(http, args, series_id).await {
            Ok(episodes) => {
                read.insert(series_id, episodes);
            }
            Err(error) => {
                eprintln!("[flinch-arrd] sonarr episodes of series {series_id} unreadable, its seasons stay below complete: {error:#}")
            }
        }
    }
    let mut known = 0;
    for target in targets.iter_mut() {
        let Some((series_id, season)) = seasons_of.get(target.id.as_str()) else { continue };
        let numbers = read.get(series_id).and_then(|episodes| episodes.on_disk(*season, target.episode_files.unwrap_or(0)));
        known += usize::from(numbers.is_some());
        target.episodes_on_disk = numbers;
    }
    println!("[flinch-arrd] seasons whose plays would read complete: {known} of {} checked against the episodes on disk", waiting.len());
}

/// Carry the numbers read onto the cards, for the planner and the item view.
pub(super) fn onto_cards(cards: &mut [ArchiveCard], targets: &[WatchTarget]) {
    let numbers: HashMap<&str, &Vec<u32>> =
        targets.iter().filter_map(|target| Some((target.id.as_str(), target.episodes_on_disk.as_ref()?))).collect();
    for card in cards.iter_mut() {
        if let Some(on_disk) = numbers.get(card.id.as_str()) {
            card.episodes_on_disk = Some((*on_disk).clone());
        }
    }
}
