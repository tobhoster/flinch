//! How hard a title would be to download again: Prowlarr's search results
//! for it, judged against the SABnzbd servers' retention, and cached per card
//! so the indexers see a bounded trickle of searches.
//!
//! Wire shape: Prowlarr `GET /api/v1/search?query=&type=&limit=` answers a
//! `ReleaseResource` array whose `protocol` is `torrent`, `usenet` or
//! `unknown`, `seeders` is null off torrents, and `age` is whole days since
//! posting. SABnzbd `GET /api/?mode=get_config&section=servers&output=json`
//! answers `{config: {servers: [{enable, retention, ...}]}}`, retention in
//! days with 0 meaning unlimited; older releases write numbers as strings.
//! Each release also carries its `title` and `size` in bytes (Prowlarr
//! `ReleaseResource.cs`, https://github.com/Prowlarr/Prowlarr/blob/develop/src/Prowlarr.Api.V1/Search/ReleaseResource.cs):
//! the smallest one is what [`crate::quality::act`] asks before a downgrade.

use super::{rows, Release};
use crate::card::{ArchiveCard, LibraryKind};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// How long one search serves a card.
pub const RELEASE_TTL_SECS: u64 = 7 * 86_400;
/// Searches per cycle at most.
pub const SEARCH_BUDGET: usize = 20;
/// Releases smaller than this are samples or fakes, never a compact copy.
pub const MIN_RELEASE_BYTES: u64 = 256 << 20;

/// Prowlarr's query and search type for a card: `"{title} {year}"` as a
/// movie search, `"{show} S{nn}"` as a TV search. `None` for a season card
/// without its show title or season.
pub fn search_query(card: &ArchiveCard) -> Option<(String, &'static str)> {
    match card.kind {
        LibraryKind::Movie => {
            let query = card.movie_year.map_or_else(|| card.title.clone(), |year| format!("{} {year}", card.title));
            Some((query, "movie"))
        }
        LibraryKind::Season => Some((format!("{} S{:02}", card.show_title.as_deref()?, card.season_index?), "tvsearch")),
    }
}

#[derive(Deserialize)]
struct SearchResult {
    protocol: Protocol,
    #[serde(default)]
    seeders: Option<u32>,
    #[serde(default)]
    age: u32,
    #[serde(default)]
    title: String,
    #[serde(default)]
    size: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Protocol {
    Torrent,
    Usenet,
    #[serde(other)]
    Unknown,
}

/// What one search found, kept so the verdict follows the current retention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Searched {
    /// When the search ran, unix seconds.
    pub searched_at: u64,
    /// The best-seeded torrent's seeders; 0 when no torrent was found.
    pub seeders: u32,
    /// The newest usenet post's age in days; `None` when none was found.
    pub youngest_usenet_days: Option<u32>,
    /// The smallest whole release found, in bytes: for a season only season
    /// packs count. `None` when none was found, and in a search cached before
    /// sizes were kept.
    #[serde(default)]
    pub smallest_bytes: Option<u64>,
    /// The largest whole release found, in bytes, under the same rules: what
    /// an upgrade search may bring in. `None` as for `smallest_bytes`.
    #[serde(default)]
    pub largest_bytes: Option<u64>,
}

impl Searched {
    /// Reduce one search's results. A malformed result is skipped. `season`
    /// names the season a TV search was for: an episode is not the season.
    pub fn from_results(results: Vec<serde_json::Value>, searched_at: u64, season: Option<u32>) -> Self {
        let mut found = Searched { searched_at, seeders: 0, youngest_usenet_days: None, smallest_bytes: None, largest_bytes: None };
        for result in rows::<SearchResult>(results) {
            let whole = season.is_none_or(|season| season_pack(&result.title, season));
            if whole && result.size >= MIN_RELEASE_BYTES {
                found.smallest_bytes = Some(found.smallest_bytes.map_or(result.size, |bytes| bytes.min(result.size)));
                found.largest_bytes = Some(found.largest_bytes.map_or(result.size, |bytes| bytes.max(result.size)));
            }
            match result.protocol {
                Protocol::Torrent => found.seeders = found.seeders.max(result.seeders.unwrap_or(0)),
                Protocol::Usenet => {
                    found.youngest_usenet_days = Some(found.youngest_usenet_days.map_or(result.age, |age| age.min(result.age)))
                }
                Protocol::Unknown => {}
            }
        }
        found
    }

