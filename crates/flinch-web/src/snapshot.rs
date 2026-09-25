//! The published library (`items.json`) for `/v1/systemone`: parsed once per
//! version of the file, off the async runtime, and shared by every request
//! until the daemon publishes a new one.

use flinch_archive::ItemSnapshot;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::Mutex;

/// One version of the file. The daemon replaces it whole (temp file, then
/// rename), so a new snapshot changes its modified time or its length.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Version {
    modified: SystemTime,
    len: u64,
}

/// One version's items; `None` when that version did not parse.
type Items = Option<Arc<Vec<ItemSnapshot>>>;

pub struct Snapshot {
    path: Arc<Path>,
    cached: Mutex<Option<(Version, Items)>>,
}

enum Read {
    Unchanged,
    Changed(Version, Items),
    /// Missing or unreadable: there is no snapshot to answer from.
    Unavailable,
}

impl Snapshot {
    pub fn new(path: PathBuf) -> Self {
        Self { path: Arc::from(path), cached: Mutex::new(None) }
    }

    /// The published items, or `None` while the file is missing, unreadable
    /// or not a snapshot. The file is read and parsed only when its version
    /// changed since the last request.
    pub async fn items(&self) -> Items {
        // One reader at a time: requests arriving after a publish wait for one
        // parse instead of each running their own.
        let mut cached = self.cached.lock().await;
        let known = cached.as_ref().map(|(version, _)| *version);
        let path = Arc::clone(&self.path);
        match tokio::task::spawn_blocking(move || read(&path, known)).await {
            Ok(Read::Unchanged) => cached.as_ref().and_then(|(_, items)| items.clone()),
            Ok(Read::Changed(version, items)) => {
                *cached = Some((version, items.clone()));
                items
            }
            // A read that could not finish answers like a missing file.
            Ok(Read::Unavailable) | Err(_) => {
                *cached = None;
                None
            }
        }
    }
}

fn read(path: &Path, known: Option<Version>) -> Read {
    let Some(version) = std::fs::metadata(path).and_then(|meta| Ok(Version { modified: meta.modified()?, len: meta.len() })).ok() else {
        return Read::Unavailable;
    };
    if known == Some(version) {
        return Read::Unchanged;
    }
    match std::fs::read(path) {
        Ok(bytes) => Read::Changed(version, serde_json::from_slice(&bytes).ok().map(Arc::new)),
        Err(_) => Read::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use crate::tests::{body_of, scratch, state, TOKEN};
    use axum::extract::State;
    use axum::http::StatusCode;
    use std::fs;

    async fn decision(st: &crate::AppState, id: &str) -> (StatusCode, String) {
        let ask = format!(r#"{{"state": "{id}", "questions": {{"decision": {{"type": "choice", "criteria": ["keep", "delete"]}}}}}}"#);
        let response = crate::api_systemone(State(st.clone()), ask).await;
        (response.status(), body_of(response).await)
    }

    #[tokio::test]
    async fn systemone_answers_from_the_items_json_published_last() {
        let tmp = scratch("snapshot");
        let st = state(&tmp, Some(TOKEN));
        let items = st.dir.join("items.json");
        assert_eq!(decision(&st, "radarr-1").await.0, StatusCode::OK);

        fs::write(&items, r#"[{"id":"radarr-22","title":"Another Movie","kind":"movie","size_bytes":2000000000,"decision":"delete","reason":"watched","delete_probability":0.99,"protected":false}]"#).unwrap();
        let (status, body) = decision(&st, "radarr-22").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let answer: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(answer["answers"]["decision"]["choice"], "delete");
        assert_eq!(decision(&st, "radarr-1").await.0, StatusCode::BAD_REQUEST, "an item the new snapshot dropped is unknown");

        fs::remove_file(&items).unwrap();
        assert_eq!(decision(&st, "radarr-22").await.0, StatusCode::SERVICE_UNAVAILABLE, "no snapshot, no answer");
        fs::remove_dir_all(&tmp).ok();
    }
}
