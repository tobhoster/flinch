//! Inflow: storage on its way in that nobody is likely to watch.
//!
//! Evictions free space after it is spent; the cheapest byte is the one never
//! downloaded. Sonarr keeps fetching every new season of a monitored show
//! whether or not anyone opened the last one, and Seerr requests land whether
//! or not they suit the household. Three rules name that inflow:
//!
//! - **Cold and unstarted**: a monitored, continuing series of which no season
//!   was ever started, whose taste ([`crate::taste`]) reads cold.
//! - **Abandoned**: a monitored series somebody started, left partway or
//!   unfinished, and last played more than [`ABANDONED_DAYS`] ago.
//! - **Cold request**: an open Seerr request for a title not yet on disk whose
//!   taste reads cold.
//!
//! Taste is advice here, not a P(watch) input: it is asked with the adopted
//! fit's outcomes when there is one, else with [`household_record`]. Every
//! rule fails closed: a title without a vector, or without any closed
//! outcome to compare, is never called cold. Nothing here writes to Sonarr,
//! Radarr, Seerr or Maintainerr; the suggestions are published for the
//! operator to act on.

use crate::arr::{ArrMovie, ArrSeries};
use crate::card::ArchiveCard;
use crate::embedding::{subject_of, VectorStore};
use crate::signals::{MediaRef, Request};
use crate::taste::{self, Outcomes, Pool, Record};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Taste (log-odds against the household's overall played rate) below which
/// a title reads cold.
pub const COLD_TASTE: f64 = -0.5;
/// Days without a play after which a started, unfinished show is abandoned.
pub const ABANDONED_DAYS: f32 = 180.0;
const GIB: f64 = 1_073_741_824.0;

/// Which rule flagged a title.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    ColdUnstarted,
    Abandoned,
    ColdRequest,
}

/// One title whose incoming storage is likely wasted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Suggestion {
    pub title: String,
    /// `sonarr-<id>` or `radarr-<id>`.
    pub subject: String,
    pub rule: Rule,
    /// One sentence for the operator.
    pub why: String,
    /// Mean on-disk size of the show's seasons; `None` when nothing is on disk.
    pub gib_per_season: Option<f64>,
    pub action: String,
    /// Log-odds taste, when it was asked.
    #[serde(default)]
    pub taste: Option<f64>,
    /// The Seerr requester, for request suggestions.
    #[serde(default)]
    pub requester: Option<String>,
}

/// What the rules read.
pub struct Inputs<'a> {
    pub cards: &'a [ArchiveCard],
    pub series: &'a [ArrSeries],
    pub movies: &'a [ArrMovie],
    pub requests: &'a [Request],
    pub vectors: &'a VectorStore,
    /// The outcomes taste is asked with.
    pub record: &'a Record,
}

/// The household-wide record when no fit is adopted: every title in the
/// library is one outcome, played when any part of it was ever played.
pub fn household_record(cards: &[ArchiveCard], now: u64) -> Record {
    let mut played: BTreeMap<&str, bool> = BTreeMap::new();
    for card in cards {
        *played.entry(subject_of(&card.id)).or_default() |= card.last_watched_days.is_some();
    }
    let mut record = Record { as_of: now, ..Record::default() };
    for (subject, played) in played {
        let one = Outcomes { played: u32::from(played), total: 1 };
        record.household.overall.played += one.played;
        record.household.overall.total += 1;
        record.household.subjects.insert(subject.to_string(), one);
    }
    record
}

