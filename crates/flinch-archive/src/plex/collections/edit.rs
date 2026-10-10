//! Shaping a collection FLINCH fills: its title, its member order, and its
//! removal once empty (Plex never removes an empty collection itself; "40
//! empty collections after Maintainerr", r/PleX 1j1y94n). Each write is read
//! back; a dry run prints and sends nothing.
//!
//! Endpoints from python-plexapi 4.15.16 (https://github.com/pkkid/python-plexapi):
//! - mixins.py `TitleMixin.editTitle` → base.py `_edit`:
//!   `PUT /library/sections/{section}/all?type=18&id={key}&title.value=…&title.locked=1`
//! - collection.py `sortUpdate("custom")` → mixins.py `editAdvanced`:
//!   `PUT /library/collections/{key}/prefs?collectionSort=2` (`Collection.key`
//!   is the `key` attribute with `/children` cut off)
//! - collection.py `moveItem(item, after)`:
//!   `PUT /library/collections/{key}/items/{ratingKey}/move[?after={ratingKey}]`
//! - collection.py `delete` → base.py `PlexPartialObject.delete`:
//!   `DELETE /library/collections/{key}`

use super::{flag, CollectionError, PlexCollections, COLLECTION_TYPE};
use crate::plex::shelf::moves;
use reqwest::Method;
use serde_json::Value;

/// `collectionSort` value for a custom (hand-made) order.
const CUSTOM_SORT: &str = "2";

impl PlexCollections<'_> {
    /// The collection's own metadata row; `None` once it no longer exists.
    async fn collection_row(&self, endpoint: &'static str, collection: &str) -> Result<Option<Value>, CollectionError> {
        match self.send(endpoint, self.request(Method::GET, &format!("/library/metadata/{collection}"))).await {
            Err(CollectionError::Http { status: 404, .. }) => Ok(None),
            Err(error) => Err(error),
            Ok(body) => Ok(body["MediaContainer"]["Metadata"].as_array().and_then(|rows| rows.first()).cloned()),
        }
    }

    /// Renames (and locks the title, so a metadata refresh keeps it), then
    /// reads the title back.
    pub async fn edit_title(&self, section_id: u32, collection: &str, title: &str) -> Result<(), CollectionError> {
        const ENDPOINT: &str = "edit collection title";
        let current = self.collection_row(ENDPOINT, collection).await?;
        if current.as_ref().and_then(|row| row["title"].as_str()) == Some(title) {
            return Ok(());
        }
        if self.dry_run {
            eprintln!("[flinch-arrd] dry-run: would rename plex collection {collection} to {title:?}");
            return Ok(());
        }
        let query = [
            ("type", COLLECTION_TYPE.to_string()),
            ("id", collection.to_string()),
            ("title.value", title.to_string()),
            ("title.locked", "1".to_string()),
        ];
        self.send(ENDPOINT, self.request(Method::PUT, &format!("/library/sections/{section_id}/all")).query(&query)).await?;
        match self.collection_row(ENDPOINT, collection).await? {
            Some(row) if row["title"].as_str() == Some(title) => Ok(()),
            _ => Err(CollectionError::NotApplied { endpoint: ENDPOINT, detail: format!("collection {collection} kept its old title") }),
        }
    }

    /// Puts `wanted` (member ratingKeys) on top in that order, switching the
    /// collection to a custom sort first; members not in `wanted` stay below
    /// in their order. Returns the moves made (or, dry, that would be).
    pub async fn order(&self, collection: &str, wanted: &[String]) -> Result<usize, CollectionError> {
        const ENDPOINT: &str = "order collection";
        let current = self.members(collection).await?;
        let planned = moves(&current, wanted);
        let row = self.collection_row(ENDPOINT, collection).await?;
        let custom = row.as_ref().is_some_and(|row| row["collectionSort"].to_string().trim_matches('"') == CUSTOM_SORT);
        if planned.is_empty() && custom {
            return Ok(0);
        }
        if row.as_ref().is_some_and(|row| flag(&row["smart"])) {
            return Err(CollectionError::Parse {
                endpoint: ENDPOINT,
                detail: format!("collection {collection} is smart and cannot be ordered"),
            });
        }
        if self.dry_run {
            eprintln!("[flinch-arrd] dry-run: would order plex collection {collection} by leave date ({} move(s))", planned.len());
            return Ok(planned.len());
        }
        if !custom {
            let path = format!("/library/collections/{collection}/prefs");
            self.send(ENDPOINT, self.request(Method::PUT, &path).query(&[("collectionSort", CUSTOM_SORT)])).await?;
        }
        for (key, after) in &planned {
            let mut request = self.request(Method::PUT, &format!("/library/collections/{collection}/items/{key}/move"));
            if let Some(after) = after {
                request = request.query(&[("after", after)]);
            }
            self.send(ENDPOINT, request).await?;
        }
        if !moves(&self.members(collection).await?, wanted).is_empty() {
            return Err(CollectionError::NotApplied { endpoint: ENDPOINT, detail: format!("collection {collection} kept its old order") });
        }
        Ok(planned.len())
    }

    /// Deletes the collection if it has no members, then reads back that it
    /// is gone. True when deleted (dry: when it would be). Callers pass only
    /// collections FLINCH itself created.
    pub async fn delete_if_empty(&self, collection: &str) -> Result<bool, CollectionError> {
        const ENDPOINT: &str = "delete empty collection";
        if !self.members(collection).await?.is_empty() {
            return Ok(false);
        }
        if self.dry_run {
            eprintln!("[flinch-arrd] dry-run: would delete empty plex collection {collection}");
            return Ok(true);
        }
        self.send(ENDPOINT, self.request(Method::DELETE, &format!("/library/collections/{collection}"))).await?;
        if self.collection_row(ENDPOINT, collection).await?.is_some() {
            return Err(CollectionError::NotApplied { endpoint: ENDPOINT, detail: format!("collection {collection} still exists") });
        }
        Ok(true)
    }
}

#[cfg(test)]
#[path = "edit_tests.rs"]
mod tests;
