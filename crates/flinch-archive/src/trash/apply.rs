//! Applying the changes the operator selected, in dependency order: custom
//! formats first (profiles score them by id), then profiles, sizes, and
//! deletions last (custom formats, then unused profiles). A dry run builds
//! every request and records it instead of
//! sending it; a live run reads the app back and reports any selected change
//! that did not take.

use super::build::{self, ProfileInputs};
use super::client::{ArrClient, ArrError, Live};
use super::desired::Desired;
use super::diff::{self, Action, Change, Kind, Owned};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Where writes go: the app, or a record of what would have been sent.
pub enum Writer<'a> {
    Live(&'a ArrClient<'a>),
    DryRun { printed: Vec<String>, next_id: u32 },
}

impl Writer<'_> {
    pub fn dry_run() -> Self {
        // Stand-in ids for what a dry run "creates", far from any real id.
        Writer::DryRun { printed: Vec::new(), next_id: u32::MAX }
    }

    async fn send(&mut self, method: Method, path: &str, body: Option<&Value>, what: &str) -> Result<Value, ArrError> {
        match self {
            Writer::Live(client) => client.call(method, path, body).await,
            Writer::DryRun { printed, next_id } => {
                printed.push(format!("would {method} {path} ({what})"));
                *next_id -= 1;
                Ok(serde_json::json!({ "id": *next_id + 1 }))
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Failed {
    pub id: String,
    pub error: String,
}

/// What one app's apply did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AppOutcome {
    pub applied: Vec<String>,
    pub failed: Vec<Failed>,
    /// Sent and accepted, but still different when read back.
    pub unverified: Vec<String>,
    /// A dry run's requests, in order.
    pub printed: Vec<String>,
}

fn created_id(answer: &Value) -> Option<u32> {
    answer.get("id").and_then(Value::as_u64).and_then(|id| u32::try_from(id).ok())
}

struct Run<'r, 'a> {
    writer: &'r mut Writer<'a>,
    outcome: AppOutcome,
}

impl Run<'_, '_> {
    async fn write(&mut self, change: &Change, method: Method, path: &str, body: Option<&Value>) -> Option<Value> {
        match self.writer.send(method, path, body, &change.name).await {
            Ok(answer) => {
                self.outcome.applied.push(change.id.clone());
                Some(answer)
            }
            Err(error) => {
                self.outcome.failed.push(Failed { id: change.id.clone(), error: error.to_string() });
                None
            }
        }
    }

    fn refuse(&mut self, change: &Change, error: &str) {
        self.outcome.failed.push(Failed { id: change.id.clone(), error: error.to_string() });
    }
}

/// Apply the selected changes of this preview. `owned` records what a live
/// run creates and forgets what it deletes; a dry run leaves it alone.
pub async fn apply(
    writer: &mut Writer<'_>,
    desired: &Desired<'_>,
    live: &Live,
    owned: &mut Owned,
    changes: &[Change],
    selected: &BTreeSet<String>,
) -> AppOutcome {
    let simulated = matches!(writer, Writer::DryRun { .. });
    let mut scratch = owned.clone();
    let owned = if simulated { &mut scratch } else { owned };
    let chosen: Vec<&Change> = changes.iter().filter(|change| selected.contains(&change.id)).collect();
    let mut run = Run { writer, outcome: AppOutcome::default() };

    let mut formats: Vec<(u32, String)> = live.custom_formats.iter().map(|cf| (cf.id, cf.name.clone())).collect();
    let mut format_ids: BTreeMap<String, u32> = desired
        .custom_formats
        .iter()
        .filter_map(|(trash_id, guide)| diff::match_cf(live, owned, trash_id, &guide.name).map(|cf| (trash_id.to_string(), cf.id)))
        .collect();
    for change in chosen.iter().filter(|c| c.kind == Kind::CustomFormat && c.action != Action::Delete) {
        let Some(guide) = change.trash_id.as_deref().and_then(|id| desired.custom_formats.get(id)) else { continue };
        let body = build::custom_format_body(guide, change.arr_id);
        match change.arr_id {
            Some(id) => {
                if run.write(change, Method::PUT, &format!("/api/v3/customformat/{id}"), Some(&body)).await.is_some() {
                    formats.iter_mut().filter(|(known, _)| *known == id).for_each(|(_, name)| name.clone_from(&guide.name));
                }
            }
            None => {
                let answer = run.write(change, Method::POST, "/api/v3/customformat", Some(&body)).await;
                if let Some(id) = answer.as_ref().and_then(created_id) {
                    owned.custom_formats.insert(guide.trash_id.clone(), id);
                    format_ids.insert(guide.trash_id.clone(), id);
                    formats.push((id, guide.name.clone()));
                }
            }
        }
    }

    for change in chosen.iter().filter(|c| c.kind == Kind::QualityProfile && c.action != Action::Delete) {
        let Some(profile) = desired.profiles.iter().find(|p| Some(p.trash_id.as_str()) == change.trash_id.as_deref()) else { continue };
        let base = change.arr_id.and_then(|id| live.profiles.iter().find(|p| p.typed.id == id)).unwrap_or(&live.schema);
        let inputs = ProfileInputs {
            base,
            schema: &live.schema.raw,
            formats: &formats,
            format_ids: &format_ids,
            languages: &live.languages,
            create: change.arr_id.is_none(),
        };
        let Some(body) = build::profile_body(profile, &inputs) else {
            run.refuse(change, "the profile's cutoff names no quality the app knows");
            continue;
        };
        match change.arr_id {
            Some(id) => {
                run.write(change, Method::PUT, &format!("/api/v3/qualityprofile/{id}"), Some(&body)).await;
            }
            None => {
                let answer = run.write(change, Method::POST, "/api/v3/qualityprofile", Some(&body)).await;
                if let Some(id) = answer.as_ref().and_then(created_id) {
                    owned.profiles.insert(profile.trash_id.clone(), id);
                }
            }
        }
    }

    if let (Some(change), Some(sizes)) = (chosen.iter().find(|c| c.kind == Kind::QualityDefinition), &desired.sizes) {
        let body = Value::Array(build::sizes_body(sizes, &live.definitions));
        run.write(change, Method::PUT, "/api/v3/qualitydefinition/update", Some(&body)).await;
    }

    for change in chosen.iter().filter(|c| c.action == Action::Delete) {
        let Some(id) = change.arr_id else { continue };
        let resource = if change.kind == Kind::QualityProfile { "qualityprofile" } else { "customformat" };
        if run.write(change, Method::DELETE, &format!("/api/v3/{resource}/{id}"), None).await.is_some() {
            match change.kind {
                Kind::QualityProfile => owned.profiles.retain(|_, known| *known != id),
                _ => owned.custom_formats.retain(|_, known| *known != id),
            }
        }
    }

    let mut outcome = run.outcome;
    if let Writer::DryRun { printed, .. } = writer {
        outcome.printed = std::mem::take(printed);
    }
    outcome
}

/// After a live apply: the changes still in the fresh preview did not take.
pub fn verify(outcome: &mut AppOutcome, fresh: &[Change]) {
    let pending: BTreeSet<&str> = fresh.iter().map(|change| change.id.as_str()).collect();
    let (stuck, done): (Vec<String>, Vec<String>) =
        std::mem::take(&mut outcome.applied).into_iter().partition(|id| pending.contains(id.as_str()));
    outcome.applied = done;
    outcome.unverified = stuck;
}
