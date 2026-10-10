//! TVDB episode ids for the seasons whose Plex episode count differs from
//! Sonarr's file count (PX-04): each Plex show's episodes, and each affected
//! series' Sonarr episodes from the series' own instance.

use crate::fetch::fetch_series_episodes;
use crate::media::fetch_show_episodes;
use crate::Args;
use flinch_archive::arr::ArrSeries;
use flinch_archive::plex::{EpisodeIds, SonarrEpisodes, Unconfirmed};
use std::collections::{BTreeSet, HashMap, HashSet};

/// The ids for `unconfirmed`. `read` holds the series already read this cycle,
/// by show subject, and is reused. A read that fails leaves its seasons
/// unconfirmed.
pub(super) async fn episode_ids(
    http: &reqwest::Client,
    args: &Args,
    (plex_url, plex_token): (&str, &str),
    unconfirmed: &[Unconfirmed],
    series: &[ArrSeries],
    read: &mut HashMap<String, SonarrEpisodes>,
) -> EpisodeIds {
    let mut ids = EpisodeIds::default();
    let shows: BTreeSet<&str> = unconfirmed.iter().flat_map(|season| season.show_rating_keys.iter().map(String::as_str)).collect();
    for show in shows {
        match fetch_show_episodes(http, plex_url, plex_token, show).await {
            Ok(episodes) => {
                ids.plex.insert(show.to_string(), episodes);
            }
            Err(error) => eprintln!("[flinch-arrd] plex episodes of show {show} unreadable: {error:#}"),
        }
    }
    let waiting: HashSet<&str> = unconfirmed.iter().map(|season| season.target_id.as_str()).collect();
    for series_item in series {
        let targets: Vec<String> = series_item
            .seasons
            .iter()
            .map(|season| series_item.season_card_id(season.season_number))
            .filter(|id| waiting.contains(id.as_str()))
            .collect();
        if targets.is_empty() {
            continue;
        }
        let subject = series_item.subject();
        if let std::collections::hash_map::Entry::Vacant(slot) = read.entry(subject.clone()) {
            match fetch_series_episodes(http, args, &series_item.instance, series_item.id).await {
                Ok(episodes) => {
                    slot.insert(episodes);
                }
                Err(error) => eprintln!("[flinch-arrd] sonarr episodes of {} unreadable: {error:#}", series_item.title),
            }
        }
        if let Some(episodes) = read.get(&subject) {
            ids.sonarr.extend(targets.into_iter().map(|id| (id, episodes.clone())));
        }
    }
    ids
}
