//! Streaming availability: whether a title streams, in the operator's region,
//! on a service the household pays for. A title that does is cheap to lose —
//! it can be watched without downloading it again — so regret lowers its
//! re-acquire cost ([`crate::regret::Reacquisition`]), and inflow advice names
//! a request for it.
//!
//! The data is TMDB's watch providers (`GET /3/movie/{id}/watch/providers`,
//! `GET /3/tv/{id}/watch/providers`;
//! <https://developer.themoviedb.org/reference/movie-watch-providers>), which
//! TMDB sources from JustWatch: every place that shows a provider credits
//! JustWatch, as TMDB's terms require. Only `flatrate` counts — `rent`, `buy`
//! and `ads` cost per title or are not the household's subscription.
//!
//! Lookups trickle like the Prowlarr searches ([`super::release`]): at most
//! [`LOOKUP_BUDGET`] a cycle, each kept [`STREAMING_TTL_SECS`]. A title never
//! looked up, looked up for another region, or whose answer was unreadable
//! has no entry and gets no discount.

pub mod tmdb;

use super::MediaRef;
use crate::arr::{ArrMovie, ArrSeries};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

/// How long one lookup stands: catalogues change monthly, not hourly.
pub const STREAMING_TTL_SECS: u64 = 7 * 86_400;
/// Lookups per cycle; the rest wait for later cycles.
pub const LOOKUP_BUDGET: usize = 40;
/// Subscribed providers the settings accept.
pub const MAX_PROVIDERS: usize = 64;

/// `settings.json` `streaming`. Every field defaults individually; off by default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamingConfig {
    /// Look titles up and discount those on a subscribed service.
    pub enabled: bool,
    /// ISO 3166-1 country code TMDB keys its answer by, e.g. `DE`.
    pub region: String,
    /// TMDB provider ids the household subscribes to (8 = Netflix, 337 = Disney Plus).
    pub provider_ids: Vec<u32>,
    /// Environment variable holding the TMDB API key (v3 key or v4 read token).
    pub tmdb_key_env: String,
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self { enabled: false, region: String::new(), provider_ids: Vec::new(), tmdb_key_env: "TMDB_API_KEY".to_owned() }
    }
}

/// A [`StreamingConfig`] outside its bounds; the message names the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidStreamingConfig(pub &'static str);

impl StreamingConfig {
    pub fn validate(&self) -> Result<(), InvalidStreamingConfig> {
        let env_ok = |name: &str| name.bytes().all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_');
        if !env_ok(&self.tmdb_key_env) {
            return Err(InvalidStreamingConfig("the TMDB key variable (streaming.tmdb_key_env) takes A-Z, 0-9 and _ only"));
        }
        if self.provider_ids.len() > MAX_PROVIDERS || self.provider_ids.contains(&0) {
            return Err(InvalidStreamingConfig("subscribed providers (streaming.provider_ids) must be 1 to 64 TMDB provider ids"));
        }
        if !self.enabled {
            return Ok(());
        }
        if self.region.len() != 2 || !self.region.bytes().all(|byte| byte.is_ascii_uppercase()) {
            return Err(InvalidStreamingConfig("the region (streaming.region) must be a two-letter country code such as DE"));
        }
        if self.provider_ids.is_empty() {
            return Err(InvalidStreamingConfig("subscribed providers (streaming.provider_ids) must name at least one provider"));
        }
        if self.tmdb_key_env.is_empty() {
            return Err(InvalidStreamingConfig("the TMDB key variable (streaming.tmdb_key_env) must be set"));
        }
        Ok(())
    }
}

/// A TMDB title, as the watch-provider endpoints name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Title {
    Movie(u64),
    Tv(u64),
}

impl Title {
    /// The endpoint path under `https://api.themoviedb.org`.
    pub fn path(self) -> String {
        match self {
            Self::Movie(id) => format!("/3/movie/{id}/watch/providers"),
            Self::Tv(id) => format!("/3/tv/{id}/watch/providers"),
        }
    }

    /// The cache key, `movie/<id>` or `tv/<id>`.
    fn key(self) -> String {
        match self {
            Self::Movie(id) => format!("movie/{id}"),
            Self::Tv(id) => format!("tv/{id}"),
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        let (kind, id) = key.split_once('/')?;
        let id = id.parse().ok().filter(|id| *id > 0)?;
        match kind {
            "movie" => Some(Self::Movie(id)),
            "tv" => Some(Self::Tv(id)),
            _ => None,
        }
    }

    /// A Seerr media item's TMDB title; a show without a TMDB id has none.
    pub fn of_media(media: &MediaRef) -> Option<Self> {
        match *media {
            MediaRef::Movie { tmdb } => (tmdb > 0).then_some(Self::Movie(tmdb)),
            MediaRef::Show { tmdb, .. } => tmdb.filter(|id| *id > 0).map(Self::Tv),
        }
    }
}

/// One flatrate provider, in TMDB's display order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provider {
    pub id: u32,
    pub name: String,
}

