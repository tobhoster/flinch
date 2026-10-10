//! Plex collections FLINCH owns itself: the native Leaving Soon shelf.
//!
//! Without Maintainerr, the warning window has to be visible where people
//! watch, so FLINCH keeps a regular (non-smart) collection per section and
//! promotes it to the section's recommended and home hubs. Every write is read
//! back, because Plex answers 200 to requests it quietly ignores (a non-owner
//! token, an unknown ratingKey); a dry run prints each write and sends none.
//!
//! Endpoints follow python-plexapi, the reference client for this unofficial
//! API (https://github.com/pkkid/python-plexapi):
//! - collection.py `Collection._create`: `POST /library/collections?uri=…&type=…&title=…&smart=0&sectionId=…`
//! - collection.py `addItems`: `PUT /library/collections/{id}/items?uri=…`
//! - collection.py `removeItems`: `DELETE /library/collections/{id}/items/{ratingKey}`
//! - collection.py `visibility` and library.py `ManagedHub.updateVisibility`:
//!   `GET /hubs/sections/{section}/manage?metadataItemId={id}`, then `PUT
//!   …/manage/{identifier}` for a managed hub or `POST …/manage` with
//!   `metadataItemId` for one not managed yet
//! - server.py `_uriRoot`: `server://{machineIdentifier}/com.plexapp.plugins.library`,
//!   the identifier read from `GET /`
//! - library.py `LibrarySection.collections`: a section search with `type=18`.
//! - mixins.py `SummaryMixin.editSummary` → base.py `PlexPartialObject._edit`:
//!   `PUT /library/sections/{section}/all?type=18&id={ratingKey}&summary.value=…&summary.locked=1`,
//!   read back from `GET /library/metadata/{ratingKey}`.

use crate::card::LibraryKind;
use reqwest::{Method, RequestBuilder};
use serde_json::Value;
use std::sync::OnceLock;

/// Plex's search type for collections (python-plexapi utils.SEARCHTYPES).
const COLLECTION_TYPE: u8 = 18;
/// ratingKeys per `uri=` so a large batch stays under URL length limits.
const KEYS_PER_URI: usize = 100;
/// Rows asked for in one listing; a listing reporting more is an error, not a
/// silent under-read.
const PAGE: usize = 10_000;

