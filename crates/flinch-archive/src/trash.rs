//! TRaSH-Guides quality sync, Recyclarr's job done in FLINCH: custom formats,
//! quality profiles with their scores, and quality sizes from the guide at a
//! pinned commit, laid over each *arr instance. An instance may read a Profilarr
//! Compliant Database instead ([`pcd`]), parsed into the same model, so the
//! preview and apply are one code path for both.
//!
//! Why here: FLINCH's quality advice downgrades items into a compact profile,
//! and the profiles decide what every future download costs on the disks
//! FLINCH governs. The built-in presets ([`presets`]) carry what FLINCH's
//! Recyclarr companion config used to ask of Recyclarr, and each previewed
//! profile or size change carries its GiB estimate ([`impact`]).
//!
//! Invariants:
//! - Preview always; write only what the operator selected on the Quality
//!   profiles page, or everything when `apply` is on. A dry run builds every
//!   request and records it, sending none.
//! - Never delete what FLINCH did not create, unless the operator opts in
//!   (`delete_unmanaged_custom_formats`, `delete_unused_profiles`). A profile
//!   is deleted only when no item uses it, and only when the operator selects
//!   that deletion: `apply` never deletes a profile on its own.
//! - A live write is read back: a change still pending afterwards is reported
//!   as unverified, not as applied.

pub mod apply;
pub mod build;
pub mod client;
pub mod config;
pub mod desired;
pub mod diff;
pub mod guide;
pub mod impact;
pub mod language;
pub mod pcd;
pub mod presets;
pub mod prune;

pub use apply::AppOutcome;
pub use client::ArrClient;
pub use config::{InstanceConfig, InvalidTrashConfig, Source, TrashConfig, GUIDE_COMMIT};
pub use diff::{Change, Owned};
pub use impact::{DiskOutlook, Impact, Usage};

