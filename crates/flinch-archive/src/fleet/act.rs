//! What FLINCH does to the fleet: remove an unimportable queue item (and
//! blocklist its release), test a Prowlarr indexer, disable one that keeps
//! failing. Each runs on the operator's request ([`super::requests`]) or,
//! where the settings allow, by itself; each is printed instead of sent in a
//! dry run and recorded as an [`Action`].

use super::requests::FleetRequest;
use super::{Action, ActionKind, Fleet, FleetApp, FleetConfig, FleetStatus, Reason};
use crate::capacity::App;
use crate::executor::ExecutorError;
use reqwest::Method;
use serde_json::Value;
use std::time::Duration;

/// Prowlarr retries a dead indexer's site before it answers a test: seen
/// live, 44 s for a site answering 504. The client's usual timeout would cut
/// that off and report a transport error instead of Prowlarr's cause.
const INDEXER_TEST_TIMEOUT: Duration = Duration::from_secs(150);

/// What a write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    Done,
    /// Printed, not sent.
    DryRun,
}

/// Why an action was not carried out.
#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    /// The item left the queue (or never was in it) since the last read.
    #[error("not in the queue as last read")]
    NotQueued,
    /// Only an item that can never import is removed.
    #[error("refused: queue items that {} are never removed", .0.describe())]
    NotRemovable(Reason),
    #[error("no configured instance by that name")]
    NoInstance,
    #[error("Prowlarr is not configured")]
    NoProwlarr,
    #[error("Prowlarr lists no such indexer")]
    NoIndexer,
    #[error(transparent)]
    Api(#[from] ExecutorError),
}

/// `DELETE /api/v3/queue/{id}?removeFromClient=true&blocklist=true`, only for
/// an item the last read judged removable. The app answers the delete with
/// 200 and offers no read of one queue record, so its answer is the proof.
pub async fn remove_queue_item(fleet: &Fleet<'_>, status: &FleetStatus, app: App, instance: &str, id: u64) -> Result<Sent, ActionError> {
    let item = status.stalled_item(app.into(), instance, id).ok_or(ActionError::NotQueued)?;
    if !(item.removable && item.reason.removable()) {
        return Err(ActionError::NotRemovable(item.reason));
    }
    let arr = fleet.arrs.iter().find(|arr| arr.app == app && arr.name == instance).ok_or(ActionError::NoInstance)?;
    let query = [("removeFromClient", "true".to_string()), ("blocklist", "true".to_string())];
    let sent = fleet.arr(arr).write("queue", Method::DELETE, &format!("/api/v3/queue/{id}"), &query, None).await?;
    Ok(sent.map_or(Sent::DryRun, |_| Sent::Done))
}

/// `POST /api/v1/indexer/test` with the indexer's own resource, as
/// Prowlarr's Test button sends it. A failed test answers 400 with the cause.
pub async fn test_indexer(fleet: &Fleet<'_>, id: u64) -> Result<Sent, ActionError> {
    let api = fleet.prowlarr().ok_or(ActionError::NoProwlarr)?;
    let resource = api.find("indexer", &format!("/api/v1/indexer/{id}")).await?.ok_or(ActionError::NoIndexer)?;
    let sent = api.write_within("indexer test", Method::POST, "/api/v1/indexer/test", &[], Some(&resource), Some(INDEXER_TEST_TIMEOUT)).await?;
    Ok(sent.map_or(Sent::DryRun, |_| Sent::Done))
}

