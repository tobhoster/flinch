//! Opt-in poster badges for the Leaving Soon shelf (`native.poster_overlays`):
//! a poster that says "LEAVES OCT 23" warns people browsing the library, not
//! only those who open the shelf (Maintainerr hub #62, #174).
//!
//! FLINCH rewrites a poster only with a way back: before uploading the badged
//! copy it records the poster Plex had selected in `overlays.json`, and it
//! selects that one again before the item leaves the shelf (taken back, kept,
//! deleted) and whenever the setting is turned off. Kometa overlays rewrite
//! posters too; the two fight over the same image, so enable one of them for
//! shelf items, not both (see docs/how-it-works.md).
//!
//! Endpoints from python-plexapi 4.15.16 mixins.py / media.py
//! (https://github.com/pkkid/python-plexapi):
//! - `PosterMixin.posters`: `GET /library/metadata/{ratingKey}/posters`,
//!   each row with its own `ratingKey` and `selected`
//! - `PosterMixin.uploadPoster(filepath=…)`: `POST /library/metadata/{ratingKey}/posters`
//!   with the image as the body
//! - `setPoster` → `BaseResource.select`: `PUT /library/metadata/{ratingKey}/poster?url={poster ratingKey}`
//! - the current image: the item's `thumb` from `GET /library/metadata/{ratingKey}`

mod badge;

use crate::plex::collections::{CollectionError, PlexCollections};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A poster FLINCH replaced, by the item's ratingKey.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overlaid {
    /// The ratingKey of the poster that was selected before (e.g.
    /// `metadata://posters/…` or `upload://posters/…`), selected again on restore.
    pub original: String,
    /// The badge text drawn.
    pub badge: String,
    pub applied_at: u64,
}

/// `overlays.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayState {
    pub posters: BTreeMap<String, Overlaid>,
}

impl OverlayState {
    pub fn path(state_dir: &Path) -> PathBuf {
        state_dir.join("overlays.json")
    }

    /// A missing file is a first run. An unreadable one is an error, not a
    /// fresh start: it holds the only way back to the original posters.
    pub fn read(state_dir: &Path) -> Result<Self, OverlayError> {
        match std::fs::read_to_string(Self::path(state_dir)) {
            Ok(text) => serde_json::from_str(&text).map_err(|error| OverlayError::State(error.to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(OverlayError::State(error.to_string())),
        }
    }

    pub fn write(&self, state_dir: &Path) -> Result<(), OverlayError> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| OverlayError::State(error.to_string()))?;
        crate::persist::replace(&Self::path(state_dir), &bytes).map_err(|error| OverlayError::State(error.to_string()))
    }
}

/// This cycle's poster work: badges to draw (ratingKey, text) and posters to
/// put back (ratingKey).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct OverlayPlan {
    pub apply: Vec<(String, String)>,
    pub restore: Vec<String>,
}

/// The badge for a leave date: "LEAVES OCT 23".
pub fn badge_text(until: u64) -> String {
    format!("LEAVES {}", crate::plex::shelf::month_day(until)).to_uppercase()
}

