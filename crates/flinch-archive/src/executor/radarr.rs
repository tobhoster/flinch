//! Radarr writes for the native executor: delete a movie (its file and
//! unmonitor it, or the whole entry), verified by reading it back, and undo.
//!
//! Endpoints from Radarr's API v3 (https://radarr.video/docs/api/, the
//! OpenAPI document at
//! https://github.com/Radarr/Radarr/blob/develop/src/Radarr.Api.V3/openapi.json):
//! `GET/DELETE /api/v3/movie/{id}` (`deleteFiles`, `addImportExclusion`),
//! `DELETE /api/v3/moviefile/{id}`, `PUT /api/v3/movie/editor`
//! (`movieIds`, `monitored`), `GET /api/v3/movie/lookup/tmdb?tmdbId=`,
//! `POST /api/v3/movie` (with `addOptions.searchForMovie`), and
//! `POST /api/v3/command` with `MoviesSearch` (`movieIds`, see
//! src/NzbDrone.Core/IndexerSearch/MoviesSearchCommand.cs).

use super::http::{as_u64, Api, ExecutorError};
use super::state::RestoreTarget;
use super::{DeleteMode, Evicted};
use reqwest::Method;
use serde_json::{json, Value};

const MOVIE: &str = "radarr movie";
const EDITOR: &str = "radarr movie editor";
const MOVIE_FILE: &str = "radarr movie file";
const REMOVE: &str = "radarr remove movie";
const LOOKUP: &str = "radarr movie lookup";
const ADD: &str = "radarr add movie";
const COMMAND: &str = "radarr command";

/// A movie as the delete and its read-back need it.
#[derive(Debug, Clone, PartialEq)]
pub struct MovieState {
    pub monitored: bool,
    pub file_id: Option<u64>,
    pub tmdb_id: Option<u32>,
    pub quality_profile_id: Option<u64>,
    pub root_folder_path: Option<String>,
}

impl MovieState {
    fn of(value: &Value) -> Self {
        let has_file = value["hasFile"].as_bool().unwrap_or(false);
        Self {
            monitored: value["monitored"].as_bool().unwrap_or(false),
            file_id: has_file.then(|| as_u64(&value["movieFile"]["id"])).flatten(),
            tmdb_id: as_u64(&value["tmdbId"]).and_then(|id| u32::try_from(id).ok()),
            quality_profile_id: as_u64(&value["qualityProfileId"]),
            root_folder_path: value["rootFolderPath"].as_str().filter(|path| !path.is_empty()).map(str::to_string),
        }
    }
}

pub struct Radarr<'a> {
    api: Api<'a>,
}

impl<'a> Radarr<'a> {
    pub fn new(http: &'a reqwest::Client, base: &'a str, key: &'a str, dry_run: bool) -> Self {
        Self { api: Api::new(http, base, key, "radarr", dry_run) }
    }

    /// `None` when Radarr no longer has the movie.
    pub async fn movie(&self, id: u32) -> Result<Option<MovieState>, ExecutorError> {
        Ok(self.api.find(MOVIE, &format!("/api/v3/movie/{id}")).await?.map(|value| MovieState::of(&value)))
    }

    async fn set_monitored(&self, id: u32, monitored: bool) -> Result<(), ExecutorError> {
        let body = json!({ "movieIds": [id], "monitored": monitored });
        self.api.write(EDITOR, Method::PUT, "/api/v3/movie/editor", &[], Some(&body)).await.map(drop)
    }