/// `PUT /api/v1/indexer/{id}` with `enable: false`, checked in the answer.
pub async fn disable_indexer(fleet: &Fleet<'_>, id: u64) -> Result<Sent, ActionError> {
    let api = fleet.prowlarr().ok_or(ActionError::NoProwlarr)?;
    let mut resource = api.find("indexer", &format!("/api/v1/indexer/{id}")).await?.ok_or(ActionError::NoIndexer)?;
    let Some(fields) = resource.as_object_mut() else {
        return Err(ExecutorError::Parse { endpoint: "indexer", detail: "the indexer is not an object".into() }.into());
    };
    fields.insert("enable".into(), Value::Bool(false));
    match api.write("indexer", Method::PUT, &format!("/api/v1/indexer/{id}"), &[], Some(&resource)).await? {
        None => Ok(Sent::DryRun),
        Some(answer) if answer.get("enable") == Some(&Value::Bool(false)) => Ok(Sent::Done),
        Some(_) => Err(ExecutorError::NotApplied { endpoint: "indexer", detail: "Prowlarr still lists it enabled".into() }.into()),
    }
}

/// Carry out one request the web UI queued.
pub async fn requested(fleet: &Fleet<'_>, status: &FleetStatus, request: &FleetRequest, now: u64) -> Action {
    match request {
        FleetRequest::QueueRemove { app, instance, id } => {
            let result = remove_queue_item(fleet, status, *app, instance, u64::from(*id)).await;
            record(ActionKind::QueueRemove, queue_target(status, *app, instance, u64::from(*id)), result, now)
        }
        FleetRequest::IndexerTest { id } => {
            let result = test_indexer(fleet, u64::from(*id)).await;
            record(ActionKind::IndexerTest, indexer_target(status, u64::from(*id)), result, now)
        }
    }
}

/// The automatic clean-up the settings switch on: every unimportable queue
/// item removed, every indexer failing past the limit disabled.
pub async fn auto(fleet: &Fleet<'_>, status: &FleetStatus, config: &FleetConfig, now: u64) -> Vec<Action> {
    let mut actions = Vec::new();
    if config.auto_remove_unimportable {
        for instance in &status.instances {
            let (Some(app), Some(queue)) = (arr_app(instance.app), &instance.queue) else { continue };
            for item in queue.stalled.iter().filter(|item| item.removable) {
                let result = remove_queue_item(fleet, status, app, &instance.instance, item.id).await;
                actions.push(record(ActionKind::QueueRemove, queue_target(status, app, &instance.instance, item.id), result, now));
            }
        }
    }
    let limit_secs = u64::from(config.disable_failing_indexers_after_hours) * 3_600;
    if limit_secs > 0 {
        let failing =
            status.indexers.iter().filter(|indexer| indexer.enabled && indexer.failing_for(now).is_some_and(|secs| secs >= limit_secs));
        for indexer in failing {
            let result = disable_indexer(fleet, indexer.id).await;
            actions.push(record(ActionKind::IndexerDisable, indexer_target(status, indexer.id), result, now));
        }
    }
    actions
}

fn arr_app(app: FleetApp) -> Option<App> {
    match app {
        FleetApp::Sonarr => Some(App::Sonarr),
        FleetApp::Radarr => Some(App::Radarr),
        FleetApp::Prowlarr => None,
    }
}

/// `sonarr@anime 123 (title)`; the title only while the item is listed.
fn queue_target(status: &FleetStatus, app: App, instance: &str, id: u64) -> String {
    let key = crate::ids::instance_key(app, instance);
    match status.stalled_item(app.into(), instance, id) {
        Some(item) => format!("{key} {id} ({})", item.title),
        None => format!("{key} {id}"),
    }
}

/// `indexer 36 (AniSource)`.
fn indexer_target(status: &FleetStatus, id: u64) -> String {
    match status.indexer(id) {
        Some(indexer) => format!("indexer {id} ({})", indexer.name),
        None => format!("indexer {id}"),
    }
}

fn record(kind: ActionKind, target: String, result: Result<Sent, ActionError>, at: u64) -> Action {
    let result = match result {
        Ok(Sent::Done) => "done".to_string(),
        Ok(Sent::DryRun) => "dry-run".to_string(),
        Err(error) => format!("failed: {error}"),
    };
    Action { at, kind, target, result }
}
