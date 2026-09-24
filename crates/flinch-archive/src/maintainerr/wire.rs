//! Maintainerr's wire format, checked against the v3.29 source: request bodies
//! and what each response means. Kept pure so every rule is table-tested
//! without a server.

use super::{MaintainerrError, MaintainerrTarget};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

pub(super) const STATUS: &str = "GET /api/app/status";
pub(super) const COLLECTIONS: &str = "GET /api/collections";
pub(super) const MEMBERS: &str = "GET /api/collections/media";
pub(super) const EXCLUSIONS: &str = "GET /api/rules/exclusion";
pub(super) const ADD_EXCLUSION: &str = "POST /api/rules/exclusion";
pub(super) const REMOVE_EXCLUSION: &str = "DELETE /api/rules/exclusion/{id}";
pub(super) const ADD_MEMBER: &str = "POST /api/collections/media/add";
pub(super) const REMOVE_MEMBER: &str = "DELETE /api/collections/media";

/// `{type, id}`: which part of `mediaId` an action applies to.
#[derive(Debug, Serialize)]
pub(super) struct Context<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    id: &'a str,
}

fn context(target: &MaintainerrTarget) -> Context<'_> {
    match target {
        MaintainerrTarget::Movie { rating_key } => Context { kind: "movie", id: rating_key },
        MaintainerrTarget::Season { season_rating_key, .. } => Context { kind: "season", id: season_rating_key },
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ExclusionRequest<'a> {
    media_id: &'a str,
    /// Only seasons carry one; without it a global exclusion cascades over the
    /// whole show. There is no `server` field in any Maintainerr version.
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<Context<'a>>,
}

pub(super) fn exclusion_request(target: &MaintainerrTarget) -> ExclusionRequest<'_> {
    ExclusionRequest {
        media_id: target.media_id(),
        context: matches!(target, MaintainerrTarget::Season { .. }).then(|| context(target)),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CollectionAddRequest<'a> {
    /// Always 0 (add). `action: 1` is never sent.
    action: u8,
    collection_id: i64,
    media_id: &'a str,
    context: Context<'a>,
}

pub(super) fn collection_add_request(collection_id: i64, target: &MaintainerrTarget) -> CollectionAddRequest<'_> {
    CollectionAddRequest { action: 0, collection_id, media_id: target.media_id(), context: context(target) }
}

/// One exclusion row as Maintainerr stores it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExclusionRow {
    pub id: i64,
    #[serde(deserialize_with = "key")]
    pub media_server_id: String,
    /// `None` for a global exclusion; set when it is scoped to one rule group.
    #[serde(default)]
    pub rule_group_id: Option<i64>,
    /// The `mediaId` of the call that created the row (the show for a season).
    #[serde(default, deserialize_with = "optional_key")]
    pub parent: Option<String>,
    /// `movie`, `show`, `season` or `episode`. Very old rows may lack it.
    #[serde(default, rename = "type")]
    pub media_type: Option<String>,
}

/// One collection, with only the fields FLINCH validates.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionInfo {
    pub id: i64,
    pub title: String,
    /// `movie`, `show`, `season` or `episode`.
    #[serde(rename = "type")]
    pub media_type: String,
    /// The Plex section the collection is bound to.
    #[serde(deserialize_with = "key")]
    pub library_id: String,
    pub is_active: bool,
    /// Maintainerr's ServarrAction: 0 delete, 1 unmonitor + delete all,
    /// 2 unmonitor + delete existing, 3 unmonitor, 4 nothing, 5 delete the show
    /// when empty.
    pub arr_action: i64,
    /// Days a member waits before Maintainerr acts; unset acts on its next run.
    #[serde(default)]
    pub delete_after_days: Option<i64>,
    /// Whether Plex promotes the collection, so the household sees it.
    #[serde(default)]
    pub visible_on_home: bool,
    #[serde(default)]
    pub visible_on_recommended: bool,
    /// No Plex collection at all: rules and actions run, nothing is shown.
    #[serde(default)]
    pub keep_in_maintainerr_only: bool,
    /// Maintainerr draws its poster overlay (the leave date) on members.
    #[serde(default)]
    pub overlay_enabled: bool,
    /// "Force delete Seerr request": Maintainerr removes the title's Seerr
    /// request when it deletes, instead of waiting for Seerr's own sync.
    #[serde(default)]
    pub force_seerr: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Member {
    #[serde(deserialize_with = "key")]
    media_server_id: String,
}

/// A ratingKey arrives as a string; old rows may carry a number.
#[derive(Deserialize)]
#[serde(untagged)]
enum Key {
    Text(String),
    Number(i64),
}

impl From<Key> for String {
    fn from(key: Key) -> Self {
        match key {
            Key::Text(text) => text,
            Key::Number(number) => number.to_string(),
        }
    }
}