#[derive(Debug, thiserror::Error)]
pub enum CollectionError {
    /// The URL is stripped: it may name the server's address.
    #[error("plex {endpoint}: request failed: {source}")]
    Transport { endpoint: &'static str, source: reqwest::Error },
    #[error("plex {endpoint}: {source}")]
    Body { endpoint: &'static str, source: crate::body::BodyError },
    /// Any non-2xx, redirects included: the token would travel with one.
    #[error("plex {endpoint}: HTTP {status}")]
    Http { endpoint: &'static str, status: u16 },
    #[error("plex {endpoint}: unexpected response: {detail}")]
    Parse { endpoint: &'static str, detail: String },
    /// Plex accepted a write the read-back does not show; usually a token that
    /// is not the server owner's.
    #[error("plex {endpoint}: write not applied: {detail}")]
    NotApplied { endpoint: &'static str, detail: String },
}

/// One server's collections, through a client that follows no redirects.
pub struct PlexCollections<'a> {
    http: &'a reqwest::Client,
    base: &'a str,
    token: &'a str,
    pub(crate) dry_run: bool,
    machine: OnceLock<String>,
}

impl<'a> PlexCollections<'a> {
    pub fn new(http: &'a reqwest::Client, base: &'a str, token: &'a str, dry_run: bool) -> Self {
        Self { http, base: base.trim_end_matches('/'), token, dry_run, machine: OnceLock::new() }
    }

    pub(crate) fn request(&self, method: Method, path: &str) -> RequestBuilder {
        // The token travels in the header only, never in a URL that is logged.
        self.http.request(method, format!("{}{path}", self.base)).header("X-Plex-Token", self.token).header("Accept", "application/json")
    }

    pub(crate) async fn send(&self, endpoint: &'static str, request: RequestBuilder) -> Result<Value, CollectionError> {
        let response = request.send().await.map_err(|source| CollectionError::Transport { endpoint, source: source.without_url() })?;
        let status = response.status();
        if !status.is_success() {
            return Err(CollectionError::Http { endpoint, status: status.as_u16() });
        }
        let body = crate::body::read_text(response).await.map_err(|source| CollectionError::Body { endpoint, source })?;
        if body.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&body).map_err(|error| CollectionError::Parse { endpoint, detail: error.to_string() })
    }

    async fn listing(&self, endpoint: &'static str, path: &str, query: &[(&str, String)]) -> Result<Vec<Value>, CollectionError> {
        let request = self
            .request(Method::GET, path)
            .query(query)
            .header("X-Plex-Container-Start", "0")
            .header("X-Plex-Container-Size", PAGE.to_string());
        let body = self.send(endpoint, request).await?;
        let container = &body["MediaContainer"];
        if !container.is_object() {
            return Err(CollectionError::Parse { endpoint, detail: "no MediaContainer".into() });
        }
        let rows = container["Metadata"].as_array().cloned().unwrap_or_default();
        if let Some(total) = container["totalSize"].as_u64() {
            if total as usize > rows.len() {
                return Err(CollectionError::Parse { endpoint, detail: format!("{} of {total} row(s) returned", rows.len()) });
            }
        }
        Ok(rows)
    }

    /// The server's machine identifier, read once: the root of every `uri=`.
    pub async fn machine_identifier(&self) -> Result<String, CollectionError> {
        if let Some(machine) = self.machine.get() {
            return Ok(machine.clone());
        }
        const ENDPOINT: &str = "server identity";
        let body = self.send(ENDPOINT, self.request(Method::GET, "/")).await?;
        let machine = body["MediaContainer"]["machineIdentifier"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| CollectionError::Parse { endpoint: ENDPOINT, detail: "no machineIdentifier".into() })?
            .to_string();
        Ok(self.machine.get_or_init(|| machine).clone())
    }

    async fn uri(&self, rating_keys: &[String]) -> Result<String, CollectionError> {
        let machine = self.machine_identifier().await?;
        Ok(format!("server://{machine}/com.plexapp.plugins.library/library/metadata/{}", rating_keys.join(",")))
    }

    /// The ratingKey of the regular collection titled exactly `title` in the
    /// section. Smart collections are never ours to fill.
    pub async fn find(&self, section_id: u32, title: &str) -> Result<Option<String>, CollectionError> {
        let rows =
            self.listing("collections", &format!("/library/sections/{section_id}/all"), &[("type", COLLECTION_TYPE.to_string())]).await?;
        Ok(rows.iter().filter(|row| row["title"].as_str() == Some(title) && !flag(&row["smart"])).find_map(rating_key))
    }

    /// The collection's ratingKey, created with `seed` when absent. Plex
    /// cannot create an empty regular collection, so with no seed (or in a
    /// dry run) an absent collection stays absent: `Ok(None)`.
    pub async fn ensure(
        &self,
        section_id: u32,
        title: &str,
        kind: LibraryKind,
        seed: &[String],
    ) -> Result<Option<String>, CollectionError> {
        if let Some(existing) = self.find(section_id, title).await? {
            return Ok(Some(existing));
        }
        let Some(first) = seed.first() else { return Ok(None) };
        let keys = &seed[..seed.len().min(KEYS_PER_URI)];
        if self.dry_run {
            eprintln!("[flinch-arrd] dry-run: would create plex collection {title:?} in section {section_id} with {} item(s)", seed.len());
            return Ok(None);
        }
        let plex_type = match kind {
            LibraryKind::Movie => "1",
            LibraryKind::Season => "3",
        };
        let query = [
            ("uri", self.uri(keys).await?),
            ("type", plex_type.to_string()),
            ("title", title.to_string()),
            ("smart", "0".to_string()),
            ("sectionId", section_id.to_string()),
        ];
        self.send("create collection", self.request(Method::POST, "/library/collections").query(&query)).await?;
        let created = self.find(section_id, title).await?.ok_or_else(|| CollectionError::NotApplied {
            endpoint: "create collection",
            detail: format!("{title:?} is not listed after creation"),
        })?;
        let members = self.members(&created).await?;
        if !members.contains(first) {
            return Err(CollectionError::NotApplied { endpoint: "create collection", detail: format!("{first} is not a member") });
        }
        if seed.len() > keys.len() {
            self.add(&created, &seed[keys.len()..]).await?;
        }
        Ok(Some(created))
    }

    /// Member ratingKeys (movies, or season ratingKeys for a season shelf).
    pub async fn members(&self, collection: &str) -> Result<Vec<String>, CollectionError> {
        let rows = self.listing("collection members", &format!("/library/collections/{collection}/children"), &[]).await?;
        Ok(rows.iter().filter_map(rating_key).collect())
    }

    /// Adds members, then reads back that every one is listed.
    pub async fn add(&self, collection: &str, rating_keys: &[String]) -> Result<(), CollectionError> {
        if rating_keys.is_empty() {
            return Ok(());
        }
        if self.dry_run {
            eprintln!(
                "[flinch-arrd] dry-run: would add {} item(s) to plex collection {collection}: {}",
                rating_keys.len(),
                rating_keys.join(",")
            );
            return Ok(());
        }
        for chunk in rating_keys.chunks(KEYS_PER_URI) {
            let request =
                self.request(Method::PUT, &format!("/library/collections/{collection}/items")).query(&[("uri", self.uri(chunk).await?)]);
            self.send("add to collection", request).await?;
        }
        let members = self.members(collection).await?;
        let missing: Vec<&str> = rating_keys.iter().filter(|key| !members.contains(key)).map(String::as_str).collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(CollectionError::NotApplied { endpoint: "add to collection", detail: format!("not members: {}", missing.join(",")) })
        }
    }

    /// Removes one member, then reads back that it is gone.
    pub async fn remove(&self, collection: &str, rating_key: &str) -> Result<(), CollectionError> {
        if self.dry_run {
            eprintln!("[flinch-arrd] dry-run: would remove {rating_key} from plex collection {collection}");
            return Ok(());
        }
        self.send("remove from collection", self.request(Method::DELETE, &format!("/library/collections/{collection}/items/{rating_key}")))
            .await?;
        if self.members(collection).await?.iter().any(|key| key == rating_key) {
            return Err(CollectionError::NotApplied {
                endpoint: "remove from collection",
                detail: format!("{rating_key} is still a member"),
            });
        }
        Ok(())
    }

    /// Shows the collection on the section's recommended tab and on home, for
    /// the owner and shared users alike: a warning only the owner sees warns
    /// nobody who might miss the item.
    pub async fn promote_home(&self, collection: &str, section_id: u32) -> Result<(), CollectionError> {
        let hub = self.managed_hub(collection, section_id).await?;
        if hub.as_ref().is_some_and(promoted) {
            return Ok(());
        }
        if self.dry_run {
            eprintln!(
                "[flinch-arrd] dry-run: would promote plex collection {collection} to section {section_id}'s recommended and home hubs"
            );
            return Ok(());
        }
        let mut params = vec![
            ("promotedToRecommended", "1".to_string()),
            ("promotedToOwnHome", "1".to_string()),
            ("promotedToSharedHome", "1".to_string()),
        ];
        let request = match hub.as_ref().and_then(|hub| hub["identifier"].as_str()) {
            Some(identifier) => self.request(Method::PUT, &format!("/hubs/sections/{section_id}/manage/{identifier}")),
            None => {
                params.push(("metadataItemId", collection.to_string()));
                self.request(Method::POST, &format!("/hubs/sections/{section_id}/manage"))
            }
        };
        self.send("promote collection", request.query(&params)).await?;
        match self.managed_hub(collection, section_id).await? {
            Some(hub) if promoted(&hub) => Ok(()),
            _ => Err(CollectionError::NotApplied {
                endpoint: "promote collection",
                detail: format!("collection {collection} is not promoted"),
            }),
        }
    }

    async fn managed_hub(&self, collection: &str, section_id: u32) -> Result<Option<Value>, CollectionError> {
        const ENDPOINT: &str = "managed hubs";
        let request = self.request(Method::GET, &format!("/hubs/sections/{section_id}/manage")).query(&[("metadataItemId", collection)]);
        let body = self.send(ENDPOINT, request).await?;
        let suffix = format!(".{collection}");
        Ok(body["MediaContainer"]["Hub"]
            .as_array()
            .and_then(|hubs| hubs.iter().find(|hub| hub["identifier"].as_str().is_some_and(|id| id.ends_with(&suffix))))
            .cloned())
    }

    /// The collection's summary as Plex holds it now.
    pub async fn summary(&self, collection: &str) -> Result<String, CollectionError> {
        const ENDPOINT: &str = "collection summary";
        let body = self.send(ENDPOINT, self.request(Method::GET, &format!("/library/metadata/{collection}"))).await?;
        let row = body["MediaContainer"]["Metadata"]
            .as_array()
            .and_then(|rows| rows.first())
            .ok_or_else(|| CollectionError::Parse { endpoint: ENDPOINT, detail: format!("collection {collection} is not listed") })?;
        Ok(row["summary"].as_str().unwrap_or_default().to_string())
    }

    /// Sets (and locks, so a metadata refresh keeps it) the collection's
    /// summary, unless it already reads `summary`; then reads it back.
    pub async fn edit_summary(&self, section_id: u32, collection: &str, summary: &str) -> Result<(), CollectionError> {
        const ENDPOINT: &str = "edit collection summary";
        if self.summary(collection).await?.trim() == summary.trim() {
            return Ok(());
        }
        if self.dry_run {
            eprintln!(
                "[flinch-arrd] dry-run: would set the summary of plex collection {collection} ({} characters)",
                summary.chars().count()
            );
            return Ok(());
        }
        let query = [
            ("type", COLLECTION_TYPE.to_string()),
            ("id", collection.to_string()),
            ("summary.value", summary.to_string()),
            ("summary.locked", "1".to_string()),
        ];
        self.send(ENDPOINT, self.request(Method::PUT, &format!("/library/sections/{section_id}/all")).query(&query)).await?;
        if self.summary(collection).await?.trim() != summary.trim() {
            return Err(CollectionError::NotApplied {
                endpoint: ENDPOINT,
                detail: format!("collection {collection} kept its old summary"),
            });
        }
        Ok(())
    }
}

fn promoted(hub: &Value) -> bool {
    ["promotedToRecommended", "promotedToOwnHome", "promotedToSharedHome"].iter().all(|field| flag(&hub[*field]))
}

/// Plex writes flags as `true`, `1` or `"1"` depending on version.
fn flag(value: &Value) -> bool {
    match value {
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_u64() == Some(1),
        Value::String(text) => text == "1" || text == "true",
        _ => false,
    }
}

fn rating_key(row: &Value) -> Option<String> {
    match &row["ratingKey"] {
        Value::String(key) => Some(key.clone()),
        Value::Number(key) => Some(key.to_string()),
        _ => None,
    }
}

mod edit;

#[cfg(test)]
mod tests;