/// Every suggestion, cold and abandoned shows first, then requests.
pub fn suggest(inputs: &Inputs<'_>) -> Vec<Suggestion> {
    let card_taste = taste::for_cards(inputs.cards, inputs.vectors, inputs.record);
    let mut by_subject: HashMap<&str, Vec<&ArchiveCard>> = HashMap::new();
    for card in inputs.cards {
        by_subject.entry(subject_of(&card.id)).or_default().push(card);
    }
    let subject_taste =
        |subject: &str| by_subject.get(subject).and_then(|cards| cards.iter().find_map(|card| card_taste.get(&card.id).copied()));
    let mut out = Vec::new();
    for show in inputs.series.iter().filter(|show| show.monitored == Some(true)) {
        let subject = format!("sonarr-{}", show.id);
        let seasons = by_subject.get(subject.as_str()).map(Vec::as_slice).unwrap_or_default();
        let per_season = gib_per_season(seasons);
        let continuing = show.status.as_deref() == Some("continuing");
        let flagged =
            if started(seasons) {
                abandoned(seasons, continuing).map(|days| {
                    (Rule::Abandoned, format!("started, left unfinished and last played {days:.0} days ago, still monitored"), None)
                })
            } else {
                continuing.then(|| subject_taste(&subject)).flatten().filter(|t| *t < COLD_TASTE).map(|t| {
                    (Rule::ColdUnstarted, format!("no season ever started and the household's taste reads cold ({t:+.2})"), Some(t))
                })
            };
        if let Some((rule, why, taste)) = flagged {
            out.push(Suggestion {
                title: show.title.clone(),
                subject,
                rule,
                why,
                gib_per_season: per_season,
                action: "unmonitor future seasons in Sonarr".to_string(),
                taste,
                requester: None,
            });
        }
    }
    let pool = Pool::new(inputs.vectors, inputs.record.household.subjects.keys().map(String::as_str));
    for request in inputs.requests {
        let Some(open) = open_request(inputs, &request.media) else { continue };
        let seasons = by_subject.get(open.subject.as_str()).map(Vec::as_slice).unwrap_or_default();
        if started(seasons) || out.iter().any(|s| s.subject == open.subject) {
            continue;
        }
        let taste = subject_taste(&open.subject)
            .or_else(|| pool.shortlist(inputs.vectors, &open.subject)?.taste(inputs.record))
            .filter(|t| *t < COLD_TASTE);
        let Some(taste) = taste else { continue };
        out.push(Suggestion {
            title: open.title.to_string(),
            subject: open.subject,
            rule: Rule::ColdRequest,
            why: format!("requested by {} and the household's taste reads cold ({taste:+.2})", request.requester),
            gib_per_season: open.series.then(|| gib_per_season(seasons)).flatten(),
            action: open.action.to_string(),
            taste: Some(taste),
            requester: Some(request.requester.clone()),
        });
    }
    out
}

struct Open<'a> {
    subject: String,
    title: &'a str,
    series: bool,
    action: &'static str,
}

/// The library title a request is for, while its download is still to come:
/// a movie without a file, or a monitored show. Requests the *arrs do not know
/// yet have no vector and are skipped.
fn open_request<'a>(inputs: &Inputs<'a>, media: &MediaRef) -> Option<Open<'a>> {
    match *media {
        MediaRef::Movie { tmdb } => {
            let movie = inputs.movies.iter().find(|movie| movie.tmdb_id.map(u64::from) == Some(tmdb) && !movie.has_file)?;
            Some(Open { subject: movie.card_id(), title: &movie.title, series: false, action: "unmonitor the movie in Radarr" })
        }
        MediaRef::Show { tvdb, tmdb } => {
            let same = |id: Option<u32>, want: Option<u64>| want.is_some() && id.filter(|id| *id > 0).map(u64::from) == want;
            let show =
                inputs.series.iter().find(|show| show.monitored == Some(true) && (same(show.tvdb_id, tvdb) || same(show.tmdb_id, tmdb)))?;
            let subject = format!("sonarr-{}", show.id);
            Some(Open { subject, title: &show.title, series: true, action: "unmonitor future seasons in Sonarr" })
        }
    }
}

/// Whether anyone ever played any part of the show.
fn started(seasons: &[&ArchiveCard]) -> bool {
    seasons.iter().any(|card| card.last_watched_days.is_some() || card.episodes_watched.is_some_and(|n| n > 0))
}

/// Days since the last play, when the show was left partway or unfinished
/// (a season not watched through, or more still to air) longer than
/// [`ABANDONED_DAYS`] ago.
fn abandoned(seasons: &[&ArchiveCard], continuing: bool) -> Option<f32> {
    let last = seasons.iter().filter_map(|card| card.last_watched_days).reduce(f32::min)?;
    let unfinished = continuing || seasons.iter().any(|card| card.is_watched != Some(true));
    (last > ABANDONED_DAYS && unfinished).then_some(last)
}

fn gib_per_season(seasons: &[&ArchiveCard]) -> Option<f64> {
    let sizes: Vec<u64> = seasons.iter().map(|card| card.size_bytes).filter(|bytes| *bytes > 0).collect();
    (!sizes.is_empty()).then(|| sizes.iter().map(|bytes| *bytes as f64).sum::<f64>() / sizes.len() as f64 / GIB)
}

#[cfg(test)]
mod tests;
