//! One request path for the native executor's clients (Radarr, Sonarr, Seerr):
//! the API key in a header, no redirects followed (the caller's client is
//! built that way), URLs stripped from errors, and every write printed instead
//! of sent in a dry run.

use reqwest::{Method, RequestBuilder};
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    /// The URL is stripped: it may carry the host, and a token in some setups.
    #[error("{endpoint}: request failed: {source}")]
    Transport { endpoint: &'static str, source: reqwest::Error },
    #[error("{endpoint}: {source}")]
    Body { endpoint: &'static str, source: crate::body::BodyError },
    /// Any non-2xx, redirects included (a key would travel with one). The body
    /// carries the app's reason ("database is locked").
    #[error("{endpoint}: HTTP {status}{}", if detail.is_empty() { String::new() } else { format!(": {detail}") })]
    Http { endpoint: &'static str, status: u16, detail: String },
    #[error("{endpoint}: unexpected response: {detail}")]
    Parse { endpoint: &'static str, detail: String },
    /// The app accepted the write but the read-back does not show it.
    #[error("{endpoint}: not applied: {detail}")]
    NotApplied { endpoint: &'static str, detail: String },
}

/// One app's API: `X-Api-Key` auth, JSON both ways.
pub(crate) struct Api<'a> {
    http: &'a reqwest::Client,
    base: &'a str,
    key: &'a str,
    app: &'static str,
    pub(crate) dry_run: bool,
}

impl<'a> Api<'a> {
    pub(crate) fn new(http: &'a reqwest::Client, base: &'a str, key: &'a str, app: &'static str, dry_run: bool) -> Self {
        Self { http, base: base.trim_end_matches('/'), key: key.trim(), app, dry_run }
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.http.request(method, format!("{}{path}", self.base)).header("X-Api-Key", self.key).header("Accept", "application/json")
    }

    /// Status and parsed body (`Null` when empty) of any answer.
    async fn send(&self, endpoint: &'static str, request: RequestBuilder) -> Result<(u16, Value), ExecutorError> {
        let response = request.send().await.map_err(|source| ExecutorError::Transport { endpoint, source: source.without_url() })?;
        let status = response.status().as_u16();
        let body = crate::body::read_text(response).await.map_err(|source| ExecutorError::Body { endpoint, source })?;
        if !(200..300).contains(&status) {
            let detail = body.chars().take(200).collect::<String>().trim().to_string();
            return Err(ExecutorError::Http { endpoint, status, detail });
        }
        if body.trim().is_empty() {
            return Ok((status, Value::Null));
        }
        let value = serde_json::from_str(&body).map_err(|error| ExecutorError::Parse { endpoint, detail: error.to_string() })?;
        Ok((status, value))
    }

    /// A read; reads go out in a dry run too, so it plans what a live run would.
    pub(crate) async fn get(&self, endpoint: &'static str, path: &str, query: &[(&str, String)]) -> Result<Value, ExecutorError> {
        Ok(self.send(endpoint, self.request(Method::GET, path).query(query)).await?.1)
    }

    /// A read where 404 means "not there" rather than a failure.
    pub(crate) async fn find(&self, endpoint: &'static str, path: &str) -> Result<Option<Value>, ExecutorError> {
        match self.get(endpoint, path, &[]).await {
            Ok(value) => Ok(Some(value)),
            Err(ExecutorError::Http { status: 404, .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// A write: sent live, printed in a dry run (`Ok(None)` then). The body is
    /// printed too; none of these bodies carries a secret.
    pub(crate) async fn write(
        &self,
        endpoint: &'static str,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Option<Value>, ExecutorError> {
        if self.dry_run {
            let query = query.iter().map(|(name, value)| format!("{name}={value}")).collect::<Vec<_>>().join("&");
            let query = if query.is_empty() { String::new() } else { format!("?{query}") };
            let body = body.map(|body| format!(" {body}")).unwrap_or_default();
            println!("[dry-run] would send {} {method} {path}{query}{body}", self.app);
            return Ok(None);
        }
        let mut request = self.request(method, path).query(query);
        if let Some(body) = body {
            request = request.json(body);
        }
        Ok(Some(self.send(endpoint, request).await?.1))
    }
}

/// A JSON number as u64, whether the app sent it as a number or a string.
pub(crate) fn as_u64(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}
