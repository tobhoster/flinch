//! Jellyfin/Emby collections (BoxSets) FLINCH owns: the native Leaving Soon
//! shelf for a household that watches on Jellyfin or Emby.
//!
//! The contract matches [`crate::plex::collections::PlexCollections`]: find or
//! create the shelf, list members, add, remove, and read every write back,
//! because a 2xx proves only that the request parsed. A dry run prints each
//! write and sends none. One shelf serves the whole server (a BoxSet is not
//! tied to a library), so there is no section and no home "promote": Jellyfin
//! and Emby have no server-side home hubs. Name the shelf so it sorts first.
//!
//! Endpoints (both servers bind query names case-insensitively):
//! - Jellyfin `CollectionController` (v10.11.8,
//!   <https://github.com/jellyfin/jellyfin/blob/v10.11.8/Jellyfin.Api/Controllers/CollectionController.cs>):
//!   `POST /Collections?name=&ids=` → `{"Id": …}`, `POST /Collections/{id}/Items?ids=`,
//!   `DELETE /Collections/{id}/Items?ids=` (204).
//! - Emby `CollectionService` (<https://dev.emby.media/reference/RestAPI/CollectionService/postCollections.html>,
//!   `…/postCollectionsByIdItems.html`, `…/deleteCollectionsByIdItems.html`):
//!   the same three paths with `Name`/`Ids`; 200 with an empty body.
//! - Reads go through a user's item listing ([`super::JellyfinClient`]):
//!   members are `ParentId={collection}`, the shelf is found with
//!   `IncludeItemTypes=BoxSet&Recursive=true&SearchTerm=` and an exact name
//!   match. The user is the first administrator from `GET /Users`
//!   (`Policy.IsAdministrator`): a BoxSet filters its members per user
//!   (`BoxSet.FilterLinkedChildrenPerUser`), and an administrator sees them all.
//!
//! Seasons. Jellyfin's `CollectionManager.AddToCollectionAsync`
//! (<https://github.com/jellyfin/jellyfin/blob/v10.11.8/Emby.Server.Implementations/Collections/CollectionManager.cs>)
//! accepts any item id it can resolve, with no type check, and jellyfin-web's
//! collection page (`src/controllers/itemDetails/index.js`, `renderCollectionItems`)
//! lists a Season member under "Other items". Emby's docs name no type limit
//! either. So seasons are announced, but this was determined from source, not
//! from a live server: every add is read back, and a season the server drops
//! fails the read-back and stays held (fail closed), never deleted unannounced.

use super::{JellyfinClient, JellyfinError, JellyfinItem, ServerKind};
use reqwest::{Method, RequestBuilder};
use serde::Deserialize;
use std::sync::OnceLock;

/// Ids per write, so a large batch stays under URL length limits.
const IDS_PER_WRITE: usize = 100;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Listing {
    #[serde(default)]
    items: Vec<JellyfinItem>,
    total_record_count: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Created {
    id: Option<String>,
}

/// One server's collections, through the no-redirect client.
pub struct JellyfinCollections<'a> {
    client: &'a JellyfinClient,
    dry_run: bool,
    admin: OnceLock<String>,
}

impl<'a> JellyfinCollections<'a> {
    pub fn new(client: &'a JellyfinClient, dry_run: bool) -> Self {
        Self { client, dry_run, admin: OnceLock::new() }
    }

