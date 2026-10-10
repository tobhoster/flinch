//! Join a Jellyfin/Emby read to the library and turn it into evidence.
//!
//! A movie joins by its TMDB or IMDb id, an episode by its series' TVDB, TMDB
//! or IMDb id plus its season number; an id two targets share joins neither.
//! A season joins only when the episodes the server lists for it number
//! exactly Sonarr's episode files: the two can order seasons differently, and
//! a disagreement would land the evidence on the wrong season.

use super::{JellyfinItem, JellyfinRead, UserData};
use crate::card::LibraryKind;
use crate::fit::plays::{Play, Viewer};
use crate::ids::ExternalIds;
use crate::plex::season::{EpisodePlays, UNVERIFIED};
use crate::plex::WatchTarget;
use crate::watch::{WatchEntry, WatchSource};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// One item's dated Jellyfin plays, oldest first: its own, and the ones that
/// speak for its audience (the movie's own, or every episode of its show).
/// Persisted per card id in [`PLAYS_FILE`] so the fitter sees what the daemon did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CardPlays {
    pub item: Vec<Play>,
    pub audience: Vec<Play>,
}

/// The state-directory file holding the last cycle's [`CardPlays`] by card id.
pub const PLAYS_FILE: &str = "jellyfin-plays.json";

/// What one Jellyfin read says about the library.
#[derive(Debug, Default)]
pub struct JellyfinEvidence {
    /// Card id → watch state. Empty unless the read was complete: an item a
    /// skipped user played would otherwise read as never played.
    pub entries: HashMap<String, WatchEntry>,
    /// Card id → its plays.
    pub plays: HashMap<String, CardPlays>,
    /// Targets the server holds, joined by catalogue id.
    pub resolved: usize,
    /// Card id → the server's own item: the movie, or the Season its episodes
    /// name (`SeasonId`). Absent where episodes disagree, so the shelf never
    /// holds the wrong season. Kept next to `PlexIds` for the Leaving Soon shelf.
    pub item_ids: HashMap<String, String>,
}

/// Per-target accumulation across every user.
#[derive(Default)]
struct Acc {
    last: Option<u64>,
    touched: bool,
    movie_played: bool,
    movie_fraction: f32,
    episodes: EpisodePlays,
    /// Server item ids the target joined: the movie's, or its episodes' season's.
    ids: HashSet<String>,
    /// Distinct episode files the server lists for the season.
    listed: HashSet<String>,
}

