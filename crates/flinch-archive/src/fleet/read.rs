//! One read of the fleet: each Radarr and Sonarr instance's
//! `system/status`, `health`, `diskspace` and queue, then Prowlarr's
//! status, health and indexers. Every read is best-effort: an app that does
//! not answer is published unreachable with the cause, and one failed read
//! leaves only its own part empty.

use super::indexers;
use super::queue::{queue_query, QueueMemory, QueueRecord, QueueStatus};
use super::{Disk, FleetApp, FleetConfig, FleetStatus, HealthEntry, InstanceStatus};
use crate::arr::instances::Connection;
use crate::executor::http::Api;
use crate::executor::ExecutorError;
use serde::Deserialize;
use serde_json::Value;

const QUEUE_PAGE_SIZE: usize = 500;
/// Queue pages read per instance before the rest is left out.
const QUEUE_PAGE_CAP: usize = 20;

/// Who the fleet is, and whether writes are only printed.
pub struct Fleet<'a> {
    /// Follows no redirects: a key would travel with one.
    pub http: &'a reqwest::Client,
    /// Every Radarr and Sonarr instance, defaults first.
    pub arrs: &'a [Connection],
    /// Prowlarr's base url and key; `None` when it is not configured.
    pub prowlarr: Option<(&'a str, &'a str)>,
    pub dry_run: bool,
}

impl<'a> Fleet<'a> {
    pub(crate) fn arr(&self, arr: &'a Connection) -> Api<'a> {
        Api::new(self.http, &arr.base, &arr.key, arr.app.label(), self.dry_run)
    }

    pub(crate) fn prowlarr(&self) -> Option<Api<'a>> {
        self.prowlarr.map(|(base, key)| Api::new(self.http, base, key, "prowlarr", self.dry_run))
    }
}

/// Read every app once. The queue memory is updated for each instance whose
/// queue was read.
pub async fn observe(fleet: &Fleet<'_>, memory: &mut QueueMemory, config: &FleetConfig, now: u64) -> FleetStatus {
    let stalled_after_secs = u64::from(config.stalled_after_minutes) * 60;
    let mut instances = Vec::with_capacity(fleet.arrs.len() + 1);
    for arr in fleet.arrs {
        instances.push(observe_arr(fleet, arr, memory, stalled_after_secs, now).await);
    }
    let mut indexers = Vec::new();
    if let (Some(api), Some((base, _))) = (fleet.prowlarr(), fleet.prowlarr) {
        let (status, listed) = observe_prowlarr(&api, base, now).await;
        instances.push(status);
        indexers = listed;
    }
    FleetStatus { checked_at: now, instances, indexers, actions: Vec::new() }
}

async fn observe_arr(fleet: &Fleet<'_>, arr: &Connection, memory: &mut QueueMemory, stalled_after_secs: u64, now: u64) -> InstanceStatus {
    let api = fleet.arr(arr);
    let mut status = unread(arr.app.into(), &arr.name, &arr.base);
    if !about(&api, "/api/v3", &mut status).await {
        return status;
    }
    match api.get("diskspace", "/api/v3/diskspace", &[]).await.and_then(|value| rows("diskspace", value)) {
        Ok(rows) => status.disks = disks(rows),
        Err(error) => note(&mut status, &error),
    }
    match queue(&api, arr.app).await {
        Ok((total, records)) => {
            let stalled = memory.judge(&arr.key(), &records, stalled_after_secs, now);
            status.queue = Some(QueueStatus { total, stalled });
        }
        Err(error) => note(&mut status, &error),
    }
    status
}

async fn observe_prowlarr(api: &Api<'_>, base: &str, now: u64) -> (InstanceStatus, Vec<indexers::IndexerStatus>) {
    let mut status = unread(FleetApp::Prowlarr, "", base);
    if !about(api, "/api/v1", &mut status).await {
        return (status, Vec::new());
    }
    let read = async {
        let listed = rows("indexer", api.get("indexer", "/api/v1/indexer", &[]).await?)?;
        let statuses = rows("indexerstatus", api.get("indexerstatus", "/api/v1/indexerstatus", &[]).await?)?;
        let stats = api.get("indexerstats", "/api/v1/indexerstats", &[]).await?;
        Ok::<_, ExecutorError>(indexers::join(listed, statuses, stats, now))
    };
    match read.await {
        Ok(listed) => (status, listed),
        Err(error) => {
            note(&mut status, &error);
            (status, Vec::new())
        }
    }
}

