//! The *arr fleet: whether each Radarr, Sonarr and Prowlarr answers, what
//! its own health checks say, which queue items are stuck and why, and which
//! Prowlarr indexers are failing. The daemon reads it each cycle into
//! `arr-fleet-status.json`; the web UI shows it and queues two actions
//! ([`requests`]) the daemon carries out ([`act`]).
//!
//! Invariants:
//! - A queue item waiting for free space is never removed, by request or by
//!   the automatic clean-up: it imports by itself once the disk has room.
//!   Only an item the app can never import ([`queue::Reason::Unimportable`])
//!   is removable, and removal blocklists the release so it is not grabbed
//!   again.
//! - Every write is printed, not sent, in a dry run, and recorded as
//!   `dry-run`; reads always go out.
//! - Prowlarr is optional: without it the indexer list is empty.

pub mod act;
pub mod indexers;
pub mod queue;
mod read;
pub mod requests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_http;

pub use act::{ActionError, Sent};
pub use indexers::IndexerStatus;
pub use queue::{QueueMemory, QueueStatus, Reason, StalledItem};
pub use read::{observe, Fleet};

use serde::{Deserialize, Serialize};

/// What the daemon publishes each cycle, in the state directory.
pub const STATUS_FILE: &str = "arr-fleet-status.json";
/// Each queue item's progress between cycles ([`QueueMemory`]).
pub const QUEUE_MEMORY_FILE: &str = "fleet-queue.json";
/// The newest actions kept in the status.
pub const ACTIONS_KEPT: usize = 50;

/// `settings.fleet`. Watching is on; every automatic action is off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FleetConfig {
    /// Read the fleet each cycle and publish it.
    pub enabled: bool,
    /// A download that has not progressed this long is stalled.
    pub stalled_after_minutes: u32,
    /// Remove (and blocklist) every unimportable queue item each cycle.
    pub auto_remove_unimportable: bool,
    /// Disable a Prowlarr indexer failing for this many hours; 0 never does.
    pub disable_failing_indexers_after_hours: u32,
}

impl Default for FleetConfig {
    fn default() -> Self {
        Self { enabled: true, stalled_after_minutes: 120, auto_remove_unimportable: false, disable_failing_indexers_after_hours: 0 }
    }
}

/// A [`FleetConfig`] outside its bounds; the message names the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidFleetConfig(pub &'static str);

impl FleetConfig {
    pub fn validate(&self) -> Result<(), InvalidFleetConfig> {
        if !(10..=10_080).contains(&self.stalled_after_minutes) {
            return Err(InvalidFleetConfig("a download is stalled after (fleet.stalled_after_minutes) 10 to 10080 minutes"));
        }
        if self.disable_failing_indexers_after_hours > 720 {
            return Err(InvalidFleetConfig(
                "failing indexers are disabled after (fleet.disable_failing_indexers_after_hours) 0 (never) to 720 hours",
            ));
        }
        Ok(())
    }
}

/// The apps the fleet watches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FleetApp {
    Sonarr,
    Radarr,
    Prowlarr,
}

impl FleetApp {
    pub fn label(self) -> &'static str {
        match self {
            FleetApp::Sonarr => "sonarr",
            FleetApp::Radarr => "radarr",
            FleetApp::Prowlarr => "prowlarr",
        }
    }
}

impl From<crate::capacity::App> for FleetApp {
    fn from(app: crate::capacity::App) -> Self {
        match app {
            crate::capacity::App::Radarr => FleetApp::Radarr,
            crate::capacity::App::Sonarr => FleetApp::Sonarr,
        }
    }
}

/// `arr-fleet-status.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FleetStatus {
    pub checked_at: u64,
    pub instances: Vec<InstanceStatus>,
    pub indexers: Vec<IndexerStatus>,
    /// The newest [`ACTIONS_KEPT`] actions, oldest first.
    pub actions: Vec<Action>,
}

impl FleetStatus {
    /// The stuck queue item `id` of `app`'s `instance`, as last read.
    pub fn stalled_item(&self, app: FleetApp, instance: &str, id: u64) -> Option<&StalledItem> {
        self.instance(app, instance)?.queue.as_ref()?.stalled.iter().find(|item| item.id == id)
    }

    pub fn instance(&self, app: FleetApp, instance: &str) -> Option<&InstanceStatus> {
        self.instances.iter().find(|status| status.app == app && status.instance == instance)
    }

    pub fn indexer(&self, id: u64) -> Option<&IndexerStatus> {
        self.indexers.iter().find(|indexer| indexer.id == id)
    }

    /// Append this cycle's actions to the earlier ones, keeping the newest.
    pub fn record(&mut self, earlier: Vec<Action>, new: Vec<Action>) {
        let mut actions = earlier;
        actions.extend(new);
        let excess = actions.len().saturating_sub(ACTIONS_KEPT);
        actions.drain(..excess);
        self.actions = actions;
    }

    /// One sentence per fault an operator should hear about: an app that does
    /// not answer, items stuck in a queue, an indexer failing. Keyed by
    /// wording, so a fault keeps its key while it lasts.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for instance in &self.instances {
            let name = instance.name();
            if !instance.reachable {
                problems.push(format!("{name} does not answer"));
                continue;
            }
            let Some(queue) = &instance.queue else { continue };
            for reason in [Reason::NoSpace, Reason::Unimportable, Reason::StalledDownload] {
                let stuck = queue.stalled.iter().filter(|item| item.reason == reason).count();
                if stuck > 0 {
                    problems.push(format!("{name}: {stuck} queue item(s) {}", reason.describe()));
                }
            }
        }
        problems.extend(
            self.indexers.iter().filter(|indexer| indexer.failing).map(|indexer| format!("Prowlarr indexer {} is failing", indexer.name)),
        );
        problems
    }
}

/// One app instance as last read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceStatus {
    pub app: FleetApp,
    /// Empty for the default instance.
    #[serde(default)]
    pub instance: String,
    pub url: String,
    /// `system/status` answered.
    pub reachable: bool,
    /// The first read that failed, in a few words.
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub health: Vec<HealthEntry>,
    /// `None` for Prowlarr, which has no download queue, or when unread.
    #[serde(default)]
    pub queue: Option<QueueStatus>,
    #[serde(default)]
    pub disks: Vec<Disk>,
}

impl InstanceStatus {
    /// `Sonarr`, or `Sonarr anime` for a named instance.
    pub fn name(&self) -> String {
        let app = match self.app {
            FleetApp::Sonarr => "Sonarr",
            FleetApp::Radarr => "Radarr",
            FleetApp::Prowlarr => "Prowlarr",
        };
        if self.instance.is_empty() {
            app.to_string()
        } else {
            format!("{app} {}", self.instance)
        }
    }
}

/// One of the app's own health checks (`/api/v3/health`, `/api/v1/health`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthEntry {
    pub source: String,
    /// `ok`, `notice`, `warning` or `error`.
    #[serde(rename = "type")]
    pub kind: String,
    pub message: String,
    #[serde(default)]
    pub wiki_url: Option<String>,
}

/// One mount the app reports (`/api/v3/diskspace`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disk {
    pub path: String,
    pub free_bytes: u64,
    pub total_bytes: u64,
}

/// What FLINCH did to the fleet, or would have in a dry run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    pub at: u64,
    pub kind: ActionKind,
    /// `sonarr 123 (title)`, `indexer 36 (AniSource)`.
    pub target: String,
    /// `done`, `dry-run`, or `failed: <why>`.
    pub result: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    QueueRemove,
    IndexerTest,
    IndexerDisable,
}