fn key<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Key::deserialize(deserializer).map(String::from)
}

fn optional_key<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(Option::<Key>::deserialize(deserializer)?.map(String::from))
}

/// The running Maintainerr build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaintainerrVersion {
    /// A release build, e.g. `3.29.0`; a pre-release suffix is ignored.
    Release { major: u32, minor: u32, patch: u32 },
    /// A branch build (`main-bd8a1e0`, `development-bd8a1e0`), which tracks a
    /// branch ahead of the latest release.
    Branch(String),
}

impl MaintainerrVersion {
    pub fn parse(raw: &str) -> Self {
        let raw = raw.trim();
        let number = |part: Option<&str>| {
            let part = part?;
            part[..part.find(|c: char| !c.is_ascii_digit()).unwrap_or(part.len())].parse().ok()
        };
        let mut parts = raw.strip_prefix('v').unwrap_or(raw).splitn(3, '.');
        match (number(parts.next()), number(parts.next()), number(parts.next())) {
            (Some(major), Some(minor), Some(patch)) => Self::Release { major, minor, patch },
            _ => Self::Branch(raw.to_string()),
        }
    }
}

impl std::fmt::Display for MaintainerrVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Release { major, minor, patch } => write!(f, "{major}.{minor}.{patch}"),
            Self::Branch(label) => f.write_str(label),
        }
    }
}

/// Maintainerr's `{code, result, message}` status body.
#[derive(Debug, Deserialize)]
struct ReturnStatus {
    code: i64,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    result: Option<String>,
}

/// NestJS's error body: `message` is a string or, for validation, a list.
#[derive(Debug, Deserialize)]
struct ErrorBody {
    #[serde(default)]
    message: serde_json::Value,
}

fn http_error(endpoint: &'static str, status: u16, body: &str) -> MaintainerrError {
    let message = match serde_json::from_str::<ErrorBody>(body).map(|parsed| parsed.message) {
        Ok(serde_json::Value::String(text)) => text,
        Ok(serde_json::Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map_or_else(|| item.to_string(), str::to_string))
            .collect::<Vec<_>>()
            .join("; "),
        _ => body.chars().take(200).collect(),
    };
    MaintainerrError::Http { endpoint, status, message }
}

fn success(status: u16) -> bool {
    (200..300).contains(&status)
}

/// A JSON read. A non-2xx and an unparseable body are both errors; an empty
/// body (Maintainerr's `undefined` after a failed query) does not parse.
pub(super) fn read_json<T: DeserializeOwned>(endpoint: &'static str, status: u16, body: &str) -> Result<T, MaintainerrError> {
    if !success(status) {
        return Err(http_error(endpoint, status, body));
    }
    serde_json::from_str(body).map_err(|source| MaintainerrError::Parse { endpoint, source })
}

pub(super) fn read_members(status: u16, body: &str) -> Result<Vec<String>, MaintainerrError> {
    Ok(read_json::<Vec<Member>>(MEMBERS, status, body)?.into_iter().map(|member| member.media_server_id).collect())
}

/// `/api/app/status` answers the JSON text of `{version, …}`. A build that
/// encodes that text once more as a JSON string is read too.
pub(super) fn read_version(status: u16, body: &str) -> Result<MaintainerrVersion, MaintainerrError> {
    #[derive(Deserialize)]
    struct Status {
        version: String,
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Shape {
        Object(Status),
        Encoded(String),
    }
    let status = match read_json::<Shape>(STATUS, status, body)? {
        Shape::Object(status) => status,
        Shape::Encoded(text) => serde_json::from_str::<Status>(&text)
            .map_err(|source| MaintainerrError::Parse { endpoint: STATUS, source })?,
    };
    Ok(MaintainerrVersion::parse(&status.version))
}

/// Success means a 2xx AND `code == 1`. Maintainerr answers 201 with code 0
/// when the call failed (`Failed - no metadata`), so a status code alone
/// proves nothing.
pub(super) fn read_return_status(endpoint: &'static str, status: u16, body: &str) -> Result<(), MaintainerrError> {
    let parsed: ReturnStatus = read_json(endpoint, status, body)?;
    if parsed.code == 1 {
        return Ok(());
    }
    let message = parsed.message.or(parsed.result).unwrap_or_default();
    Err(MaintainerrError::Refused { endpoint, code: parsed.code, message })
}

/// A write whose effect a later read-back verifies: any 2xx is accepted.
/// `POST /api/collections/media/add` answers 201 on success; 400 (the item
/// does not fit the collection), 404 (no such collection) and 502 (Plex
/// refused or did not answer) are failures that carry `message`.
pub(super) fn read_accepted(endpoint: &'static str, status: u16, body: &str) -> Result<(), MaintainerrError> {
    if success(status) {
        return Ok(());
    }
    Err(http_error(endpoint, status, body))
}
