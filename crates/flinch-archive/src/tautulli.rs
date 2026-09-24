//! Watch evidence from Tautulli.
//!
//! Why a second media-server source exists at all: Plex's own watch fields are
//! scoped to the **authenticated account**, and its playback log is trimmed.
//! Tautulli keeps its own database of every stream, with user attribution and
//! `percent_complete`.
//!
//! Streams join library items by Plex ratingKey through the item's
//! [`Resolution`] — or, for media Plex re-added since, by its `plex://` GUID —
//! never by title: a title join credited a 1978 stream to the 2025 remake. Any
//! stream, finished or not, proves the household touched the
//! item. Silence is claimed as "never streamed" only under the conditions in
//! [`absence_by_target`], because it is the one claim that can delete something.

use crate::card::LibraryKind;
use crate::plex::{Resolution, RowKey, WatchTarget};
use crate::watch::{EvidenceHealth, WatchEntry, WatchSource};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// One stream from Tautulli's history.
///
/// Tautulli answers with strings for everything, including numbers, so the parse
/// is lenient in both directions rather than assuming a JSON shape it does not
/// guarantee across versions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TautulliRow {
    #[serde(default, deserialize_with = "lenient_string")]
    pub media_type: String,
    #[serde(default, deserialize_with = "lenient_string")]
    pub title: String,
    #[serde(default, deserialize_with = "lenient_string")]
    pub grandparent_title: String,
    #[serde(default, deserialize_with = "lenient_string")]
    pub parent_media_index: String,
    #[serde(default, deserialize_with = "lenient_string")]
    pub media_index: String,
    /// The Plex ratingKey of what was streamed (the movie, or the episode).
    #[serde(default, deserialize_with = "lenient_string")]
    pub rating_key: String,
    /// The season's ratingKey on an episode stream.
    #[serde(default, deserialize_with = "lenient_string")]
    pub parent_rating_key: String,
    /// The show's ratingKey on an episode stream.
    #[serde(default, deserialize_with = "lenient_string")]
    pub grandparent_rating_key: String,
    /// Release year of what was streamed (the movie's, for a movie).
    #[serde(default, deserialize_with = "lenient_string")]
    pub year: String,
    /// Plex's GUID of what was streamed, as it was then: `plex://movie/…` or
    /// `plex://episode/…` under Plex's current agents, an agent GUID under
    /// legacy ones. Only the former outlives a library re-add.
    #[serde(default, deserialize_with = "lenient_string")]
    pub guid: String,
    /// Epoch seconds of the stream.
    #[serde(default, deserialize_with = "lenient_string")]
    pub date: String,
    /// 0-100.
    #[serde(default, deserialize_with = "lenient_string")]
    pub percent_complete: String,
    /// Tautulli's id for the account that streamed it (the Plex user id).
    #[serde(default, deserialize_with = "lenient_string")]
    pub user_id: String,
    /// Account name, the fallback viewer identity when `user_id` is missing.
    #[serde(default, deserialize_with = "lenient_string")]
    pub user: String,
}

fn lenient_string<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::String(text) => text,
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    })
}

impl TautulliRow {
    pub fn epoch(&self) -> Option<u64> {
        self.date.parse().ok().filter(|epoch| *epoch > 0)
    }

    pub fn season(&self) -> Option<u32> {
        self.parent_media_index.parse().ok()
    }

    /// Whether this stream counts as a finished watch (most of the runtime).
    /// Anything less is still a play — the household touched it — but it does
    /// not complete an episode or a movie.
    pub fn is_watch(&self) -> bool {
        self.percent_complete.parse::<f32>().map(|percent| percent >= 85.0).unwrap_or(true)
    }

    /// Share of the runtime streamed, 0.0-1.0; unknown counts as finished.
    pub fn fraction(&self) -> f32 {
        self.percent_complete.parse::<f32>().map(|percent| (percent / 100.0).clamp(0.0, 1.0)).unwrap_or(1.0)
    }

    /// Who streamed it, when Tautulli says.
    pub fn viewer(&self) -> Option<&str> {
        [self.user_id.as_str(), self.user.as_str()].into_iter().find(|id| !id.is_empty())
    }

    /// The row's identifying fields for play joins.
    pub fn key(&self) -> Option<RowKey> {
        RowKey::new(
            &self.media_type,
            [Some(self.rating_key.as_str()), Some(self.parent_rating_key.as_str()), Some(self.grandparent_rating_key.as_str())],
            Some(self.guid.as_str()),
            self.season(),
            &self.title,
            self.year.parse().ok(),
        )
    }
}

/// One page of a `get_history` response.
#[derive(Debug, Clone, Default)]
pub struct TautulliPage {
    pub rows: Vec<TautulliRow>,
    /// Rows the query matches in total; paging is complete once this many are
    /// read. Absent in the old unpaged response shape.
    pub records_filtered: Option<u64>,
}