pub fn evidence(targets: &[WatchTarget], read: &JellyfinRead) -> JellyfinEvidence {
    let index = Index::new(targets);
    let series: HashMap<&str, &JellyfinItem> =
        read.users.iter().flat_map(|user| &user.items).filter(|item| item.kind == "Series").map(|item| (item.id.as_str(), item)).collect();
    let mut accs: HashMap<usize, Acc> = HashMap::new();
    let mut item_plays: HashMap<usize, Vec<Play>> = HashMap::new();
    let mut series_plays: HashMap<&str, Vec<Play>> = HashMap::new();
    for user in &read.users {
        for item in &user.items {
            let empty = UserData::default();
            let data = item.user_data.as_ref().unwrap_or(&empty);
            let target = match item.kind.as_str() {
                "Movie" => index.movie(item),
                "Episode" if !item.is_virtual() => {
                    let show = item.series_id.as_deref().and_then(|id| series.get(id));
                    show.zip(item.parent_index_number).and_then(|(show, season)| index.season(show, season))
                }
                _ => None,
            };
            let play = play(item, data, &user.user_id);
            if let (Some(play), Some(series_id)) = (&play, item.series_id.as_deref().filter(|_| item.kind == "Episode")) {
                series_plays.entry(series_id).or_default().push(play.clone());
            }
            let Some(target) = target else { continue };
            let acc = accs.entry(target).or_default();
            match item.kind.as_str() {
                "Movie" => acc.ids.extend(Some(item.id.clone()).filter(|id| !id.is_empty())),
                _ => acc.ids.extend(item.season_id.clone().filter(|id| !id.is_empty())),
            }
            if item.kind == "Episode" {
                acc.listed.insert(item.id.clone());
            }
            if !data.touched() {
                continue;
            }
            acc.touched = true;
            acc.last = acc.last.max(play.as_ref().map(|play| play.epoch));
            if item.kind == "Movie" {
                acc.movie_played |= data.played;
                acc.movie_fraction = acc.movie_fraction.max(fraction(item, data));
            } else {
                acc.episodes.record(item.index_number, data.played);
            }
            if let Some(play) = play {
                item_plays.entry(target).or_default().push(play);
            }
        }
    }
    // PX-04's rule for Jellyfin: a season counts only when the server lists as
    // many episodes as Sonarr holds files.
    accs.retain(|target, acc| {
        let target = &targets[*target];
        target.kind == LibraryKind::Movie || target.episode_files.is_none_or(|files| acc.listed.len() == files as usize)
    });
    let mut out = JellyfinEvidence { resolved: accs.len(), ..JellyfinEvidence::default() };
    for (&at, acc) in &accs {
        if let (1, Some(id)) = (acc.ids.len(), acc.ids.iter().next()) {
            out.item_ids.insert(targets[at].id.clone(), id.clone());
        }
    }
    // Season target → the server's series that are its show.
    let mut shows: HashMap<usize, Vec<&str>> = HashMap::new();
    for show in series.values() {
        for at in index.season_targets(show) {
            shows.entry(at).or_default().push(show.id.as_str());
        }
    }
    for (&at, acc) in &accs {
        let target = &targets[at];
        if read.complete {
            out.entries.insert(target.id.clone(), entry(target, acc));
        }
        let mut item = item_plays.remove(&at).unwrap_or_default();
        item.sort_by_key(|play| play.epoch);
        let mut audience = match target.kind {
            LibraryKind::Movie => item.clone(),
            LibraryKind::Season => {
                shows.get(&at).into_iter().flatten().flat_map(|show| series_plays.get(show).into_iter().flatten().cloned()).collect()
            }
        };
        audience.sort_by_key(|play| play.epoch);
        out.plays.insert(target.id.clone(), CardPlays { item, audience });
    }
    out
}

fn entry(target: &WatchTarget, acc: &Acc) -> WatchEntry {
    let progress = match (target.kind, acc.touched) {
        (_, false) => 0.0,
        (LibraryKind::Movie, true) if acc.movie_played => 1.0,
        // Started, or once played and since marked unplayed: touched, unfinished.
        (LibraryKind::Movie, true) => acc.movie_fraction.clamp(0.01, UNVERIFIED),
        (LibraryKind::Season, true) => acc.episodes.progress(target),
    };
    WatchEntry { id: target.id.clone(), last_watched_epoch: acc.last, progress, source: WatchSource::Jellyfin }
}

fn fraction(item: &JellyfinItem, data: &UserData) -> f32 {
    if data.played {
        return 1.0;
    }
    match item.run_time_ticks.filter(|ticks| *ticks > 0) {
        Some(runtime) => (data.playback_position_ticks as f64 / runtime as f64).clamp(0.0, 1.0) as f32,
        None => 0.0,
    }
}

/// A user's last play of an item. Jellyfin keeps only the latest date per user
/// and item, so a play exists only where `LastPlayedDate` does.
fn play(item: &JellyfinItem, data: &UserData, user: &str) -> Option<Play> {
    if !data.touched() {
        return None;
    }
    let epoch = parse_utc(data.last_played_date.as_deref()?)?;
    let episode = item.index_number.filter(|_| item.kind == "Episode");
    Some(Play { epoch, episode, viewer: Some(Viewer::JellyfinUser(user.to_string())), fraction: fraction(item, data) })
}