    /// The verdict under `retention_days` (0 = unlimited; `None` = unknown,
    /// which leaves usenet unjudged).
    pub fn release(&self, retention_days: Option<u32>) -> Release {
        let out_of_retention = |retention: u32| match self.youngest_usenet_days {
            None => true,
            Some(age) => retention != 0 && age > retention,
        };
        Release { seeders: Some(self.seeders), usenet_out_of_retention: retention_days.map(out_of_retention) }
    }
}

/// Whether a release title is a pack of `season`: it names `S01` (scene
/// style) as a token not followed by an episode (`S01E01`).
pub fn season_pack(title: &str, season: u32) -> bool {
    let title = title.to_ascii_uppercase();
    let token = format!("S{season:02}");
    let bytes = title.as_bytes();
    title.match_indices(&token).any(|(at, _)| {
        let starts = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
        let next = bytes.get(at + token.len());
        starts && !next.is_some_and(|byte| byte.is_ascii_digit() || *byte == b'E')
    })
}

/// The longest retention among the enabled usenet servers in a SABnzbd
/// `get_config` answer: 0 (unlimited) when any enabled server has 0. `None`
/// when no enabled server states one.
pub fn parse_retention(config: &serde_json::Value) -> Option<u32> {
    let servers = config.pointer("/config/servers")?.as_array()?;
    let retentions = servers.iter().filter(|server| number(server.get("enable")).is_some_and(|enable| enable != 0));
    let retentions: Vec<u64> = retentions.filter_map(|server| number(server.get("retention"))).collect();
    let unlimited = retentions.contains(&0);
    let longest = if unlimited { Some(0) } else { retentions.into_iter().max() };
    longest.map(|days| u32::try_from(days).unwrap_or(u32::MAX))
}

/// A SABnzbd config number, written as a number, a string or a boolean.
fn number(value: Option<&serde_json::Value>) -> Option<u64> {
    match value? {
        serde_json::Value::Number(number) => number.as_u64(),
        serde_json::Value::String(text) => text.trim().parse().ok(),
        serde_json::Value::Bool(flag) => Some(u64::from(*flag)),
        _ => None,
    }
}

/// The release cache (`releases.json`): card id → its last search.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReleaseCache(BTreeMap<String, Searched>);

impl ReleaseCache {
    /// The cards to search this cycle, at most [`SEARCH_BUDGET`]: those never
    /// searched first, in card order, then those whose search is older than
    /// [`RELEASE_TTL_SECS`], oldest first. A search dated in the future (a
    /// clock step back) is due. Cards without a query are never due.
    pub fn due<'a>(&self, cards: &'a [ArchiveCard], now: u64) -> Vec<&'a ArchiveCard> {
        let mut due: Vec<(Option<u64>, &ArchiveCard)> = cards
            .iter()
            .filter(|card| search_query(card).is_some())
            .filter_map(|card| match self.0.get(&card.id) {
                None => Some((None, card)),
                Some(searched) if searched.searched_at > now || now - searched.searched_at >= RELEASE_TTL_SECS => {
                    Some((Some(searched.searched_at), card))
                }
                Some(_) => None,
            })
            .collect();
        // Stable: never-searched cards keep their order ahead of every stale one.
        due.sort_by_key(|(searched_at, _)| *searched_at);
        due.into_iter().take(SEARCH_BUDGET).map(|(_, card)| card).collect()
    }

    pub fn insert(&mut self, card_id: String, searched: Searched) {
        self.0.insert(card_id, searched);
    }

    /// Forget the cards no longer in the library.
    pub fn retain_cards(&mut self, cards: &[ArchiveCard]) {
        let ids: std::collections::HashSet<&str> = cards.iter().map(|card| card.id.as_str()).collect();
        self.0.retain(|id, _| ids.contains(id.as_str()));
    }

    /// Every cached card's verdict under `retention_days`, stale or not: an
    /// old search beats none while the budget catches up.
    pub fn releases(&self, retention_days: Option<u32>) -> HashMap<String, Release> {
        self.0.iter().map(|(id, searched)| (id.clone(), searched.release(retention_days))).collect()
    }

    /// Every cached card's smallest whole release, stale or not.
    pub fn smallest(&self) -> HashMap<String, u64> {
        self.0.iter().filter_map(|(id, searched)| Some((id.clone(), searched.smallest_bytes?))).collect()
    }

    /// Every cached card's largest whole release, stale or not.
    pub fn largest(&self) -> HashMap<String, u64> {
        self.0.iter().filter_map(|(id, searched)| Some((id.clone(), searched.largest_bytes?))).collect()
    }
}
