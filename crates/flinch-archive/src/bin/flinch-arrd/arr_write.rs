//! Writes to Radarr and Sonarr, and the reads that verify them. A dry run
//! prints every write instead of sending it, so it plans exactly what an
//! enforcing run would and changes nothing. The key travels in the
//! `X-Api-Key` header only, never in a url or a message; redirects are
//! refused (the client follows none, so a key never leaves with one).

use super::fetch::{fetch_json, refuse_redirect};
use anyhow::{Context, Result};
use flinch_archive::arr::instances::Connection;
use flinch_archive::body;
use flinch_archive::capacity::App;
use reqwest::Method;

pub(super) struct ArrWriter<'a> {
    http: &'a reqwest::Client,
    arr: &'a Connection,
    dry_run: bool,
}

impl<'a> ArrWriter<'a> {
    /// The instance's writer; `None` when its url or key is not configured.
    pub(super) fn new(http: &'a reqwest::Client, arr: &'a Connection, dry_run: bool) -> Option<Self> {
        (!arr.base.is_empty() && !arr.key.is_empty()).then_some(Self { http, arr, dry_run })
    }

    pub(super) fn app(&self) -> App {
        self.arr.app
    }

    /// The instance's name; empty for the default.
    pub(super) fn instance(&self) -> &'a str {
        &self.arr.name
    }

    /// `Radarr`, or `Radarr 4k` for a named instance, for messages.
    pub(super) fn name(&self) -> String {
        self.arr.label()
    }

    /// Read `path` (`/api/v3/...`), live in a dry run too.
    pub(super) async fn get(&self, path: &str) -> Result<serde_json::Value> {
        fetch_json(self.http, &format!("{}{path}", self.arr.base), &self.arr.key).await
    }

    /// Send `body` to `path`; `None` in a dry run, which only prints it.
    pub(super) async fn send(&self, method: Method, path: &str, body: &serde_json::Value) -> Result<Option<serde_json::Value>> {
        if self.dry_run {
            println!("[dry-run] would {method} {} {path} {body}", self.name());
            return Ok(None);
        }
        let response = self
            .http
            .request(method.clone(), format!("{}{path}", self.arr.base))
            .header("X-Api-Key", &self.arr.key)
            .json(body)
            .send()
            .await
            .with_context(|| format!("{} {method} {path}: no answer", self.name()))?;
        let status = response.status();
        refuse_redirect(status)?;
        if !status.is_success() {
            // The body says why ("validation failed: ..."); it never echoes the key.
            let reason = body::read_text(response).await.unwrap_or_default();
            anyhow::bail!("{} {method} {path}: HTTP {status}: {}", self.name(), reason.chars().take(200).collect::<String>().trim());
        }
        let bytes = body::read(response).await.context("answer unreadable")?;
        if bytes.is_empty() {
            return Ok(Some(serde_json::Value::Null));
        }
        serde_json::from_slice(&bytes).map(Some).context("answer was not JSON")
    }
}
