//! Sonarr writes for the native executor: delete one season (its episode
//! files, the season unmonitored), verified by reading it back, and undo.
//!
//! Endpoints from Sonarr's API v3 (https://sonarr.tv/docs/api/, the OpenAPI
//! document at
//! https://github.com/Sonarr/Sonarr/blob/v5-develop/src/Sonarr.Api.V3/openapi.json):
//! `GET/PUT /api/v3/series/{id}` (the whole series resource, its
//! `seasons[].monitored`), `GET /api/v3/episodefile?seriesId=`,
//! `DELETE /api/v3/episodefile/bulk` (`episodeFileIds`), and
//! `POST /api/v3/command` with `SeasonSearch` (`seriesId`, `seasonNumber`, see
//! src/NzbDrone.Core/IndexerSearch/SeasonSearchCommand.cs).

use super::http::{as_u64, Api, ExecutorError};
use super::state::RestoreTarget;
use super::Evicted;
use reqwest::Method;
use serde_json::{json, Value};

const SERIES: &str = "sonarr series";
const FILES: &str = "sonarr episode files";
const DELETE_FILES: &str = "sonarr delete episode files";
const COMMAND: &str = "sonarr command";

pub struct Sonarr<'a> {
    api: Api<'a>,
}

/// The season's `monitored` flag in a series resource; `None` when the series
/// has no such season.
fn season_monitored(series: &Value, season: u32) -> Option<bool> {
    series["seasons"]
        .as_array()?
        .iter()
        .find(|row| as_u64(&row["seasonNumber"]) == Some(u64::from(season)))
        .map(|row| row["monitored"].as_bool().unwrap_or(false))
}

impl<'a> Sonarr<'a> {
    pub fn new(http: &'a reqwest::Client, base: &'a str, key: &'a str, dry_run: bool) -> Self {
        Self { api: Api::new(http, base, key, "sonarr", dry_run) }
    }

    async fn series(&self, series_id: u32) -> Result<Value, ExecutorError> {
        match self.api.find(SERIES, &format!("/api/v3/series/{series_id}")).await? {
            Some(series) => Ok(series),
            None => Err(ExecutorError::NotApplied { endpoint: SERIES, detail: format!("series {series_id} is no longer in Sonarr") }),
        }
    }

    /// The ids of the season's episode files.
    async fn season_files(&self, series_id: u32, season: u32) -> Result<Vec<u64>, ExecutorError> {
        let rows = self.api.get(FILES, "/api/v3/episodefile", &[("seriesId", series_id.to_string())]).await?;
        let Some(rows) = rows.as_array() else {
            return Err(ExecutorError::Parse { endpoint: FILES, detail: "not an array".into() });
        };
        Ok(rows.iter().filter(|row| as_u64(&row["seasonNumber"]) == Some(u64::from(season))).filter_map(|row| as_u64(&row["id"])).collect())
    }

    /// Set the season's flag through the whole series resource, as Sonarr's
    /// own UI does.
    async fn set_season_monitored(&self, mut series: Value, series_id: u32, season: u32, monitored: bool) -> Result<(), ExecutorError> {
        let Some(row) = series["seasons"]
            .as_array_mut()
            .and_then(|seasons| seasons.iter_mut().find(|row| as_u64(&row["seasonNumber"]) == Some(u64::from(season))))
        else {
            return Err(ExecutorError::NotApplied { endpoint: SERIES, detail: format!("series {series_id} has no season {season}") });
        };
        row["monitored"] = json!(monitored);
        self.api.write(SERIES, Method::PUT, &format!("/api/v3/series/{series_id}"), &[], Some(&series)).await.map(drop)
    }

    /// Unmonitor the season, delete its episode files, read both back. The
    /// season is unmonitored first, so files deleted while monitored are never
    /// fetched again; a failed delete monitors it again and reports nothing
    /// done.
    pub async fn evict(&self, series_id: u32, season: u32) -> Result<Evicted, ExecutorError> {
        let series = self.series(series_id).await?;
        let was_monitored = season_monitored(&series, season)
            .ok_or_else(|| ExecutorError::NotApplied { endpoint: SERIES, detail: format!("series {series_id} has no season {season}") })?;
        let files = self.season_files(series_id, season).await?;
        if files.is_empty() {
            return Err(ExecutorError::NotApplied {
                endpoint: FILES,
                detail: format!("season {season} of series {series_id} has no files"),
            });
        }
        self.set_season_monitored(series.clone(), series_id, season, false).await?;
        let body = json!({ "episodeFileIds": files });
        if let Err(error) = self.api.write(DELETE_FILES, Method::DELETE, "/api/v3/episodefile/bulk", &[], Some(&body)).await {
            if was_monitored {
                if let Err(undo) = self.set_season_monitored(series, series_id, season, true).await {
                    eprintln!(
                        "[flinch-arrd] native: sonarr series {series_id} season {season} left unmonitored after a failed delete: {undo}"
                    );
                }
            }
            return Err(error);
        }
        if self.api.dry_run {
            return Ok(Evicted::Simulated);
        }
        let left = self.season_files(series_id, season).await?;
        let monitored = season_monitored(&self.series(series_id).await?, season);
        if !left.is_empty() || monitored != Some(false) {
            return Err(ExecutorError::NotApplied {
                endpoint: DELETE_FILES,
                detail: format!("season {season} of series {series_id} reads back {} file(s), monitored={monitored:?}", left.len()),
            });
        }
        Ok(Evicted::Done(RestoreTarget::Sonarr { series_id, season }))
    }

    /// Undo: monitor the season again and search it. Returns whether it was
    /// sent (false in a dry run).
    pub async fn restore(&self, series_id: u32, season: u32) -> Result<bool, ExecutorError> {
        let series = self.series(series_id).await?;
        self.set_season_monitored(series, series_id, season, true).await?;
        let command = json!({ "name": "SeasonSearch", "seriesId": series_id, "seasonNumber": season });
        self.api.write(COMMAND, Method::POST, "/api/v3/command", &[], Some(&command)).await?;
        if self.api.dry_run {
            return Ok(false);
        }
        match season_monitored(&self.series(series_id).await?, season) {
            Some(true) => Ok(true),
            _ => Err(ExecutorError::NotApplied {
                endpoint: SERIES,
                detail: format!("season {season} of series {series_id} is not monitored"),
            }),
        }
    }
}
