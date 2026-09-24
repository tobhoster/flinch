//! Deletions FLINCH did not make: files Radarr or Sonarr recorded as removed
//! that no FLINCH hand-over explains.
//!
//! FLINCH deletes nothing itself; what it hands to Maintainerr is remembered
//! in the eviction ledger ([`crate::capacity::EvictionLedger::handoffs`]).
//! Everything else that removed a file — a person in the *arr's UI, a script
//! on its API, Maintainerr's own rules, a file that vanished from disk — gets
//! none of the cleanup a hand-over gets. Listing each one with whether the
//! *arr still monitors it shows which will simply be downloaded again.
//!
//! A movie removed from Radarr outright takes its history with it, so only
//! deletions that left the entry behind can be listed — exactly the ones that
//! can come back.

use crate::arr::{ArrMovie, ArrSeries};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// How far back deletions are listed.
pub const WINDOW_SECS: u64 = 30 * 86_400;

/// At most this many items are listed, newest first.
pub const LIMIT: usize = 50;

/// How a file left, in the *arr's words (`DeleteMediaFileReason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalReason {
    /// Deleted through the *arr: its UI, or anything with its API key.
    Manual,
    /// Gone from disk behind the *arr's back, found by a rescan.
    MissingFromDisk,
    /// Any other removal the *arr recorded.
    Other,
}

/// One removed file, as the *arr history records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Removal {
    /// The card it belonged to: `radarr-7`, or `sonarr-12-s3` for an episode.
    pub card: String,
    /// Unix seconds.
    pub at: u64,
    pub reason: RemovalReason,
}

/// One item whose files something other than FLINCH removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutsideDeletion {
    pub id: String,
    pub title: String,
    /// `S3` for a season; `None` for a movie.
    #[serde(default)]
    pub season_label: Option<String>,
    /// `movie` or `season`, as in items.json.
    pub kind: String,
    /// The latest removal, unix seconds.
    pub at_unix: u64,
    /// Files removed in the window: one for a movie, each episode's for a season.
    pub files: u32,
    /// How the latest file left.
    pub reason: RemovalReason,
    /// Whether the *arr still monitors it — for a season, the show and the
    /// season both. Monitored with no file on disk means it downloads again.
    /// `None` when the *arr did not say.
    pub monitored: Option<bool>,
    /// A file is on disk again.
    pub on_disk: bool,
}

/// What the library says about one item now.
struct Known<'a> {
    title: &'a str,
    season: Option<u32>,
    monitored: Option<bool>,
    on_disk: bool,
}

fn library<'a>(movies: &'a [ArrMovie], series: &'a [ArrSeries]) -> HashMap<String, Known<'a>> {
    let mut known = HashMap::new();
    for movie in movies {
        let entry = Known { title: &movie.title, season: None, monitored: movie.monitored, on_disk: movie.has_file };
        known.insert(format!("radarr-{}", movie.id), entry);
    }
    for show in series {
        for season in &show.seasons {
            let monitored = show.monitored.zip(season.monitored).map(|(show, season)| show && season);
            let entry = Known {
                title: &show.title,
                season: Some(season.season_number),
                monitored,
                on_disk: season.statistics.episode_file_count > 0,
            };
            known.insert(format!("sonarr-{}-s{}", show.id, season.season_number), entry);
        }
    }
    known
}

/// Every item with files removed in the last [`WINDOW_SECS`] that FLINCH did
/// not hand over first, newest first. A removal is FLINCH's when the item was
/// handed over (`handoffs`, card id → hand-over time) no later than the
/// removal. Items the *arr no longer has are left out: nothing can bring them
/// back.
pub fn outside_deletions(
    removals: &[Removal],
    handoffs: &BTreeMap<String, u64>,
    movies: &[ArrMovie],
    series: &[ArrSeries],
    now: u64,
) -> Vec<OutsideDeletion> {
    let known = library(movies, series);
    let mut by_card: BTreeMap<&str, (u64, u32, RemovalReason)> = BTreeMap::new();
    for removal in removals {
        let recent = now.saturating_sub(removal.at) <= WINDOW_SECS;
        let flinchs = handoffs.get(&removal.card).is_some_and(|handed_at| *handed_at <= removal.at);
        if !recent || flinchs {
            continue;
        }
        let (latest, files, reason) = by_card.entry(removal.card.as_str()).or_insert((removal.at, 0, removal.reason));
        *files += 1;
        if removal.at >= *latest {
            (*latest, *reason) = (removal.at, removal.reason);
        }
    }
    let mut listed: Vec<OutsideDeletion> = by_card
        .into_iter()
        .filter_map(|(card, (at_unix, files, reason))| {
            let item = known.get(card)?;
            Some(OutsideDeletion {
                id: card.to_string(),
                title: item.title.to_string(),
                season_label: item.season.map(|number| format!("S{number}")),
                kind: if item.season.is_some() { "season" } else { "movie" }.to_string(),
                at_unix,
                files,
                reason,
                monitored: item.monitored,
                on_disk: item.on_disk,
            })
        })
        .collect();
    listed.sort_by(|a, b| b.at_unix.cmp(&a.at_unix).then_with(|| a.id.cmp(&b.id)));
    listed.truncate(LIMIT);
    listed
}

#[cfg(test)]
mod tests;