/// Badge every shelf member (`shelf`: ratingKey, until) that lacks this
/// date's badge; restore every recorded poster no longer on the shelf, or all
/// of them when overlays are off.
pub fn plan(state: &OverlayState, shelf: &[(String, u64)], enabled: bool) -> OverlayPlan {
    let wanted: BTreeMap<&str, String> =
        if enabled { shelf.iter().map(|(key, until)| (key.as_str(), badge_text(*until))).collect() } else { BTreeMap::new() };
    OverlayPlan {
        apply: wanted
            .iter()
            .filter(|(key, text)| state.posters.get(**key).is_none_or(|done| &done.badge != *text))
            .map(|(key, text)| (key.to_string(), text.clone()))
            .collect(),
        restore: state.posters.keys().filter(|key| !wanted.contains_key(key.as_str())).cloned().collect(),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    #[error(transparent)]
    Plex(#[from] CollectionError),
    #[error("poster image: {0}")]
    Image(#[from] image::ImageError),
    #[error("overlays.json: {0}")]
    State(String),
}

const POSTERS: &str = "posters";

/// The selected poster's ratingKey; `None` when the item is gone from Plex.
async fn selected(plex: &PlexCollections<'_>, key: &str) -> Result<Option<String>, CollectionError> {
    let request = plex.request(Method::GET, &format!("/library/metadata/{key}/posters"));
    let body = match plex.send(POSTERS, request).await {
        Err(CollectionError::Http { status: 404, .. }) => return Ok(None),
        other => other?,
    };
    let rows = body["MediaContainer"]["Metadata"].as_array().cloned().unwrap_or_default();
    Ok(rows
        .iter()
        .find(|row| {
            matches!(&row["selected"], serde_json::Value::Bool(true))
                || row["selected"].as_str() == Some("1")
                || row["selected"].as_u64() == Some(1)
        })
        .and_then(|row| row["ratingKey"].as_str().map(str::to_string)))
}

/// Draws `text` on the item's current poster and uploads it, recording the
/// poster it replaces first (a badge already drawn keeps its first original).
pub async fn apply(plex: &PlexCollections<'_>, state: &mut OverlayState, key: &str, text: &str, now: u64) -> Result<(), OverlayError> {
    if plex.dry_run {
        println!("[dry-run] would badge the poster of plex item {key} with {text:?}");
        return Ok(());
    }
    let original = match state.posters.get(key) {
        Some(done) => done.original.clone(),
        None => selected(plex, key)
            .await?
            .ok_or_else(|| CollectionError::NotApplied { endpoint: POSTERS, detail: format!("item {key} has no selected poster") })?,
    };
    let body = plex.send("item", plex.request(Method::GET, &format!("/library/metadata/{key}"))).await?;
    let thumb = body["MediaContainer"]["Metadata"][0]["thumb"]
        .as_str()
        .filter(|thumb| thumb.starts_with('/'))
        .ok_or_else(|| CollectionError::Parse { endpoint: "item", detail: format!("item {key} has no thumb") })?
        .to_string();
    let image = download(plex, &thumb).await?;
    let badged = badge::draw(&image, text)?;
    // Recorded before the upload: a crash after it still knows the way back.
    state.posters.insert(key.to_string(), Overlaid { original: original.clone(), badge: text.to_string(), applied_at: now });
    let upload = plex.request(Method::POST, &format!("/library/metadata/{key}/posters")).header("Content-Type", "image/jpeg").body(badged);
    plex.send("upload poster", upload).await?;
    match selected(plex, key).await? {
        Some(now_selected) if now_selected != original => Ok(()),
        _ => Err(CollectionError::NotApplied { endpoint: "upload poster", detail: format!("item {key} kept its poster") }.into()),
    }
}

async fn download(plex: &PlexCollections<'_>, thumb: &str) -> Result<Vec<u8>, CollectionError> {
    const ENDPOINT: &str = "poster image";
    let response = plex
        .request(Method::GET, thumb)
        .send()
        .await
        .map_err(|source| CollectionError::Transport { endpoint: ENDPOINT, source: source.without_url() })?;
    if !response.status().is_success() {
        return Err(CollectionError::Http { endpoint: ENDPOINT, status: response.status().as_u16() });
    }
    crate::body::read(response).await.map_err(|source| CollectionError::Body { endpoint: ENDPOINT, source })
}

/// Selects the recorded original again and forgets the record once Plex
/// shows it (or the item is gone from Plex). Nothing recorded: nothing to do.
pub async fn restore(plex: &PlexCollections<'_>, state: &mut OverlayState, key: &str) -> Result<(), OverlayError> {
    let Some(done) = state.posters.get(key) else { return Ok(()) };
    if plex.dry_run {
        println!("[dry-run] would restore the original poster of plex item {key}");
        return Ok(());
    }
    let original = done.original.clone();
    match selected(plex, key).await? {
        None => {}
        Some(current) if current == original => {}
        Some(_) => {
            let request = plex.request(Method::PUT, &format!("/library/metadata/{key}/poster")).query(&[("url", original.as_str())]);
            plex.send("restore poster", request).await?;
            if selected(plex, key).await?.is_some_and(|current| current != original) {
                return Err(CollectionError::NotApplied { endpoint: "restore poster", detail: format!("item {key} kept the badge") }.into());
            }
        }
    }
    state.posters.remove(key);
    Ok(())
}

#[cfg(test)]
mod tests;
