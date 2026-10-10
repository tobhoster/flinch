//! Removing the redundant copies of a confirmed choice, each verified by
//! reading back that it is gone and the kept copy is still there.
//!
//! Two routes, from the copy's owner:
//! - A Plex copy no *arr tracks: `DELETE /library/metadata/{ratingKey}/media/{mediaId}`,
//!   as python-plexapi's `Media.delete` sends it (media.py: `self._initpath +
//!   '/media/%s' % self.id` with the DELETE method; https://github.com/pkkid/python-plexapi).
//!   Plex refuses unless Settings → Library → "Allow media deletion" is on.
//!   The read-back is `GET /library/metadata/{ratingKey}`'s `Media` ids.
//! - A file of a *second* *arr instance, when the kept copy is another
//!   instance's: the instance unmonitors the movie, then
//!   `DELETE /api/v3/moviefile/{id}` (https://radarr.video/docs/api/), through
//!   the native executor's verified [`crate::executor::radarr::Radarr::evict`].
//!
//! A copy the only *arr tracks is never removed: the *arr would download it
//! again, so the plan refuses and asks the operator to keep that copy.

use super::{Copy, Group};
use crate::executor::radarr::Radarr;
use crate::executor::{DeleteMode, Evicted, ExecutorError};
use reqwest::Method;

const MEDIA: &str = "plex media";

/// One redundant copy's removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    PlexMedia { rating_key: String, media_id: u64 },
    ArrFile { instance: String, movie_id: u32, file_id: u32 },
}

/// The removals a group's confirmed choice asks for: empty without one, and
/// an error naming why when it cannot be acted on. `protected` is a pinned
/// item or one someone is partway through: its copies stay as they are.
pub fn plan(group: &Group, protected: bool) -> Result<Vec<(&Copy, Removal)>, String> {
    let Some(decision) = group.confirmed() else { return Ok(Vec::new()) };
    if let Some(held) = &group.held {
        return Err(held.clone());
    }
    if protected {
        return Err("the item is pinned, or someone is partway through it: its copies stay as they are".to_string());
    }
    let Some(keep) = group.copy(&decision.keep) else { return Ok(Vec::new()) };
    let mut removals = Vec::new();
    for copy in group.copies.iter().filter(|copy| copy.id != keep.id) {
        let removal = match (&copy.owner, &keep.owner) {
            (Some(file), Some(kept)) if kept.instance != file.instance => {
                Removal::ArrFile { instance: file.instance.clone(), movie_id: file.movie_id, file_id: file.file_id }
            }
            (Some(file), _) => {
                return Err(format!(
                    "{} tracks {}: removing it would only download it again; keep that copy, or remove it in {}",
                    file.instance, file.path, file.instance
                ))
            }
            (None, _) => match (&copy.rating_key, copy.media_id) {
                (Some(rating_key), Some(media_id)) => Removal::PlexMedia { rating_key: rating_key.clone(), media_id },
                _ => return Err(format!("{} has no Plex media id to remove", copy.id)),
            },
        };
        removals.push((copy, removal));
    }
    Ok(removals)
}

/// One *arr instance's connection.
pub struct ArrInstance<'a> {
    pub name: &'a str,
    pub base: &'a str,
    pub key: &'a str,
}

/// Everything a removal talks to. The client must follow no redirects.
pub struct Clients<'a> {
    pub http: &'a reqwest::Client,
    /// Plex base URL and token; `None` when Plex is not configured.
    pub plex: Option<(&'a str, &'a str)>,
    pub arrs: &'a [ArrInstance<'a>],
    pub dry_run: bool,
}

