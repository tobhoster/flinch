//! Ollama: the server behind the taste vectors and the taste judge.
//!
//! Two endpoints, both JSON over one base URL (a local Ollama by default, or
//! ollama.com with an API key):
//!
//! - `POST /api/embed` turns texts into vectors
//!   (<https://github.com/ollama/ollama/blob/main/docs/api.md#generate-embeddings>):
//!   `{"model", "input": [...], "truncate": true}` in,
//!   `{"embeddings": [[...], ...]}` out, one vector per text in order.
//! - `POST /v1/systemone` asks a decision model such as Clef typed questions
//!   about a `state`: `{"model", "state", "questions": {name: {"type":
//!   "noul", "instructions"}}}` in, `{"answers": {name: {"type": "noul",
//!   "noul": p}}}` out. Only yes/no ("noul") questions are asked here; the
//!   answer is the probability of yes.
//!
//! The key, when one is set, goes in `Authorization: Bearer`; a local Ollama
//! needs none. No error carries the URL's path, query or credentials, nor the
//! key: messages name the server by its origin (`http://host:port`).

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

/// Where Ollama listens unless the operator says otherwise.
pub const DEFAULT_URL: &str = "http://localhost:11434";
/// One batch of texts: an embedding model on CPU takes seconds per batch, and
/// a model Ollama has to load first takes longer.
const EMBED_TIMEOUT: Duration = Duration::from_secs(60);
/// One decision: Clef is a 27B model, and on CPU a single answer can take a
/// minute or more.
const DECIDE_TIMEOUT: Duration = Duration::from_secs(120);
const EMBED: &str = "/api/embed";
const DECIDE: &str = "/v1/systemone";

/// The operator's Ollama (`settings.json` `ollama`). Every field defaults
/// individually.
///
/// The key is either typed into Settings (`token`, never sent back to the
/// browser) or named by environment variable (`api_key_env`), so it can live in
/// a secret instead of `settings.json`. A local Ollama needs neither.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OllamaConfig {
    pub url: String,
    pub token: String,
    pub api_key_env: String,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self { url: DEFAULT_URL.to_string(), token: String::new(), api_key_env: String::new() }
    }
}

/// An [`OllamaConfig`] outside its bounds; the message names the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidOllamaConfig(pub &'static str);

impl OllamaConfig {
    pub fn validate(&self) -> Result<(), InvalidOllamaConfig> {
        let env = self.api_key_env.trim();
        if !env.is_empty() && !env.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(InvalidOllamaConfig("the Ollama API key variable (ollama.api_key_env) must be letters, digits and _"));
        }
        let url = self.url.trim();
        if url.chars().any(char::is_whitespace) {
            return Err(InvalidOllamaConfig("the Ollama URL (ollama.url) must not contain spaces"));
        }
        match reqwest::Url::parse(url) {
            Ok(parsed) if matches!(parsed.scheme(), "http" | "https") && parsed.has_host() => Ok(()),
            _ => Err(InvalidOllamaConfig("the Ollama URL (ollama.url) must be an http:// or https:// address")),
        }
    }

    /// The API key: the Settings token, else the named variable. `None` when
    /// neither holds one.
    pub fn api_key(&self) -> Option<String> {
        let token = self.token.trim();
        if !token.is_empty() {
            return Some(token.to_string());
        }
        let env = self.api_key_env.trim();
        (!env.is_empty()).then(|| std::env::var(env).ok()).flatten().map(|key| key.trim().to_string()).filter(|key| !key.is_empty())
    }

    /// The server as messages name it: scheme, host and port only.
    pub fn origin(&self) -> String {
        match reqwest::Url::parse(self.url.trim()) {
            Ok(url) if url.has_host() => url.origin().ascii_serialization(),
            _ => "the configured URL".to_string(),
        }
    }
}

/// One typed question.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// Yes or no: the answer is the probability of yes.
    Noul { instructions: String },
}

/// One question's answer.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    Noul { noul: f64 },
}

/// Named questions; a `BTreeMap` so the request body is the same every time.
pub type Questions = BTreeMap<String, Question>;
/// Every asked question's answer, by name.
pub type Answers = BTreeMap<String, Answer>;

/// What went wrong asking Ollama.
#[derive(Debug, thiserror::Error)]
pub enum OllamaError {
    #[error("the Ollama URL (ollama.url) is not an http:// or https:// address")]
    BadUrl,
    #[error("Ollama at {at} is unreachable: {detail}")]
    Unreachable { at: String, detail: String },
    #[error("Ollama at {at} has no model {model}: run `ollama pull {model}`")]
    NoModel { at: String, model: String },
    #[error("Ollama at {at} does not serve {endpoint}: update Ollama")]
    NoEndpoint { at: String, endpoint: &'static str },
    #[error(
        "Ollama at {at} refused the request (HTTP {status}): set its API key in Settings (ollama.token) or name the variable holding it (ollama.api_key_env)"
    )]
    Unauthorized { at: String, status: u16 },
    #[error("Ollama at {at} answered {endpoint} with HTTP {status}: {message}")]
    Status { at: String, endpoint: &'static str, status: u16, message: String },
    #[error("Ollama at {at}, {endpoint}: {source}")]
    Body { at: String, endpoint: &'static str, source: crate::body::BodyError },
    #[error("Ollama at {at} sent an unusable {endpoint} answer: {detail}")]
    Malformed { at: String, endpoint: &'static str, detail: String },
}

/// One vector per text, in order, from `model`. The texts are cut to the
/// model's context by Ollama (`truncate`), never refused for length.
pub async fn embed(http: &reqwest::Client, config: &OllamaConfig, model: &str, texts: &[&str]) -> Result<Vec<Vec<f32>>, OllamaError> {
    #[derive(Serialize)]
    struct Request<'a> {
        model: &'a str,
        input: &'a [&'a str],
        truncate: bool,
    }
    #[derive(Deserialize)]
    struct Response {
        embeddings: Vec<Vec<f32>>,
    }
    let body = Request { model, input: texts, truncate: true };
    let response: Response = post(http, config, EMBED, model, &body, EMBED_TIMEOUT).await?;
    if response.embeddings.len() != texts.len() {
        let detail = format!("{} vectors for {} texts", response.embeddings.len(), texts.len());
        return Err(OllamaError::Malformed { at: config.origin(), endpoint: EMBED, detail });
    }
    Ok(response.embeddings)
}