use crate::capacity::App;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The preview, the last apply and what FLINCH created, in the state dir.
pub const STATE_FILE: &str = "trash.json";
/// The operator's selection, written by flinch-web, consumed by the daemon.
pub const APPLY_REQUEST_FILE: &str = "trash-apply.json";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrashState {
    pub enabled: bool,
    pub guide_commit: String,
    /// The PCD pins read (`{commit}-{schema commit}`), when an instance reads one.
    pub pcd_commit: String,
    /// The license the PCD's manifest declared.
    pub pcd_license: Option<String>,
    pub refreshed_at_unix: u64,
    /// The settings this preview was made with; a change refreshes it early.
    pub config_fingerprint: u64,
    pub dry_run: bool,
    pub apply_automatically: bool,
    /// Why the guide could not be read; the apps were not compared then.
    pub error: Option<String>,
    pub apps: Vec<AppState>,
    pub last_apply: Option<ApplyRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppState {
    pub app: App,
    /// The instance ([`crate::ids`]); empty for the default, whose state
    /// files from before instances read unchanged.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instance: String,
    #[serde(default)]
    pub source: Source,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub changes: Vec<Change>,
    /// Change id → its disk estimate, for the profile and size changes the
    /// library touches.
    #[serde(default)]
    pub impacts: BTreeMap<String, Impact>,
    #[serde(default)]
    pub in_sync: u32,
    #[serde(default)]
    pub problems: Vec<String>,
    #[serde(default)]
    pub compact_profile_id: Option<u32>,
    #[serde(default)]
    pub managed_profile_ids: Vec<u32>,
    #[serde(default)]
    pub owned: Owned,
}

impl AppState {
    pub fn failed(app: App, source: Source, error: String, owned: Owned) -> Self {
        Self {
            app,
            instance: String::new(),
            source,
            error: Some(error),
            changes: Vec::new(),
            impacts: BTreeMap::new(),
            in_sync: 0,
            problems: Vec::new(),
            compact_profile_id: None,
            managed_profile_ids: Vec::new(),
            owned,
        }
    }

    fn of(app: App, source: Source, (diff, impacts): Preview, owned: Owned) -> Self {
        Self {
            app,
            instance: String::new(),
            source,
            error: None,
            changes: diff.changes,
            impacts,
            in_sync: diff.in_sync,
            problems: diff.problems,
            compact_profile_id: diff.compact_profile_id,
            managed_profile_ids: diff.managed_profile_ids,
            owned,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ApplyRecord {
    pub at_unix: u64,
    pub dry_run: bool,
    /// On schedule because `apply` is on, rather than the operator's click.
    pub automatic: bool,
    pub requested: Vec<String>,
    pub apps: Vec<AppApply>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppApply {
    pub app: App,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instance: String,
    #[serde(flatten)]
    pub outcome: AppOutcome,
}

/// What the page asks the daemon to apply: change ids from the preview.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ApplyRequest {
    pub changes: Vec<String>,
    #[serde(default)]
    pub requested_at_unix: u64,
}

impl TrashState {
    /// `None` before the first preview or when the file is unreadable.
    pub fn read(state_dir: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(state_dir.join(STATE_FILE)).ok()?).ok()
    }

    pub fn write(&self, state_dir: &Path) -> std::io::Result<()> {
        crate::persist::replace(&state_dir.join(STATE_FILE), &serde_json::to_vec(self)?)
    }

    /// The state of `app`'s `instance` (empty: the default).
    pub fn instance(&self, app: App, instance: &str) -> Option<&AppState> {
        self.apps.iter().find(|state| state.app == app && state.instance == instance)
    }

    /// The counts the overview shows; `None` while the sync is off.
    pub fn summary(&self) -> Option<TrashSummary> {
        self.enabled.then(|| TrashSummary {
            refreshed_at_unix: self.refreshed_at_unix,
            pending: self.apps.iter().map(|app| app.changes.len() as u32).sum(),
            in_sync: self.apps.iter().map(|app| app.in_sync).sum(),
            problems: self.apps.iter().map(|app| app.problems.len() as u32).sum(),
            errors: self.error.iter().cloned().chain(self.apps.iter().filter_map(|app| app.error.clone())).collect(),
            last_apply_unix: self.last_apply.as_ref().map(|apply| apply.at_unix),
        })
    }
}

/// The sync at a glance, in `status.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrashSummary {
    pub refreshed_at_unix: u64,
    pub pending: u32,
    pub in_sync: u32,
    pub problems: u32,
    pub errors: Vec<String>,
    pub last_apply_unix: Option<u64>,
}

/// The *arr id of the compact profile FLINCH syncs for `app`'s `instance`, as
/// last read from it; `None` when the sync is off, unread, or the profile does
/// not exist yet. Quality actions move downgraded items into it.
pub fn compact_profile_id(state_dir: &Path, app: App, instance: &str) -> Option<u32> {
    TrashState::read(state_dir)
        .filter(|state| state.enabled)
        .and_then(|state| state.instance(app, instance).and_then(|app| app.compact_profile_id))
}

/// Every profile the sync manages in `app`'s `instance`: anything else
/// writing to them would be undone by the next apply.
pub fn managed_profile_ids(state_dir: &Path, app: App, instance: &str) -> Vec<u32> {
    TrashState::read(state_dir)
        .filter(|state| state.enabled)
        .and_then(|state| state.instance(app, instance).map(|app| app.managed_profile_ids.clone()))
        .unwrap_or_default()
}

/// A stable digest of the settings, to notice an operator's change.
pub fn fingerprint(config: &TrashConfig) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(config).unwrap_or_default().hash(&mut hasher);
    hasher.finish()
}

/// Which previewed changes to apply.
pub enum Selection<'a> {
    /// `apply: true`: every change of the fresh preview but profile deletions.
    All,
    Only(&'a BTreeSet<String>),
}

/// One instance's sync: what to compare and how far to go.
pub struct AppSync<'a> {
    pub client: &'a ArrClient<'a>,
    pub config: &'a InstanceConfig,
    /// The source the instance reads, TRaSH-Guides or a PCD.
    pub guide: &'a guide::AppGuide,
    pub delete_unmanaged: bool,
    pub delete_unused_profiles: bool,
    /// Last run's record of what FLINCH created here.
    pub owned: Owned,
    /// Items per profile with their release sizes; `None` when the library
    /// was not read, which leaves impacts out and every profile kept.
    pub usage: Option<&'a Usage>,
    pub outlook: Option<DiskOutlook>,
    /// A profile FLINCH needs by name (the quality actions' fallback).
    pub keep_profile: Option<&'a str>,
}

type Preview = (diff::Diff, BTreeMap<String, Impact>);

fn preview(sync: &AppSync<'_>, desired: &desired::Desired<'_>, live: &client::Live, owned: &Owned) -> Preview {
    let app = sync.client.app();
    let mut out = diff::diff(app, desired, live, owned, sync.delete_unmanaged);
    if sync.delete_unused_profiles {
        let unused = prune::Unused { app, live, managed: &out.managed_profile_ids, owned, usage: sync.usage, keep: sync.keep_profile };
        out.changes.extend(prune::unused_profiles(&unused));
    }
    let mut impacts = sync.usage.map(|usage| impact::estimate(app, &out.changes, desired, live, usage, sync.outlook)).unwrap_or_default();
    qualify(app, sync.client.instance(), &mut out.changes, &mut impacts);
    (out, impacts)
}