    fn label(&self) -> &'static str {
        self.client.kind().label()
    }

    /// A write: success is any 2xx; the body (204, or Emby's empty 200) is not read.
    async fn write(&self, endpoint: &'static str, request: RequestBuilder) -> Result<(), JellyfinError> {
        let response = request.send().await.map_err(|source| JellyfinError::Transport { endpoint, source: source.without_url() })?;
        let status = response.status();
        if !status.is_success() {
            return Err(JellyfinError::Status { endpoint, status: status.as_u16() });
        }
        Ok(())
    }

    /// The first administrator: the user every read goes through.
    async fn admin(&self) -> Result<String, JellyfinError> {
        if let Some(admin) = self.admin.get() {
            return Ok(admin.clone());
        }
        let users = self.client.users().await?;
        let admin = users
            .into_iter()
            .find(|user| user.policy.as_ref().is_some_and(|policy| policy.is_administrator))
            .ok_or_else(|| JellyfinError::Unexpected { endpoint: "users", detail: "no administrator listed".into() })?
            .id;
        Ok(self.admin.get_or_init(|| admin).clone())
    }

    /// Every row of one listing; a listing short of its own total is an error.
    async fn listing(&self, endpoint: &'static str, query: &[(&str, &str)]) -> Result<Vec<JellyfinItem>, JellyfinError> {
        let admin = self.admin().await?;
        let request = self.client.user_listing(&admin).query(query).query(&[("EnableTotalRecordCount", "true"), ("EnableImages", "false")]);
        let listing: Listing = JellyfinClient::json(endpoint, request).await?;
        if listing.total_record_count.is_some_and(|total| total > listing.items.len()) {
            return Err(JellyfinError::Unexpected {
                endpoint,
                detail: format!("{} of {:?} row(s) returned", listing.items.len(), listing.total_record_count),
            });
        }
        Ok(listing.items)
    }

    /// The id of the collection named exactly `name`.
    pub async fn find(&self, name: &str) -> Result<Option<String>, JellyfinError> {
        let rows = self.listing("collections", &[("IncludeItemTypes", "BoxSet"), ("Recursive", "true"), ("SearchTerm", name)]).await?;
        Ok(rows.into_iter().find(|row| row.kind == "BoxSet" && row.name == name).map(|row| row.id).filter(|id| !id.is_empty()))
    }

    /// The shelf's id, created with `seed` when absent, and every seed item
    /// read back as a member. `Ok(None)` only in a dry run with no shelf yet.
    /// The flag is true when this call created the collection.
    pub async fn ensure(&self, name: &str, seed: &[String]) -> Result<Option<(String, bool)>, JellyfinError> {
        if let Some(existing) = self.find(name).await? {
            self.add(&existing, seed).await?;
            return Ok(Some((existing, false)));
        }
        if self.dry_run {
            eprintln!("[flinch-arrd] dry-run: would create {} collection {name:?} with {} item(s)", self.label(), seed.len());
            return Ok(None);
        }
        const ENDPOINT: &str = "create collection";
        let first = &seed[..seed.len().min(IDS_PER_WRITE)];
        let ids = first.join(",");
        let mut query = vec![("Name", name)];
        if !ids.is_empty() {
            query.push(("Ids", ids.as_str()));
        }
        let created: Created = JellyfinClient::json(ENDPOINT, self.request(Method::POST, "/Collections").query(&query)).await?;
        let found = self
            .find(name)
            .await?
            .ok_or_else(|| JellyfinError::NotApplied { endpoint: ENDPOINT, detail: format!("{name:?} is not listed after creation") })?;
        if created.id.as_deref().is_some_and(|id| id != found) {
            return Err(JellyfinError::NotApplied { endpoint: ENDPOINT, detail: format!("{name:?} is listed under another id") });
        }
        self.add(&found, seed).await?;
        Ok(Some((found, true)))
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.client.request(method, path)
    }

    /// Member item ids (movies, or season ids).
    pub async fn members(&self, collection: &str) -> Result<Vec<String>, JellyfinError> {
        let rows = self.listing("collection members", &[("ParentId", collection)]).await?;
        Ok(rows.into_iter().map(|row| row.id).collect())
    }

    /// Adds whichever of `ids` are not members yet, then reads back that every
    /// one is listed.
    pub async fn add(&self, collection: &str, ids: &[String]) -> Result<(), JellyfinError> {
        const ENDPOINT: &str = "add to collection";
        let members = self.members(collection).await?;
        let new: Vec<&str> = ids.iter().map(String::as_str).filter(|id| !members.iter().any(|member| member == id)).collect();
        if new.is_empty() {
            return Ok(());
        }
        if self.dry_run {
            eprintln!(
                "[flinch-arrd] dry-run: would add {} item(s) to {} collection {collection}: {}",
                new.len(),
                self.label(),
                new.join(",")
            );
            return Ok(());
        }
        for chunk in new.chunks(IDS_PER_WRITE) {
            let request = self.request(Method::POST, &format!("/Collections/{collection}/Items")).query(&[("Ids", chunk.join(","))]);
            self.write(ENDPOINT, request).await?;
        }
        let members = self.members(collection).await?;
        let missing: Vec<&str> = new.into_iter().filter(|id| !members.iter().any(|member| member == id)).collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(JellyfinError::NotApplied { endpoint: ENDPOINT, detail: format!("not members: {}", missing.join(",")) })
        }
    }

    /// Removes one member, then reads back that it is gone.
    pub async fn remove(&self, collection: &str, id: &str) -> Result<(), JellyfinError> {
        const ENDPOINT: &str = "remove from collection";
        if self.dry_run {
            eprintln!("[flinch-arrd] dry-run: would remove {id} from {} collection {collection}", self.label());
            return Ok(());
        }
        self.write(ENDPOINT, self.request(Method::DELETE, &format!("/Collections/{collection}/Items")).query(&[("Ids", id)])).await?;
        if self.members(collection).await?.iter().any(|member| member == id) {
            return Err(JellyfinError::NotApplied { endpoint: ENDPOINT, detail: format!("{id} is still a member") });
        }
        Ok(())
    }

    pub fn kind(&self) -> ServerKind {
        self.client.kind()
    }
}

#[cfg(test)]
mod tests;
