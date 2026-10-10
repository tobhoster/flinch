//! What the sync reads from and writes to Radarr and Sonarr: custom formats,
//! quality profiles and quality definitions over `/api/v3` (shapes from each
//! app's OpenAPI spec: <https://github.com/Radarr/Radarr/blob/develop/src/Radarr.Api.V3/openapi.json>,
//! <https://github.com/Sonarr/Sonarr/blob/develop/src/Sonarr.Api.V3/openapi.json>).
//!
//! Profiles and definitions are kept raw beside their typed view: an update
//! patches the object the app served, so a field this code does not know
//! survives the round trip.

use crate::capacity::App;
use reqwest::Method;
use serde::Deserialize;
use serde_json::Value;

fn null_as_default<'de, D: serde::Deserializer<'de>, T: Default + Deserialize<'de>>(deserializer: D) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArrCustomFormat {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub include_custom_format_when_renaming: bool,
    #[serde(default)]
    pub specifications: Vec<ArrSpecification>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ArrSpecification {
    pub name: String,
    pub implementation: String,
    #[serde(default)]
    pub negate: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub fields: Vec<ArrField>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ArrField {
    pub name: String,
    #[serde(default)]
    pub value: Value,
}

/// Every field reads `null` as its default: the schema a new profile starts
/// from leaves most of them unset.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArrProfile {
    #[serde(default, deserialize_with = "null_as_default")]
    pub id: u32,
    #[serde(default, deserialize_with = "null_as_default")]
    pub name: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub upgrade_allowed: bool,
    #[serde(default, deserialize_with = "null_as_default")]
    pub cutoff: i64,
    /// Worst first, as the API lists them.
    #[serde(default, deserialize_with = "null_as_default")]
    pub items: Vec<ArrProfileItem>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub min_format_score: i32,
    #[serde(default, deserialize_with = "null_as_default")]
    pub cutoff_format_score: i32,
    #[serde(default, deserialize_with = "null_as_default")]
    pub min_upgrade_format_score: i32,
    #[serde(default, deserialize_with = "null_as_default")]
    pub format_items: Vec<ArrFormatItem>,
    /// Radarr only.
    #[serde(default)]
    pub language: Option<ArrLanguage>,
}

/// A quality (with `quality` set) or a group (with `id`, `name` and `items`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ArrProfileItem {
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub quality: Option<ArrQuality>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub items: Vec<ArrProfileItem>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub allowed: bool,
}

impl ArrProfileItem {
    /// The name a ladder and a cutoff use: the group's, or its quality's.
    pub fn label(&self) -> &str {
        match (&self.quality, &self.name) {
            (Some(quality), _) => &quality.name,
            (None, Some(name)) => name,
            (None, None) => "",
        }
    }