/// `model`'s answers to `questions` about `state`. Every question asked has
/// an answer, of the type asked and in range, or the call is an error.
pub async fn decide(
    http: &reqwest::Client,
    config: &OllamaConfig,
    model: &str,
    state: &serde_json::Value,
    questions: &Questions,
) -> Result<Answers, OllamaError> {
    #[derive(Serialize)]
    struct Request<'a> {
        model: &'a str,
        state: &'a serde_json::Value,
        questions: &'a Questions,
    }
    #[derive(Deserialize)]
    struct Response {
        answers: Answers,
    }
    let body = Request { model, state, questions };
    let mut response: Response = post(http, config, DECIDE, model, &body, DECIDE_TIMEOUT).await?;
    let mut answers = Answers::new();
    for name in questions.keys() {
        let malformed = |detail: String| OllamaError::Malformed { at: config.origin(), endpoint: DECIDE, detail };
        match response.answers.remove(name) {
            Some(Answer::Noul { noul }) if (0.0..=1.0).contains(&noul) => answers.insert(name.clone(), Answer::Noul { noul }),
            Some(Answer::Noul { noul }) => return Err(malformed(format!("{name} answered {noul}, not a probability"))),
            None => return Err(malformed(format!("no answer to {name}"))),
        };
    }
    Ok(answers)
}

/// POST `body` to `endpoint`, with the key when one is set, and read the answer.
async fn post<B: Serialize, T: DeserializeOwned>(
    http: &reqwest::Client,
    config: &OllamaConfig,
    endpoint: &'static str,
    model: &str,
    body: &B,
    timeout: Duration,
) -> Result<T, OllamaError> {
    let at = config.origin();
    let mut request = http.post(endpoint_url(config, endpoint)?).timeout(timeout).json(body);
    if let Some(key) = config.api_key() {
        request = request.bearer_auth(key);
    }
    let response = request.send().await.map_err(|error| OllamaError::Unreachable { at: at.clone(), detail: transport(&error, timeout) })?;
    let status = response.status().as_u16();
    let text = crate::body::read_text(response).await.map_err(|source| OllamaError::Body { at: at.clone(), endpoint, source })?;
    if !(200..300).contains(&status) {
        return Err(refusal(at, endpoint, model, status, &text));
    }
    serde_json::from_str(&text).map_err(|error| OllamaError::Malformed { at, endpoint, detail: error.to_string() })
}

/// `endpoint` under the configured base URL's path; the base's query and
/// fragment are dropped, so nothing of them travels or is reported.
fn endpoint_url(config: &OllamaConfig, endpoint: &str) -> Result<reqwest::Url, OllamaError> {
    let mut url = reqwest::Url::parse(config.url.trim()).map_err(|_| OllamaError::BadUrl)?;
    if !matches!(url.scheme(), "http" | "https") || !url.has_host() {
        return Err(OllamaError::BadUrl);
    }
    let path = format!("{}{endpoint}", url.path().trim_end_matches('/'));
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

/// Why a request never got an answer, in words and without the URL: the
/// innermost cause ("Connection refused") says more than reqwest's own line.
fn transport(error: &reqwest::Error, timeout: Duration) -> String {
    if error.is_timeout() {
        return format!("no answer within {} s", timeout.as_secs());
    }
    let mut cause: &dyn std::error::Error = error;
    while let Some(inner) = cause.source() {
        cause = inner;
    }
    if std::ptr::addr_eq(cause, error) {
        return "the request failed".to_string();
    }
    cause.to_string()
}

/// A non-2xx answer as the error that says what to do about it. Ollama
/// explains itself in `{"error": "..."}`; a missing model is a 404 naming it.
fn refusal(at: String, endpoint: &'static str, model: &str, status: u16, body: &str) -> OllamaError {
    #[derive(Deserialize)]
    struct Explained {
        error: String,
    }
    let message = serde_json::from_str::<Explained>(body).map(|explained| explained.error).unwrap_or_else(|_| body.trim().to_string());
    match status {
        401 | 403 => OllamaError::Unauthorized { at, status },
        404 if message.contains("model") && message.contains("not found") => OllamaError::NoModel { at, model: model.to_string() },
        404 => OllamaError::NoEndpoint { at, endpoint },
        _ => OllamaError::Status { at, endpoint, status, message: message.chars().take(300).collect() },
    }
}

#[cfg(test)]
mod tests;