/// `YYYY-MM-DDTHH:MM:SS[.fraction][Z|±HH:MM]` as epoch seconds; no zone reads
/// as UTC, which is what both servers store.
pub(crate) fn parse_utc(text: &str) -> Option<u64> {
    let text = text.trim();
    let (date, time) = text.split_once('T')?;
    let mut date = date.splitn(3, '-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let (clock, offset) = match time.find(['Z', 'z', '+', '-']) {
        Some(at) => (&time[..at], &time[at..]),
        None => (time, ""),
    };
    let clock = clock.split('.').next()?;
    let mut clock = clock.splitn(3, ':').map(str::parse::<i64>);
    let (hour, minute, second) = (clock.next()?.ok()?, clock.next()?.ok()?, clock.next().unwrap_or(Ok(0)).ok()?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let shift = match offset.chars().next() {
        Some(sign @ ('+' | '-')) => {
            let (h, m) = offset[1..].split_once(':').unwrap_or((&offset[1..], "0"));
            let minutes = h.parse::<i64>().ok()? * 60 + m.parse::<i64>().ok()?;
            if sign == '+' {
                minutes * 60
            } else {
                -minutes * 60
            }
        }
        _ => 0,
    };
    let epoch = days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second - shift;
    u64::try_from(epoch).ok()
}

/// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Catalogue ids → target index; `None` marks an id two targets share.
struct Index {
    movies: HashMap<String, Option<usize>>,
    seasons: HashMap<(String, u32), Option<usize>>,
    shows: HashMap<String, HashSet<usize>>,
}

fn keys(ids: &ExternalIds) -> Vec<String> {
    let mut keys = Vec::new();
    keys.extend(ids.tvdb.map(|id| format!("tvdb:{id}")));
    keys.extend(ids.tmdb.map(|id| format!("tmdb:{id}")));
    keys.extend(ids.imdb.as_deref().map(str::trim).filter(|id| !id.is_empty()).map(|id| format!("imdb:{}", id.to_ascii_lowercase())));
    keys
}

fn item_keys(item: &JellyfinItem) -> Vec<String> {
    let number = |name: &str| item.provider(name).and_then(|id| id.parse::<u32>().ok());
    keys(&ExternalIds { tvdb: number("Tvdb"), tmdb: number("Tmdb"), imdb: item.provider("Imdb").map(str::to_string) })
}

impl Index {
    fn new(targets: &[WatchTarget]) -> Self {
        let mut index = Self { movies: HashMap::new(), seasons: HashMap::new(), shows: HashMap::new() };
        let claim = |slot: &mut Option<usize>, at: usize| {
            if *slot != Some(at) {
                *slot = None;
            }
        };
        for (at, target) in targets.iter().enumerate() {
            for key in keys(&target.external) {
                match (target.kind, target.season_index) {
                    (LibraryKind::Movie, _) => {
                        index.movies.entry(key).and_modify(|slot| claim(slot, at)).or_insert(Some(at));
                    }
                    (LibraryKind::Season, Some(season)) => {
                        index.shows.entry(key.clone()).or_default().insert(at);
                        index.seasons.entry((key, season)).and_modify(|slot| claim(slot, at)).or_insert(Some(at));
                    }
                    (LibraryKind::Season, None) => {}
                }
            }
        }
        index
    }

    /// The one target every id of the item names, or none.
    fn unique(found: impl Iterator<Item = Option<usize>>) -> Option<usize> {
        let mut only = None;
        for slot in found {
            let at = slot?;
            if only.is_some_and(|seen| seen != at) {
                return None;
            }
            only = Some(at);
        }
        only
    }

    fn movie(&self, item: &JellyfinItem) -> Option<usize> {
        Self::unique(item_keys(item).into_iter().filter_map(|key| self.movies.get(&key).copied()))
    }

    fn season(&self, show: &JellyfinItem, season: u32) -> Option<usize> {
        Self::unique(item_keys(show).into_iter().filter_map(|key| self.seasons.get(&(key, season)).copied()))
    }

    fn season_targets(&self, show: &JellyfinItem) -> HashSet<usize> {
        item_keys(show).iter().filter_map(|key| self.shows.get(key)).flatten().copied().collect()
    }
}