    /// Delete the movie as `mode` says, then read it back. File mode
    /// unmonitors first, so a file deleted while still monitored is never
    /// downloaded again; if the file delete then fails the movie is
    /// monitored again, and nothing is reported done.
    pub async fn evict(&self, id: u32, mode: DeleteMode, add_import_exclusion: bool) -> Result<Evicted, ExecutorError> {
        let Some(before) = self.movie(id).await? else {
            return Err(ExecutorError::NotApplied { endpoint: MOVIE, detail: format!("movie {id} is no longer in Radarr") });
        };
        let target = RestoreTarget::Radarr {
            radarr_id: id,
            tmdb_id: before.tmdb_id,
            mode,
            quality_profile_id: before.quality_profile_id,
            root_folder_path: before.root_folder_path.clone(),
        };
        match mode {
            DeleteMode::FileAndUnmonitor => {
                let Some(file_id) = before.file_id else {
                    return Err(ExecutorError::NotApplied { endpoint: MOVIE_FILE, detail: format!("movie {id} has no file") });
                };
                self.set_monitored(id, false).await?;
                if let Err(error) = self.api.write(MOVIE_FILE, Method::DELETE, &format!("/api/v3/moviefile/{file_id}"), &[], None).await {
                    if before.monitored {
                        if let Err(undo) = self.set_monitored(id, true).await {
                            eprintln!("[flinch-arrd] native: radarr movie {id} left unmonitored after a failed delete: {undo}");
                        }
                    }
                    return Err(error);
                }
                if self.api.dry_run {
                    return Ok(Evicted::Simulated);
                }
                match self.movie(id).await? {
                    Some(after) if !after.monitored && after.file_id.is_none() => Ok(Evicted::Done(target)),
                    Some(after) => Err(ExecutorError::NotApplied {
                        endpoint: MOVIE_FILE,
                        detail: format!("movie {id} reads back monitored={} with file={}", after.monitored, after.file_id.is_some()),
                    }),
                    None => Err(ExecutorError::NotApplied { endpoint: MOVIE_FILE, detail: format!("movie {id} vanished from Radarr") }),
                }
            }
            DeleteMode::RemoveEntry => {
                let query = [("deleteFiles", "true".to_string()), ("addImportExclusion", add_import_exclusion.to_string())];
                self.api.write(REMOVE, Method::DELETE, &format!("/api/v3/movie/{id}"), &query, None).await?;
                if self.api.dry_run {
                    return Ok(Evicted::Simulated);
                }
                match self.movie(id).await? {
                    None => Ok(Evicted::Done(target)),
                    Some(_) => Err(ExecutorError::NotApplied { endpoint: REMOVE, detail: format!("movie {id} is still in Radarr") }),
                }
            }
        }
    }

    /// Undo a delete: monitor and search again, or add a removed entry back
    /// (searching on add). Returns whether it was sent (false in a dry run).
    pub async fn restore(&self, target: &RestoreTarget) -> Result<bool, ExecutorError> {
        let RestoreTarget::Radarr { radarr_id, tmdb_id, mode, quality_profile_id, root_folder_path } = target else {
            return Err(ExecutorError::NotApplied { endpoint: MOVIE, detail: "not a Radarr delete".into() });
        };
        let (radarr_id, tmdb_id, quality_profile_id, root_folder_path) =
            (*radarr_id, *tmdb_id, *quality_profile_id, root_folder_path.as_deref());
        match mode {
            DeleteMode::FileAndUnmonitor => {
                self.set_monitored(radarr_id, true).await?;
                let command = json!({ "name": "MoviesSearch", "movieIds": [radarr_id] });
                self.api.write(COMMAND, Method::POST, "/api/v3/command", &[], Some(&command)).await?;
                if self.api.dry_run {
                    return Ok(false);
                }
                match self.movie(radarr_id).await? {
                    Some(after) if after.monitored => Ok(true),
                    _ => Err(ExecutorError::NotApplied { endpoint: EDITOR, detail: format!("movie {radarr_id} is not monitored") }),
                }
            }
            DeleteMode::RemoveEntry => {
                let (Some(tmdb_id), Some(profile), Some(root)) = (tmdb_id, quality_profile_id, root_folder_path) else {
                    return Err(ExecutorError::NotApplied {
                        endpoint: ADD,
                        detail: "the removed entry's TMDB id, quality profile or root folder was not recorded".into(),
                    });
                };
                let mut movie = self.api.get(LOOKUP, "/api/v3/movie/lookup/tmdb", &[("tmdbId", tmdb_id.to_string())]).await?;
                let Some(fields) = movie.as_object_mut() else {
                    return Err(ExecutorError::Parse { endpoint: LOOKUP, detail: "not a movie object".into() });
                };
                fields.insert("qualityProfileId".into(), json!(profile));
                fields.insert("rootFolderPath".into(), json!(root));
                fields.insert("monitored".into(), json!(true));
                fields.insert("addOptions".into(), json!({ "searchForMovie": true }));
                self.api.write(ADD, Method::POST, "/api/v3/movie", &[], Some(&movie)).await?;
                if self.api.dry_run {
                    return Ok(false);
                }
                let listed = self.api.get(MOVIE, "/api/v3/movie", &[("tmdbId", tmdb_id.to_string())]).await?;
                match listed.as_array().and_then(|rows| rows.first()) {
                    Some(row) if MovieState::of(row).monitored => Ok(true),
                    _ => Err(ExecutorError::NotApplied { endpoint: ADD, detail: format!("TMDB {tmdb_id} is not monitored in Radarr") }),
                }
            }
        }
    }
}