    /// The id a cutoff points at: the group's, or its quality's.
    pub fn cutoff_id(&self) -> Option<i64> {
        self.quality.as_ref().map(|quality| quality.id).or(self.id)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ArrQuality {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ArrFormatItem {
    pub format: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub score: i32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ArrLanguage {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArrQualityDefinition {
    pub id: u32,
    pub quality: ArrQuality,
    #[serde(default)]
    pub min_size: Option<f64>,
    /// `null` means unlimited.
    #[serde(default)]
    pub max_size: Option<f64>,
    #[serde(default)]
    pub preferred_size: Option<f64>,
}

/// A typed view beside the object it was read from.
#[derive(Debug, Clone, PartialEq)]
pub struct Raw<T> {
    pub typed: T,
    pub raw: Value,
}

/// Everything one app's sync compares against, read in one pass.
#[derive(Debug, Clone, PartialEq)]
pub struct Live {
    pub custom_formats: Vec<ArrCustomFormat>,
    pub profiles: Vec<Raw<ArrProfile>>,
    /// `/api/v3/qualityprofile/schema`: a new profile with every quality.
    pub schema: Raw<ArrProfile>,
    pub definitions: Vec<Raw<ArrQualityDefinition>>,
    /// Radarr's languages, for a profile's language; empty for Sonarr.
    pub languages: Vec<ArrLanguage>,
}

/// Why an *arr call failed. The API key travels in a header, so no variant
/// can carry it; the app's own reason is kept, shortened.
#[derive(Debug, thiserror::Error)]
pub enum ArrError {
    #[error("{app} {method} {path}: request failed: {source}")]
    Transport { app: &'static str, method: Method, path: String, source: reqwest::Error },
    #[error("{app} {method} {path}: HTTP {status}{reason}")]
    Http { app: &'static str, method: Method, path: String, status: u16, reason: String },
    #[error("{app} {method} {path}: {source}")]
    Body { app: &'static str, method: Method, path: String, source: crate::body::BodyError },
    #[error("{app} {path}: unexpected answer: {source}")]
    Parse { app: &'static str, path: String, source: serde_json::Error },
}

/// One instance's `/api/v3`, through the daemon's client (which follows no
/// redirect: a 3xx is an error naming its status, and the key stays home).
pub struct ArrClient<'a> {
    http: &'a reqwest::Client,
    app: App,
    /// The instance ([`crate::ids`]); empty for the default.
    instance: &'a str,
    base: String,
    key: &'a str,
}

impl<'a> ArrClient<'a> {
    pub fn new(http: &'a reqwest::Client, app: App, base: &str, key: &'a str) -> Self {
        Self { http, app, instance: crate::ids::DEFAULT_INSTANCE, base: base.trim_end_matches('/').to_string(), key }
    }

    /// The same client, for the named instance its url and key reach.
    pub fn named(self, instance: &'a str) -> Self {
        Self { instance, ..self }
    }

    pub fn app(&self) -> App {
        self.app
    }

    pub fn instance(&self) -> &'a str {
        self.instance
    }

    /// Send one request; the answer's JSON, or `Value::Null` for an empty body.
    pub async fn call(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value, ArrError> {
        let app = self.app.label();
        let mut request = self.http.request(method.clone(), format!("{}{path}", self.base)).header("X-Api-Key", self.key);
        if let Some(body) = body {
            request = request.json(body);
        }
        let failed = |source: reqwest::Error| ArrError::Transport {
            app,
            method: method.clone(),
            path: path.to_string(),
            source: source.without_url(),
        };
        let response = request.send().await.map_err(failed)?;
        let status = response.status();
        let text = crate::body::read_text(response).await.map_err(|source| ArrError::Body {
            app,
            method: method.clone(),
            path: path.to_string(),
            source,
        })?;
        if !status.is_success() {
            let reason: String = text.chars().take(300).collect();
            let reason = if reason.trim().is_empty() { String::new() } else { format!(": {}", reason.trim()) };
            return Err(ArrError::Http { app, method, path: path.to_string(), status: status.as_u16(), reason });
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|source| ArrError::Parse { app, path: path.to_string(), source })
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<(T, Value), ArrError> {
        let raw = self.call(Method::GET, path, None).await?;
        let typed = serde_json::from_value(raw.clone()).map_err(|source| ArrError::Parse {
            app: self.app.label(),
            path: path.to_string(),
            source,
        })?;
        Ok((typed, raw))
    }

    async fn get_rows<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<Vec<Raw<T>>, ArrError> {
        let (rows, _): (Vec<Value>, _) = self.get(path).await?;
        rows.into_iter()
            .map(|raw| {
                let typed = serde_json::from_value(raw.clone()).map_err(|source| ArrError::Parse {
                    app: self.app.label(),
                    path: path.to_string(),
                    source,
                })?;
                Ok(Raw { typed, raw })
            })
            .collect()
    }

    pub async fn custom_formats(&self) -> Result<Vec<ArrCustomFormat>, ArrError> {
        Ok(self.get("/api/v3/customformat").await?.0)
    }

    /// Every resource the diff needs. Any read failing fails the app: a diff
    /// against half a picture would offer to create what already exists.
    pub async fn read_live(&self) -> Result<Live, ArrError> {
        let (schema, schema_raw) = self.get("/api/v3/qualityprofile/schema").await?;
        let languages = match self.app {
            App::Radarr => self.get("/api/v3/language").await?.0,
            App::Sonarr => Vec::new(),
        };
        Ok(Live {
            custom_formats: self.custom_formats().await?,
            profiles: self.get_rows("/api/v3/qualityprofile").await?,
            schema: Raw { typed: schema, raw: schema_raw },
            definitions: self.get_rows("/api/v3/qualitydefinition").await?,
            languages,
        })
    }
}
