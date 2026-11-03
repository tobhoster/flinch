//! Maintainerr's own rules on the collections FLINCH fills.
//!
//! A rule group whose rules are on keeps adding what *it* selects to its
//! collection, and Maintainerr deletes that on the collection's schedule.
//! On a collection FLINCH hands evictions to, that is a second planner:
//! seen live, it deleted two seasons and two movies FLINCH never chose, with
//! no regret, no forecast and no Leaving Soon warning. So FLINCH turns those
//! rules off (setting `stop_maintainerr_rules`, on by default), after writing
//! the whole rule group to `state/maintainerr-rules-backup/` so it can be
//! restored, and reads the group back to confirm. Rule groups on collections
//! FLINCH does not use are never touched.

use super::wire::{self, read_json};
use super::{validate, CollectionInfo, CollectionTitles, MaintainerrApi, MaintainerrError};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

pub(super) const RULE_GROUPS: &str = "GET /api/rules";
pub(super) const UPDATE_RULE_GROUP: &str = "PUT /api/rules";

/// One rule group as `GET /api/rules` lists it: the fields FLINCH decides
/// on, and the whole object, kept verbatim for the backup and the update.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleGroup {
    pub id: i64,
    pub name: String,
    pub collection_id: Option<i64>,
    pub use_rules: bool,
    pub rules: usize,
    pub raw: Value,
}

impl RuleGroup {
    pub(super) fn from_value(raw: Value) -> Option<Self> {
        let id = raw.get("id")?.as_i64()?;
        let name = raw.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
        let collection_id =
            raw.get("collectionId").and_then(Value::as_i64).or_else(|| raw.pointer("/collection/id").and_then(Value::as_i64));
        let use_rules = raw.get("useRules").and_then(Value::as_bool).unwrap_or(false);
        let rules = raw.get("rules").and_then(Value::as_array).map_or(0, Vec::len);
        Some(Self { id, name, collection_id, use_rules, rules, raw })
    }

    /// Its rules still select items: switched on, with at least one rule.
    pub fn picks(&self) -> bool {
        self.use_rules && self.rules > 0
    }
}

pub(super) fn read_rule_groups(status: u16, body: &str) -> Result<Vec<RuleGroup>, MaintainerrError> {
    Ok(read_json::<Vec<Value>>(RULE_GROUPS, status, body)?.into_iter().filter_map(RuleGroup::from_value).collect())
}

pub(super) fn read_update(status: u16, body: &str) -> Result<(), MaintainerrError> {
    wire::read_return_status(UPDATE_RULE_GROUP, status, body)
}

/// Collection fields that are runtime state, never settings: sending them
/// back would at best be ignored and at worst rewrite history.
const RUNTIME: [&str; 8] =
    ["media", "id", "addDate", "handledMediaAmount", "handledMediaSizeBytes", "lastDurationInSeconds", "totalSizeBytes", "mediaServerId"];

/// Settings Maintainerr's RuleGroupDto takes at the top level although the
/// group lists them under `collection`.
const LIFTED: [&str; 13] = [
    "arrAction",
    "listExclusions",
    "cleanupLeftoverFolders",
    "forceSeerr",
    "manualCollection",
    "manualCollectionName",
    "tautulliWatchedPercentOverride",
    "keepInMaintainerrOnly",
    "radarrSettingsId",
    "sonarrSettingsId",
    "sportarrSettingsId",
    "radarrQualityProfileId",
    "sonarrQualityProfileId",
];

/// The `PUT /api/rules` body that keeps the group and every setting of its
/// collection exactly as they are, with its rules off. The collection goes
/// back whole (sort title, overlay, windows): a body naming only some
/// fields resets the rest, as a hand-made update did to Leaving Soon's sort
/// title. `None` when the group has no collection to keep.
pub(super) fn rules_off_request(group: &RuleGroup) -> Option<Value> {
    let source = group.raw.as_object()?;
    let mut collection = source.get("collection")?.as_object()?.clone();
    for field in RUNTIME {
        collection.remove(field);
    }
    let mut body: Map<String, Value> = source
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "collection" | "rules" | "collectionId"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for field in LIFTED {
        if let Some(value) = collection.get(field).filter(|value| !value.is_null()) {
            body.insert(field.to_string(), value.clone());
        }
    }
    body.retain(|_, value| !value.is_null());
    body.entry("notifications").or_insert_with(|| Value::Array(Vec::new()));
    body.insert("useRules".into(), Value::Bool(false));
    body.insert("rules".into(), Value::Array(Vec::new()));
    body.insert("collection".into(), Value::Object(collection));
    Some(Value::Object(body))
}

/// The rule groups whose rules still fill a collection FLINCH hands items to.
pub fn competing<'a>(groups: &'a [RuleGroup], collections: &[CollectionInfo], titles: &CollectionTitles) -> Vec<&'a RuleGroup> {
    let used: BTreeSet<i64> = validate::destinations(collections, titles).into_keys().collect();
    groups.iter().filter(|group| group.picks() && group.collection_id.is_some_and(|id| used.contains(&id))).collect()
}

/// What happened to one competing rule group this cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RulesOff {
    /// Turned off and read back off; the backup is at `backup`.
    Done {
        name: String,
        backup: String,
    },
    /// Printed, not sent.
    DryRun {
        name: String,
    },
    Failed {
        name: String,
        error: String,
    },
}

impl std::fmt::Display for RulesOff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Done { name, backup } => {
                write!(f, "turned off Maintainerr's own rules on {name:?}, which FLINCH fills (backup: {backup})")
            }
            Self::DryRun { name } => write!(f, "would turn off Maintainerr's own rules on {name:?}, which FLINCH fills"),
            Self::Failed { name, error } => {
                write!(f, "Maintainerr's own rules on {name:?} still pick items FLINCH did not choose; turning them off failed: {error}")
            }
        }
    }
}

/// Turn off the rules of every group that competes with FLINCH. `backup`
/// writes a group's JSON before its update and answers where; an update
/// whose backup could not be written is not sent.
pub async fn turn_off<A: MaintainerrApi>(
    api: &mut A,
    collections: &[CollectionInfo],
    titles: &CollectionTitles,
    backup: impl Fn(&RuleGroup) -> std::io::Result<String>,
) -> Result<Vec<RulesOff>, MaintainerrError> {
    let groups = api.rule_groups().await?;
    let mut outcomes = Vec::new();
    for group in competing(&groups, collections, titles) {
        let name = group.name.clone();
        let outcome = if api.simulated() {
            match api.turn_off_rules(group).await {
                Ok(()) => RulesOff::DryRun { name },
                Err(error) => RulesOff::Failed { name, error: error.to_string() },
            }
        } else {
            match backup(group) {
                Err(error) => RulesOff::Failed { name, error: format!("backup not written, nothing sent: {error}") },
                Ok(path) => match api.turn_off_rules(group).await {
                    Err(error) => RulesOff::Failed { name, error: error.to_string() },
                    Ok(()) => match api.rule_groups().await?.iter().find(|again| again.id == group.id) {
                        Some(again) if again.picks() => {
                            RulesOff::Failed { name, error: "Maintainerr answered success but the rules are still on".into() }
                        }
                        _ => RulesOff::Done { name, backup: path },
                    },
                },
            }
        };
        outcomes.push(outcome);
    }
    Ok(outcomes)
}