fn unread(app: FleetApp, instance: &str, url: &str) -> InstanceStatus {
    InstanceStatus {
        app,
        instance: instance.to_string(),
        url: url.to_string(),
        reachable: false,
        error: None,
        version: None,
        branch: None,
        os: None,
        started_at: None,
        health: Vec::new(),
        queue: None,
        disks: Vec::new(),
    }
}

/// `system/status` and `health` under `prefix`. `false` when the app does
/// not answer at all; nothing more is asked of it then.
async fn about(api: &Api<'_>, prefix: &str, status: &mut InstanceStatus) -> bool {
    #[derive(Deserialize, Default)]
    #[serde(rename_all = "camelCase", default)]
    struct System {
        version: Option<String>,
        branch: Option<String>,
        os_name: Option<String>,
        start_time: Option<String>,
    }
    let system = api
        .get("system/status", &format!("{prefix}/system/status"), &[])
        .await
        .and_then(|value| serde_json::from_value::<System>(value).map_err(|error| parse("system/status", &error)));
    let system = match system {
        Ok(system) => system,
        Err(error) => {
            note(status, &error);
            return false;
        }
    };
    status.reachable = true;
    (status.version, status.branch, status.os, status.started_at) = (system.version, system.branch, system.os_name, system.start_time);
    match api.get("health", &format!("{prefix}/health"), &[]).await.and_then(|value| rows("health", value)) {
        Ok(rows) => status.health = health(rows),
        Err(error) => note(status, &error),
    }
    true
}

/// Every queue record, page by page up to the cap, and how many the app
/// lists in all.
async fn queue(api: &Api<'_>, app: crate::capacity::App) -> Result<(u64, Vec<QueueRecord>), ExecutorError> {
    #[derive(Deserialize, Default)]
    #[serde(rename_all = "camelCase", default)]
    struct Page {
        total_records: u64,
        records: Vec<Value>,
    }
    let mut records = Vec::new();
    let mut total = 0;
    for page in 1..=QUEUE_PAGE_CAP {
        let answer = api.get("queue", "/api/v3/queue", &queue_query(app, page, QUEUE_PAGE_SIZE)).await?;
        let Page { total_records, records: rows } = serde_json::from_value(answer).map_err(|error| parse("queue", &error))?;
        total = total_records;
        let last = rows.len() < QUEUE_PAGE_SIZE;
        records.extend(crate::arr::parse_rows::<QueueRecord>(rows).parsed);
        if last {
            break;
        }
    }
    Ok((total, records))
}

fn health(rows: Vec<Value>) -> Vec<HealthEntry> {
    #[derive(Deserialize, Default)]
    #[serde(rename_all = "camelCase", default)]
    struct Check {
        source: Option<String>,
        #[serde(rename = "type")]
        kind: Option<String>,
        message: Option<String>,
        /// A string in Sonarr v4 and Radarr v5; older builds sent an object.
        wiki_url: Option<Value>,
    }
    crate::arr::parse_rows::<Check>(rows)
        .parsed
        .into_iter()
        .map(|check| HealthEntry {
            source: check.source.unwrap_or_default(),
            kind: check.kind.unwrap_or_default(),
            message: check.message.unwrap_or_default(),
            wiki_url: check.wiki_url.and_then(|url| match url {
                Value::String(url) => Some(url),
                other => other.get("fullUri").and_then(Value::as_str).map(str::to_string),
            }),
        })
        .collect()
}

fn disks(rows: Vec<Value>) -> Vec<Disk> {
    #[derive(Deserialize, Default)]
    #[serde(rename_all = "camelCase", default)]
    struct Space {
        path: String,
        free_space: u64,
        total_space: u64,
    }
    crate::arr::parse_rows::<Space>(rows)
        .parsed
        .into_iter()
        .map(|space| Disk { path: space.path, free_bytes: space.free_space, total_bytes: space.total_space })
        .collect()
}

/// Keep the first failure: it is usually the cause of the rest.
fn note(status: &mut InstanceStatus, error: &ExecutorError) {
    if status.error.is_none() {
        status.error = Some(error.to_string());
    }
}

fn rows(endpoint: &'static str, value: Value) -> Result<Vec<Value>, ExecutorError> {
    match value {
        Value::Array(rows) => Ok(rows),
        _ => Err(ExecutorError::Parse { endpoint, detail: "answer is not an array".into() }),
    }
}

fn parse(endpoint: &'static str, error: &serde_json::Error) -> ExecutorError {
    ExecutorError::Parse { endpoint, detail: error.to_string() }
}