/// Parse a Tautulli `get_history` response body; `None` when it is not one.
pub fn parse_history_page(body: &str) -> Option<TautulliPage> {
    #[derive(Deserialize)]
    struct Envelope {
        response: Response,
    }
    #[derive(Deserialize)]
    struct Response {
        data: Rows,
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Rows {
        /// Newer Tautulli: `data: { data: [...], recordsFiltered: N }`.
        Paged {
            data: Vec<TautulliRow>,
            #[serde(default, rename = "recordsFiltered")]
            records_filtered: Option<u64>,
        },
        /// Older shape: `data: [...]`.
        Plain(Vec<TautulliRow>),
    }
    let envelope: Envelope = serde_json::from_str(body).ok()?;
    Some(match envelope.response.data {
        Rows::Paged { data, records_filtered } => TautulliPage { rows: data, records_filtered },
        Rows::Plain(rows) => TautulliPage { rows, records_filtered: None },
    })
}

/// Tautulli's `keep_history` switches. A stream by a user, or in a library,
/// whose switch is off leaves no row, so Tautulli's silence about anything
/// they could have streamed proves nothing (TT-01).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeepHistory {
    /// Active users whose streams are not kept, by name, for the log.
    pub users_off: Vec<String>,
    /// Library sections whose streams are kept.
    pub sections_on: HashSet<u32>,
}

impl KeepHistory {
    /// Read `get_users` and `get_libraries_table` answers; `None` when either
    /// is not one, or the library table was cut short. A missing switch reads
    /// as off, and a user not marked inactive counts.
    pub fn parse(users: &str, libraries: &str) -> Option<Self> {
        #[derive(Deserialize)]
        struct Envelope<T> {
            response: Data<T>,
        }
        #[derive(Deserialize)]
        struct Data<T> {
            data: T,
        }
        #[derive(Deserialize)]
        struct Table {
            data: Vec<Switch>,
            #[serde(default, rename = "recordsFiltered")]
            records_filtered: Option<u64>,
        }
        /// A user or library row: only its switch and its name matter.
        #[derive(Deserialize)]
        struct Switch {
            #[serde(default, deserialize_with = "lenient_string")]
            keep_history: String,
            #[serde(default, deserialize_with = "lenient_string")]
            is_active: String,
            #[serde(default, deserialize_with = "lenient_string")]
            section_id: String,
            #[serde(default, deserialize_with = "lenient_string")]
            friendly_name: String,
            #[serde(default, deserialize_with = "lenient_string")]
            username: String,
        }
        let kept = |row: &Switch| matches!(row.keep_history.as_str(), "1" | "true");
        let active = |row: &Switch| !matches!(row.is_active.as_str(), "0" | "false");
        let name = |row: &Switch| {
            [&row.friendly_name, &row.username].into_iter().find(|name| !name.is_empty()).map_or("(unnamed)", String::as_str).to_string()
        };
        let users: Envelope<Vec<Switch>> = serde_json::from_str(users).ok()?;
        let libraries: Envelope<Table> = serde_json::from_str(libraries).ok()?;
        let table = libraries.response.data;
        if table.records_filtered.is_some_and(|total| total > table.data.len() as u64) {
            return None;
        }
        Some(Self {
            users_off: users.response.data.iter().filter(|user| active(user) && !kept(user)).map(name).collect(),
            sections_on: table.data.iter().filter(|library| kept(library)).filter_map(|library| library.section_id.parse().ok()).collect(),
        })
    }

    /// Whether every stream that could touch these sections is kept: every
    /// active user's, in each of them.
    pub fn covers(&self, sections: impl IntoIterator<Item = u32>) -> bool {
        self.users_off.is_empty() && sections.into_iter().all(|section| self.sections_on.contains(&section))
    }
}

/// A stretch this long with no stream at all is read as Tautulli not recording
/// (down, or history purged), not as a household on holiday: silence across it
/// proves nothing.
pub const MAX_SILENCE_SECS: u64 = 60 * 86_400;

/// The stretch of time Tautulli's record demonstrably covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coverage {
    /// Oldest stream of the latest unbroken run.
    pub start: u64,
    /// Newest stream.
    pub end: u64,
}

impl Coverage {
    /// Whether Tautulli is still recording at `now`.
    pub fn recording_at(&self, now: u64) -> bool {
        now.saturating_sub(self.end) <= MAX_SILENCE_SECS
    }
}

/// The latest unbroken run of streams: walking back from the newest, coverage
/// ends at the first silence longer than [`MAX_SILENCE_SECS`]. Absence before a
/// gap is not evidence, so a gap moves the start of coverage past it.
pub fn coverage(rows: &[TautulliRow]) -> Option<Coverage> {
    let mut epochs: Vec<u64> = rows.iter().filter_map(TautulliRow::epoch).collect();
    epochs.sort_unstable();
    let end = *epochs.last()?;
    let mut start = end;
    for epoch in epochs.iter().rev().skip(1) {
        if start - epoch > MAX_SILENCE_SECS {
            break;
        }
        start = *epoch;
    }
    Some(Coverage { start, end })
}