/// Card id → its TMDB title: a movie's card, and every season card of a show
/// (streaming services carry whole shows). Items without a TMDB id are absent.
pub fn card_titles(movies: &[ArrMovie], series: &[ArrSeries]) -> HashMap<String, Title> {
    let movies = movies.iter().filter_map(|movie| Some((movie.card_id(), Title::Movie(u64::from(movie.tmdb_id.filter(|id| *id > 0)?)))));
    let seasons = series
        .iter()
        .filter_map(|show| Some((show, Title::Tv(u64::from(show.tmdb_id.filter(|id| *id > 0)?)))))
        .flat_map(|(show, title)| show.seasons.iter().map(move |season| (show.season_card_id(season.season_number), title)));
    movies.chain(seasons).collect()
}

/// A title streams on a subscribed service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    pub provider: String,
    pub region: String,
}

impl Stream {
    /// "streams on Netflix (DE)".
    pub fn note(&self) -> String {
        format!("streams on {} ({})", self.provider, self.region)
    }
}

/// The flatrate providers of `region` in a watch-providers answer
/// (`{"id": .., "results": {"DE": {"flatrate": [{"provider_id", "provider_name", ..}]}}}`).
/// A region missing from `results` streams nowhere; an answer without
/// `results` is unreadable (`None`). Malformed provider rows are skipped.
pub fn parse_flatrate(answer: &serde_json::Value, region: &str) -> Option<Vec<Provider>> {
    let results = answer.get("results")?.as_object()?;
    let rows = results.get(region).and_then(|country| country.get("flatrate")).and_then(serde_json::Value::as_array);
    let provider = |row: &serde_json::Value| {
        let id = u32::try_from(row.get("provider_id")?.as_u64()?).ok()?;
        let name = row.get("provider_name")?.as_str()?.trim();
        (!name.is_empty()).then(|| Provider { id, name: name.to_owned() })
    };
    Some(rows.map(|rows| rows.iter().filter_map(provider).collect()).unwrap_or_default())
}

/// One lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Looked {
    pub looked_at: u64,
    pub region: String,
    pub flatrate: Vec<Provider>,
}

/// The lookup cache (`streaming.json`): `movie/<id>` or `tv/<id>` → its last lookup.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StreamingCache(BTreeMap<String, Looked>);

impl StreamingCache {
    /// The titles to look up this cycle, at most [`LOOKUP_BUDGET`]: those never
    /// looked up for `region` first, in the given order, then those older than
    /// [`STREAMING_TTL_SECS`], oldest first. A lookup dated in the future is due.
    pub fn due(&self, titles: &[Title], region: &str, now: u64) -> Vec<Title> {
        let mut seen = HashSet::new();
        let mut due: Vec<(Option<u64>, Title)> = titles
            .iter()
            .filter(|title| seen.insert(**title))
            .filter_map(|title| match self.0.get(&title.key()).filter(|looked| looked.region == region) {
                None => Some((None, *title)),
                Some(looked) if looked.looked_at > now || now - looked.looked_at >= STREAMING_TTL_SECS => {
                    Some((Some(looked.looked_at), *title))
                }
                Some(_) => None,
            })
            .collect();
        due.sort_by_key(|(looked_at, _)| *looked_at);
        due.into_iter().take(LOOKUP_BUDGET).map(|(_, title)| title).collect()
    }

    pub fn insert(&mut self, title: Title, looked: Looked) {
        self.0.insert(title.key(), looked);
    }

    /// Forget titles no longer in the library or requested.
    pub fn retain(&mut self, titles: &[Title]) {
        let keep: HashSet<String> = titles.iter().map(|title| title.key()).collect();
        self.0.retain(|key, _| keep.contains(key));
    }

    /// Every title looked up for `region` that streams on one of `subscribed`,
    /// stale or not (an old lookup beats none while the budget catches up),
    /// naming the first such provider in TMDB's order.
    pub fn streams(&self, region: &str, subscribed: &[u32]) -> HashMap<Title, Stream> {
        self.0
            .iter()
            .filter(|(_, looked)| looked.region == region)
            .filter_map(|(key, looked)| {
                let provider = looked.flatrate.iter().find(|provider| subscribed.contains(&provider.id))?;
                Some((Title::from_key(key)?, Stream { provider: provider.name.clone(), region: region.to_owned() }))
            })
            .collect()
    }

    /// How many titles have a lookup for `region`.
    pub fn known(&self, region: &str) -> usize {
        self.0.values().filter(|looked| looked.region == region).count()
    }
}

/// What the streaming lookups know, for the status page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamingStatus {
    pub region: String,
    /// Titles with a lookup for the region.
    pub known: usize,
    /// Of those, titles on a subscribed service.
    pub streaming: usize,
    /// Lookups made this cycle.
    pub looked_up: usize,
}

#[cfg(test)]
mod tests;