/// A named instance's change ids start with its key (`radarr@4k:cf:…`), so
/// the page's selection names one instance's change, never every Radarr's.
/// The default instance's ids stay `radarr:…`.
fn qualify(app: App, instance: &str, changes: &mut [Change], impacts: &mut BTreeMap<String, Impact>) {
    if instance.is_empty() {
        return;
    }
    let (from, to) = (format!("{}:", app.label()), format!("{}:", crate::ids::instance_key(app, instance)));
    let rename = |id: &mut String| {
        if let Some(rest) = id.strip_prefix(&from) {
            *id = format!("{to}{rest}");
        }
    };
    for change in changes.iter_mut() {
        rename(&mut change.id);
        change.requires.iter_mut().for_each(rename);
    }
    *impacts = std::mem::take(impacts)
        .into_iter()
        .map(|(mut id, impact)| {
            rename(&mut id);
            (id, impact)
        })
        .collect();
}

/// Preview one instance and, when asked, apply the selection: read it,
/// compare, write, read back. An unreadable instance keeps what FLINCH knew
/// it created, so a later run can still tell its own formats from the
/// operator's.
pub async fn sync_app(sync: AppSync<'_>, apply: Option<(Selection<'_>, bool)>) -> (AppState, Option<AppOutcome>) {
    let instance = sync.client.instance();
    let (mut state, outcome) = sync_instance(sync, apply).await;
    state.instance = instance.to_string();
    (state, outcome)
}

async fn sync_instance(sync: AppSync<'_>, apply: Option<(Selection<'_>, bool)>) -> (AppState, Option<AppOutcome>) {
    let (app, source) = (sync.client.app(), sync.config.source);
    let live = match sync.client.read_live().await {
        Ok(live) => live,
        Err(error) => {
            let prefix = format!("{}:", crate::ids::instance_key(app, sync.client.instance()));
            let outcome = apply.map(|(selection, _)| {
                let failed = match selection {
                    Selection::All => Vec::new(),
                    Selection::Only(ids) => ids
                        .iter()
                        .filter(|id| id.starts_with(&prefix))
                        .map(|id| apply::Failed { id: id.clone(), error: error.to_string() })
                        .collect(),
                };
                AppOutcome { failed, ..AppOutcome::default() }
            });
            return (AppState::failed(app, source, error.to_string(), sync.owned.clone()), outcome);
        }
    };
    let mut owned = sync.owned.clone();
    // Forget what the operator deleted since; those ids are not FLINCH's now.
    owned.custom_formats.retain(|_, id| live.custom_formats.iter().any(|cf| cf.id == *id));
    owned.profiles.retain(|_, id| live.profiles.iter().any(|p| p.typed.id == *id));
    let desired = desired::resolve(sync.config, sync.guide, app);
    let first = preview(&sync, &desired, &live, &owned);
    let Some((selection, dry_run)) = apply else {
        return (AppState::of(app, source, first, owned), None);
    };
    let all: BTreeSet<String>;
    let selected = match selection {
        Selection::All => {
            let automatic = |change: &&Change| !(change.kind == diff::Kind::QualityProfile && change.action == diff::Action::Delete);
            all = first.0.changes.iter().filter(automatic).map(|change| change.id.clone()).collect();
            &all
        }
        Selection::Only(ids) => ids,
    };
    let mut writer = if dry_run { apply::Writer::dry_run() } else { apply::Writer::Live(sync.client) };
    let mut outcome = apply::apply(&mut writer, &desired, &live, &mut owned, &first.0.changes, selected).await;
    if dry_run || outcome.applied.is_empty() {
        return (AppState::of(app, source, first, owned), Some(outcome));
    }
    match sync.client.read_live().await {
        Ok(fresh) => {
            let after = preview(&sync, &desired, &fresh, &owned);
            apply::verify(&mut outcome, &after.0.changes);
            (AppState::of(app, source, after, owned), Some(outcome))
        }
        Err(error) => {
            outcome.unverified.append(&mut outcome.applied);
            let mut state = AppState::of(app, source, first, owned);
            state.error = Some(format!("read back after applying failed: {error}"));
            (state, Some(outcome))
        }
    }
}

#[cfg(test)]
mod tests;