/// How long an item must have been on offer before "nobody streamed it" means
/// anything.
pub const ABSENCE_SETTLE_SECS: u64 = 30 * 86_400;

/// Whether Tautulli's silence about an item is evidence as of `as_of`: it
/// arrived after Tautulli's record began, and has been on offer long enough to
/// have been watched. The fitter asks this at every cut date, so the panel
/// grants absence evidence exactly where the daemon would have.
pub fn absence_is_evidence(added_epoch: u64, coverage_start: u64, as_of: u64) -> bool {
    added_epoch >= coverage_start && as_of.saturating_sub(added_epoch) >= ABSENCE_SETTLE_SECS
}

/// Absence evidence: resolved items Tautulli watched arrive and never saw
/// streamed at all.
///
/// Claimed only when every condition holds, because a wrong claim deletes
/// something the household wanted:
/// - this cycle's evidence allows it ([`EvidenceHealth::absence_is_claimable`]:
///   Plex was read, Tautulli was read in full);
/// - Tautulli is still recording at `now`;
/// - the target resolved to a Plex ratingKey by GUID — an unmatched title is
///   not an unwatched one;
/// - no stream of *any* completeness exists for it;
/// - it has a known arrival date, after coverage began and long enough ago.
pub fn absence_by_target(
    targets: &[WatchTarget],
    resolution: &Resolution,
    rows: &[TautulliRow],
    health: &EvidenceHealth,
    now: u64,
) -> HashMap<String, WatchEntry> {
    let mut out = HashMap::new();
    let Some(coverage) = coverage(rows).filter(|coverage| coverage.recording_at(now)) else { return out };
    if !health.absence_is_claimable() {
        return out;
    }
    let keys: Vec<RowKey> = rows.iter().filter_map(TautulliRow::key).collect();
    for target in targets.iter().filter(|target| resolution.is_guid_resolved(&target.id)) {
        let Some(added) = target.added_epoch else { continue };
        if !absence_is_evidence(added, coverage.start, now) {
            continue;
        }
        let join = resolution.join(target);
        if keys.iter().any(|key| join.matches(key)) {
            continue;
        }
        let entry = WatchEntry {
            id: target.id.clone(),
            last_watched_epoch: None,
            progress: 0.0,
            rewatch_score: None,
            source: WatchSource::TautulliAbsence,
        };
        out.insert(target.id.clone(), entry);
    }
    out
}

/// Watch entries from Tautulli's streams, joined through the resolution.
///
/// Every stream counts toward recency. Progress counts finished episodes (or a
/// finished movie) fully and a stream that stopped early as half an episode, so
/// an item only ever streamed partially reads as partially played — touched,
/// never "never played" and never "complete".
pub fn plays_by_target(targets: &[WatchTarget], resolution: &Resolution, rows: &[TautulliRow]) -> HashMap<String, WatchEntry> {
    let keyed: Vec<(RowKey, &TautulliRow)> =
        rows.iter().filter(|row| row.epoch().is_some()).filter_map(|row| Some((row.key()?, row))).collect();
    let mut out = HashMap::new();
    for target in targets {
        let join = resolution.join(target);
        let streams: Vec<&TautulliRow> = keyed.iter().filter(|(key, _)| join.matches(key)).map(|(_, row)| *row).collect();
        if streams.is_empty() {
            continue;
        }
        let progress = match target.kind {
            LibraryKind::Movie => {
                if streams.iter().any(|row| row.is_watch()) {
                    1.0
                } else {
                    streams.iter().map(|row| row.fraction()).fold(0.01f32, f32::max).min(0.99)
                }
            }
            LibraryKind::Season => season_progress(&streams, target.episode_files.or(target.episodes_total)),
        };
        let entry = WatchEntry {
            id: target.id.clone(),
            last_watched_epoch: streams.iter().filter_map(|row| row.epoch()).max(),
            progress,
            rewatch_score: None,
            source: WatchSource::Tautulli,
        };
        out.insert(target.id.clone(), entry);
    }
    out
}

fn season_progress(streams: &[&TautulliRow], total: Option<u32>) -> f32 {
    let Some(total) = total.filter(|total| *total > 0) else { return 0.5 };
    let finished: HashSet<&str> = streams.iter().filter(|row| row.is_watch()).map(|row| row.media_index.as_str()).collect();
    let started: HashSet<&str> = streams.iter().map(|row| row.media_index.as_str()).collect();
    let partial_only = started.difference(&finished).count() as f32;
    let progress = (finished.len() as f32 + 0.5 * partial_only) / total as f32;
    if finished.len() as u32 >= total {
        1.0
    } else {
        progress.clamp(0.01, 0.99)
    }
}

#[cfg(test)]
mod tests;
