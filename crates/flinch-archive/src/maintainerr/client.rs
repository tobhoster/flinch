//! The live Maintainerr client.

use super::wire;
use super::{CollectionInfo, ExclusionRow, MaintainerrApi, MaintainerrError, MaintainerrTarget, MaintainerrVersion};
use reqwest::{Method, RequestBuilder};
use std::time::Duration;

/// Bounds a hung server on every call except the exclusion POST.
const TIMEOUT: Duration = Duration::from_secs(15);
/// `POST /api/rules/exclusion` waits up to 30 s for Maintainerr's execution
/// lock before it answers 409, then reads Plex metadata for the season and
/// each of its episodes. The client outwaits both: a POST that times out may
/// still land, and its row would then read as the operator's.
const EXCLUSION_TIMEOUT: Duration = Duration::from_secs(60);

pub struct HttpMaintainerr {
    base_url: String,
    api_key: Option<String>,
    http: reqwest::Client,
}

impl HttpMaintainerr {
    /// The API key is optional because Maintainerr checks none. When one is
    /// set it is sent as `X-Api-Key` (Maintainerr's own header), so a proxy in
    /// front of Maintainerr can check it. Redirects are not followed, so the
    /// key never reaches another host: a 3xx reads as an HTTP error.
    pub fn new(base_url: &str, api_key: &str) -> Result<Self, MaintainerrError> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|source| MaintainerrError::Transport { endpoint: "client setup", source })?;
        let api_key = api_key.trim();
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: (!api_key.is_empty()).then(|| api_key.to_string()),
            http,
        })
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let request = self
            .http
            .request(method, format!("{}{path}", self.base_url))
            .header(reqwest::header::ACCEPT, "application/json");
        match &self.api_key {
            Some(key) => request.header("X-Api-Key", key),
            None => request,
        }
    }
}

/// Send a request and return its status and body text.
async fn send(endpoint: &'static str, request: RequestBuilder) -> Result<(u16, String), MaintainerrError> {
    let response = request.send().await.map_err(|source| MaintainerrError::Transport { endpoint, source })?;
    let status = response.status().as_u16();
    let body = crate::body::read_text(response).await.map_err(|source| MaintainerrError::Body { endpoint, source })?;
    Ok((status, body))
}

impl MaintainerrApi for HttpMaintainerr {
    async fn version(&mut self) -> Result<MaintainerrVersion, MaintainerrError> {
        let (status, body) = send(wire::STATUS, self.request(Method::GET, "/api/app/status")).await?;
        wire::read_version(status, &body)
    }

    async fn collections(&mut self) -> Result<Vec<CollectionInfo>, MaintainerrError> {
        let (status, body) = send(wire::COLLECTIONS, self.request(Method::GET, "/api/collections")).await?;
        wire::read_json(wire::COLLECTIONS, status, &body)
    }

    async fn collection_members(&mut self, collection_id: i64) -> Result<Vec<String>, MaintainerrError> {
        let request = self.request(Method::GET, "/api/collections/media/").query(&[("collectionId", collection_id)]);
        let (status, body) = send(wire::MEMBERS, request).await?;
        wire::read_members(status, &body)
    }

    async fn exclusions(&mut self, media_id: &str) -> Result<Vec<ExclusionRow>, MaintainerrError> {
        let request = self.request(Method::GET, "/api/rules/exclusion").query(&[("mediaServerId", media_id)]);
        let (status, body) = send(wire::EXCLUSIONS, request).await?;
        wire::read_json(wire::EXCLUSIONS, status, &body)
    }

    async fn add_exclusion(&mut self, target: &MaintainerrTarget) -> Result<(), MaintainerrError> {
        let request = self
            .request(Method::POST, "/api/rules/exclusion")
            .timeout(EXCLUSION_TIMEOUT)
            .json(&wire::exclusion_request(target));
        let (status, body) = send(wire::ADD_EXCLUSION, request).await?;
        wire::read_return_status(wire::ADD_EXCLUSION, status, &body)
    }

    async fn remove_exclusion(&mut self, exclusion_id: i64) -> Result<(), MaintainerrError> {
        let request = self.request(Method::DELETE, &format!("/api/rules/exclusion/{exclusion_id}"));
        let (status, body) = send(wire::REMOVE_EXCLUSION, request).await?;
        wire::read_return_status(wire::REMOVE_EXCLUSION, status, &body)
    }

    async fn add_to_collection(&mut self, collection_id: i64, target: &MaintainerrTarget) -> Result<(), MaintainerrError> {
        let request = self
            .request(Method::POST, "/api/collections/media/add")
            .json(&wire::collection_add_request(collection_id, target));
        let (status, body) = send(wire::ADD_MEMBER, request).await?;
        wire::read_accepted(wire::ADD_MEMBER, status, &body)
    }

    async fn remove_from_collection(&mut self, collection_id: i64, item_key: &str) -> Result<(), MaintainerrError> {
        let collection = collection_id.to_string();
        let request = self
            .request(Method::DELETE, "/api/collections/media")
            .query(&[("mediaId", item_key), ("collectionId", collection.as_str())]);
        let (status, body) = send(wire::REMOVE_MEMBER, request).await?;
        wire::read_accepted(wire::REMOVE_MEMBER, status, &body)
    }
}
