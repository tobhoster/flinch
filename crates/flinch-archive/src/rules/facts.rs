//! What rules ask about an item beyond what the planner already knows: where
//! it lives, how it is tagged, who asked for it. Gathered once a cycle and
//! published with the candidates, so a preview judges the same facts.
//!
//! Every fact is optional: `None` means FLINCH could not read it this cycle
//! (tags unreadable, Seerr down, no theme vector), which a rule treats as
//! unknown, never as "no".

use super::Kind;
use crate::card::LibraryKind;
use crate::ids::PlexIds;
use crate::plan::candidates::{cards_of, Library};
use crate::themes::Themes;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// One request for the item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestFact {
    /// The requester's Seerr display name.
    pub requester: String,
    /// Unix seconds; `None` when Seerr gave no readable date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Facts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    /// The *arr's folder for the movie or the show.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Its *arr tag labels.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plex_section: Option<u32>,
    /// Every Seerr request naming it; `Some(empty)` when Seerr was read and
    /// nobody asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requests: Option<Vec<RequestFact>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genres: Option<Vec<String>>,
    /// The *arr's quality name (movies; Sonarr has none per season).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    /// Anyone has played it; `None` without watch evidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub played: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_played_days: Option<f32>,
    /// The season number (seasons only; 0 is the specials).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
    /// 1 for the show's newest regular season on disk, 2 for the one before…
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newest_rank: Option<u32>,
    /// The show's lowest-numbered regular season, on disk or not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_season: Option<bool>,
    /// Sonarr calls the show continuing (or upcoming); `None` without a status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuing: Option<bool>,
}

/// One instance's tag id → label ([`crate::ids`]: an empty instance is the
/// default); `None` labels when its tags were unreadable. Tag ids are per
/// instance: 3 in one Radarr is not 3 in another.
pub type InstanceTags = (crate::capacity::App, String, Option<HashMap<u32, String>>);

/// What the daemon read beside the candidates' own library.
pub struct Sources<'a> {
    pub plex_ids: &'a HashMap<String, PlexIds>,
    /// Every instance's tags; an instance not listed reads as unreadable.
    pub tags: &'a [InstanceTags],
    pub themes: &'a Themes,
}

impl Sources<'_> {
    fn tags_of(&self, app: crate::capacity::App, instance: &str) -> Option<&HashMap<u32, String>> {
        self.tags.iter().find(|(known, name, _)| *known == app && name == instance).and_then(|(_, _, labels)| labels.as_ref())
    }
}

/// What the *arrs say about one card.
struct Arr<'a> {
    kind: Kind,
    path: Option<&'a str>,
    tags: Option<Vec<String>>,
    genres: &'a [String],
    quality: Option<String>,
    season: Option<SeasonPlace>,
}

/// Where a season sits in its show, for rolling retention.
#[derive(Clone, Copy)]
struct SeasonPlace {
    number: u32,
    newest_rank: Option<u32>,
    first: bool,
    continuing: Option<bool>,
}

/// Each season's place: ranked newest first among regular seasons with
/// files (as [`crate::arr::ArrSeries::to_cards`] counts them).
fn places(show: &crate::arr::ArrSeries) -> HashMap<u32, SeasonPlace> {
    let continuing = show.status.as_deref().map(|status| matches!(status, "continuing" | "upcoming"));
    let first = show.seasons.iter().map(|season| season.season_number).filter(|number| *number > 0).min();
    let mut on_disk: Vec<u32> = show
        .seasons
        .iter()
        .filter(|season| season.season_number > 0 && (season.statistics.episode_file_count > 0 || season.statistics.size_on_disk > 0))
        .map(|season| season.season_number)
        .collect();
    on_disk.sort_unstable_by(|a, b| b.cmp(a));
    show.seasons
        .iter()
        .map(|season| {
            let number = season.season_number;
            let newest_rank = on_disk.iter().position(|n| *n == number).and_then(|rank| u32::try_from(rank + 1).ok());
            (number, SeasonPlace { number, newest_rank, first: Some(number) == first, continuing })
        })
        .collect()
}

fn labels(ids: &[u32], names: Option<&HashMap<u32, String>>) -> Option<Vec<String>> {
    names.map(|names| ids.iter().filter_map(|id| names.get(id).cloned()).collect())
}

/// Facts for every card in `library`, keyed by card id.
pub fn gather(library: &Library, sources: &Sources) -> HashMap<String, Facts> {
    let mut arr: HashMap<String, Arr> = HashMap::new();
    for movie in library.movies {
        let tags = labels(&movie.tags, sources.tags_of(crate::capacity::App::Radarr, &movie.instance));
        let entry =
            Arr { kind: Kind::Movie, path: movie.path.as_deref(), tags, genres: &movie.genres, quality: movie.quality(), season: None };
        arr.insert(movie.card_id(), entry);
    }
    for show in library.series {
        let places = places(show);
        for season in &show.seasons {
            let tags = labels(&show.tags, sources.tags_of(crate::capacity::App::Sonarr, &show.instance));
            let place = places.get(&season.season_number).copied();
            let entry = Arr { kind: Kind::Season, path: show.path.as_deref(), tags, genres: &show.genres, quality: None, season: place };
            arr.insert(show.season_card_id(season.season_number), entry);
        }
    }
    let requests = library.signals.requests_read.then(|| requests(library));
    library
        .cards
        .iter()
        .map(|card| {
            let known = arr.remove(&card.id);
            let watch = library.watch.get(&card.id);
            let place = known.as_ref().and_then(|arr| arr.season);
            let facts = Facts {
                kind: Some(known.as_ref().map_or(
                    match card.kind {
                        LibraryKind::Movie => Kind::Movie,
                        LibraryKind::Season => Kind::Season,
                    },
                    |arr| arr.kind,
                )),
                path: known.as_ref().and_then(|arr| arr.path).map(str::to_owned),
                plex_section: sources.plex_ids.get(&card.id).and_then(|ids| ids.section_id),
                requests: requests.as_ref().map(|by_card| by_card.get(card.id.as_str()).cloned().unwrap_or_default()),
                theme: sources.themes.name_of(crate::embedding::subject_of(&card.id)).map(str::to_owned),
                genres: known.as_ref().filter(|arr| !arr.genres.is_empty()).map(|arr| arr.genres.to_vec()),
                played: watch.map(|entry| card.last_watched_days.is_some() || entry.progress > 0.0),
                last_played_days: card.last_watched_days,
                tags: known.as_ref().and_then(|arr| arr.tags.clone()),
                season: place.map(|place| place.number),
                newest_rank: place.and_then(|place| place.newest_rank),
                first_season: place.map(|place| place.first),
                continuing: place.and_then(|place| place.continuing),
                quality: known.and_then(|arr| arr.quality),
            };
            (card.id.clone(), facts)
        })
        .collect()
}

/// Card id → every request naming it.
fn requests<'a>(library: &Library<'a>) -> HashMap<&'a str, Vec<RequestFact>> {
    let known: HashSet<&'a str> = library.cards.iter().map(|card| card.id.as_str()).collect();
    let mut by_card: HashMap<&'a str, Vec<RequestFact>> = HashMap::new();
    for request in &library.signals.requests {
        for card in cards_of(library, &known, &request.media, &request.seasons) {
            by_card.entry(card).or_default().push(RequestFact { requester: request.requester.clone(), at: request.requested_at });
        }
    }
    by_card
}

#[cfg(test)]
mod tests;