impl Clients<'_> {
    fn radarr(&self, instance: &str) -> Result<Radarr<'_>, ExecutorError> {
        let arr = self
            .arrs
            .iter()
            .find(|arr| arr.name == instance)
            .ok_or_else(|| ExecutorError::NotApplied { endpoint: "radarr", detail: format!("no configured instance named {instance}") })?;
        Ok(Radarr::new(self.http, arr.base, arr.key, self.dry_run))
    }

    fn plex(&self) -> Result<(&str, &str), ExecutorError> {
        self.plex.ok_or_else(|| ExecutorError::NotApplied { endpoint: MEDIA, detail: "Plex is not configured".into() })
    }

    /// The `Media` ids Plex lists for `rating_key`; empty when it has no such item.
    async fn media_ids(&self, rating_key: &str) -> Result<Vec<u64>, ExecutorError> {
        let (base, token) = self.plex()?;
        let response = self
            .http
            .get(format!("{}/library/metadata/{rating_key}", base.trim_end_matches('/')))
            .header("X-Plex-Token", token)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|source| ExecutorError::Transport { endpoint: MEDIA, source: source.without_url() })?;
        let status = response.status().as_u16();
        if status == 404 {
            return Ok(Vec::new());
        }
        if !(200..300).contains(&status) {
            return Err(ExecutorError::Http { endpoint: MEDIA, status, detail: String::new() });
        }
        let body = crate::body::read_text(response).await.map_err(|source| ExecutorError::Body { endpoint: MEDIA, source })?;
        let value: serde_json::Value =
            serde_json::from_str(&body).map_err(|error| ExecutorError::Parse { endpoint: MEDIA, detail: error.to_string() })?;
        let media = value["MediaContainer"]["Metadata"][0]["Media"].as_array().cloned().unwrap_or_default();
        Ok(media.iter().filter_map(|media| media["id"].as_u64().or_else(|| media["id"].as_str()?.parse().ok())).collect())
    }

    async fn delete_media(&self, rating_key: &str, media_id: u64) -> Result<(), ExecutorError> {
        let path = format!("/library/metadata/{rating_key}/media/{media_id}");
        if self.dry_run {
            println!("[dry-run] would DELETE Plex {path}");
            return Ok(());
        }
        let (base, token) = self.plex()?;
        let response = self
            .http
            .request(Method::DELETE, format!("{}{path}", base.trim_end_matches('/')))
            .header("X-Plex-Token", token)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|source| ExecutorError::Transport { endpoint: MEDIA, source: source.without_url() })?;
        let status = response.status().as_u16();
        match status {
            200..=299 => Ok(()),
            401 | 403 => Err(ExecutorError::Http {
                endpoint: MEDIA,
                status,
                detail: "Plex refused: turn on Settings → Library → \"Allow media deletion\", with the server owner's token".into(),
            }),
            _ => Err(ExecutorError::Http { endpoint: MEDIA, status, detail: String::new() }),
        }
    }

    /// Whether the kept copy is still there.
    async fn present(&self, keep: &Copy) -> Result<bool, ExecutorError> {
        if let (Some(rating_key), Some(media_id)) = (&keep.rating_key, keep.media_id) {
            return Ok(self.media_ids(rating_key).await?.contains(&media_id));
        }
        match &keep.owner {
            Some(file) => {
                Ok(self.radarr(&file.instance)?.movie(file.movie_id).await?.and_then(|movie| movie.file_id)
                    == Some(u64::from(file.file_id)))
            }
            None => Ok(false),
        }
    }

    /// Remove one redundant copy, keeping `keep`: checked before, read back
    /// after. `Ok(false)` is a dry run, which sends nothing.
    pub async fn remove(&self, keep: &Copy, removal: &Removal) -> Result<bool, ExecutorError> {
        let gone = |detail: String| ExecutorError::NotApplied { endpoint: MEDIA, detail };
        if !self.present(keep).await? {
            return Err(gone(format!("the kept copy {} is not there; nothing removed", keep.id)));
        }
        match removal {
            Removal::PlexMedia { rating_key, media_id } => {
                if !self.media_ids(rating_key).await?.contains(media_id) {
                    return Err(gone(format!("Plex no longer lists media {media_id} of {rating_key}")));
                }
                self.delete_media(rating_key, *media_id).await?;
                if self.dry_run {
                    return Ok(false);
                }
                if self.media_ids(rating_key).await?.contains(media_id) {
                    return Err(gone(format!("Plex still lists media {media_id} of {rating_key}")));
                }
            }
            Removal::ArrFile { instance, movie_id, file_id } => {
                let radarr = self.radarr(instance)?;
                let now = radarr.movie(*movie_id).await?.and_then(|movie| movie.file_id);
                if now != Some(u64::from(*file_id)) {
                    return Err(gone(format!("{instance} movie {movie_id} no longer has file {file_id}")));
                }
                if radarr.evict(*movie_id, DeleteMode::FileAndUnmonitor, false).await? == Evicted::Simulated {
                    return Ok(false);
                }
            }
        }
        if !self.present(keep).await? {
            return Err(gone(format!("the kept copy {} is gone after the removal", keep.id)));
        }
        Ok(true)
    }
}
