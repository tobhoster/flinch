//! Prowlarr's indexers and which of them are failing.
//!
//! Wire shape (Prowlarr v1/v2): `GET /api/v1/indexer` lists every
//! `IndexerResource` (`id`, `name`, `enable`, `protocol`, and the settings a
//! test sends back); `GET /api/v1/indexerstatus` lists one
//! `IndexerStatusResource` per indexer that ever failed (`indexerId`,
//! `disabledTill`, `mostRecentFailure`, `initialFailure`); `GET
//! /api/v1/indexerstats` answers `{ "indexers": [...] }` with
//! `numberOfQueries`, `numberOfGrabs`, `numberOfFailedQueries` and
//! `averageResponseTime` (ms) per indexer.
//!
//! A failure sets `disabledTill` (the back-off); a success after it clears
//! it again, while `mostRecentFailure` stays. So a set `disabledTill` means
//! the indexer's last outcome was a failure, whether or not the back-off has
//! run out.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A failure this recent counts as failing even without a back-off.
pub const RECENT_FAILURE_SECS: u64 = 6 * 3_600;

/// One indexer as published.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexerStatus {
    pub id: u64,
    pub name: String,
    pub enabled: bool,
    /// `torrent` or `usenet`.
    pub protocol: String,
    pub failing: bool,
    #[serde(default)]
    pub disabled_till: Option<String>,
    #[serde(default)]
    pub most_recent_failure: Option<String>,
    /// When the current run of failures began.
    #[serde(default)]
    pub initial_failure: Option<String>,
    pub queries: u64,
    pub grabs: u64,
    pub failed_queries: u64,
    /// Queries that did not fail, of all; `None` before the first query.
    #[serde(default)]
    pub success_rate: Option<f64>,
    pub avg_response_ms: u64,
}

impl IndexerStatus {
    /// How long it has been failing, at `now`; `None` while it is not.
    pub fn failing_for(&self, now: u64) -> Option<u64> {
        let since = self.initial_failure.as_deref().and_then(crate::presence::parse_utc)?;
        self.failing.then(|| now.saturating_sub(since))
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Indexer {
    id: u64,
    name: Option<String>,
    enable: bool,
    protocol: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Status {
    indexer_id: u64,
    disabled_till: Option<String>,
    most_recent_failure: Option<String>,
    initial_failure: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Stats {
    indexer_id: u64,
    number_of_queries: u64,
    number_of_grabs: u64,
    number_of_failed_queries: u64,
    average_response_time: u64,
}

/// Join the three reads, in the order Prowlarr lists its indexers. A status
/// or stats row of an indexer no longer listed is dropped; a malformed row
/// is skipped.
pub fn join(indexers: Vec<serde_json::Value>, statuses: Vec<serde_json::Value>, stats: serde_json::Value, now: u64) -> Vec<IndexerStatus> {
    let statuses: HashMap<u64, Status> =
        crate::arr::parse_rows::<Status>(statuses).parsed.into_iter().map(|status| (status.indexer_id, status)).collect();
    let stats_rows = match stats {
        serde_json::Value::Object(mut answer) => match answer.remove("indexers") {
            Some(serde_json::Value::Array(rows)) => rows,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let stats: HashMap<u64, Stats> =
        crate::arr::parse_rows::<Stats>(stats_rows).parsed.into_iter().map(|row| (row.indexer_id, row)).collect();
    crate::arr::parse_rows::<Indexer>(indexers)
        .parsed
        .into_iter()
        .map(|indexer| {
            let status = statuses.get(&indexer.id);
            let row = stats.get(&indexer.id);
            let (queries, failed_queries) = row.map_or((0, 0), |row| (row.number_of_queries, row.number_of_failed_queries));
            IndexerStatus {
                id: indexer.id,
                name: indexer.name.unwrap_or_default(),
                enabled: indexer.enable,
                protocol: indexer.protocol.unwrap_or_default(),
                failing: status.is_some_and(|status| failing(status, now)),
                disabled_till: status.and_then(|status| status.disabled_till.clone()),
                most_recent_failure: status.and_then(|status| status.most_recent_failure.clone()),
                initial_failure: status.and_then(|status| status.initial_failure.clone()),
                queries,
                grabs: row.map_or(0, |row| row.number_of_grabs),
                failed_queries,
                success_rate: (queries > 0).then(|| queries.saturating_sub(failed_queries) as f64 / queries as f64),
                avg_response_ms: row.map_or(0, |row| row.average_response_time),
            }
        })
        .collect()
}

/// The last outcome was a failure (a back-off is set), or one is recent.
fn failing(status: &Status, now: u64) -> bool {
    let at = |text: &Option<String>| text.as_deref().and_then(crate::presence::parse_utc);
    at(&status.disabled_till).is_some()
        || at(&status.most_recent_failure).is_some_and(|failed| now.saturating_sub(failed) < RECENT_FAILURE_SECS)
}
