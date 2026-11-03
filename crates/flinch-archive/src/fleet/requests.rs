//! The fleet actions the web UI asks for. The *arr and Prowlarr keys live
//! with the daemon, so the page never calls the apps: it drops one empty
//! file per request under `fleet-requests/` on the state volume (like
//! `restore/<card id>`), and the daemon carries it out at the top of its next
//! cycle, honouring the dry run, then deletes it.
//!
//! - `fleet-requests/queue-remove.<instance>-<queue id>`: remove a stuck
//!   queue item and blocklist its release (`sonarr`, `radarr@4k`, …).
//! - `fleet-requests/indexer-test.<indexer id>`: test a Prowlarr indexer.

use crate::capacity::App;
use crate::ids::ArrRef;
use std::path::{Path, PathBuf};

const QUEUE_REMOVE: &str = "queue-remove.";
const INDEXER_TEST: &str = "indexer-test.";

/// One queued request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetRequest {
    QueueRemove { app: App, instance: String, id: u32 },
    IndexerTest { id: u32 },
}

impl FleetRequest {
    /// A removal named by an instance key (`sonarr`, `radarr@4k`) and a queue
    /// id. Anything else is `None`, so no request names a path outside the
    /// queue.
    pub fn queue_remove(instance_key: &str, id: &str) -> Option<Self> {
        if instance_key.contains('-') || id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let text = format!("{instance_key}-{id}");
        let item = ArrRef::parse(&text).filter(|item| item.season.is_none())?;
        Some(Self::QueueRemove { app: item.app, instance: item.instance.to_string(), id: item.id })
    }

    /// A test of the Prowlarr indexer `id`.
    pub fn indexer_test(id: &str) -> Option<Self> {
        (!id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| id.parse().ok())
            .flatten()
            .map(|id| Self::IndexerTest { id })
    }

    /// Its file under [`requests_dir`].
    pub fn path(&self, state_dir: &Path) -> PathBuf {
        let name = match self {
            Self::QueueRemove { app, instance, id } => format!("{QUEUE_REMOVE}{}-{id}", crate::ids::instance_key(*app, instance)),
            Self::IndexerTest { id } => format!("{INDEXER_TEST}{id}"),
        };
        requests_dir(state_dir).join(name)
    }

    fn from_file_name(name: &str) -> Option<Self> {
        if let Some(rest) = name.strip_prefix(QUEUE_REMOVE) {
            let (instance_key, id) = rest.rsplit_once('-')?;
            return Self::queue_remove(instance_key, id);
        }
        Self::indexer_test(name.strip_prefix(INDEXER_TEST)?)
    }
}

/// The request queue.
pub fn requests_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("fleet-requests")
}

/// Every queued request with its file, oldest name first. A file that names
/// no request is left alone.
pub fn pending(state_dir: &Path) -> Vec<(PathBuf, FleetRequest)> {
    let Ok(entries) = std::fs::read_dir(requests_dir(state_dir)) else { return Vec::new() };
    let mut requests: Vec<(PathBuf, FleetRequest)> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let request = FleetRequest::from_file_name(entry.file_name().to_str()?)?;
            Some((entry.path(), request))
        })
        .collect();
    requests.sort_by(|a, b| a.0.cmp(&b.0));
    requests
}
