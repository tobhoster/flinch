//! The request bodies the sync sends. A profile is built over the object the
//! *arr served (or its schema, for a new one) so unknown fields survive; its
//! ladder lists every quality the app knows, as Radarr and Sonarr require
//! ("Must contain all qualities"), and its scores every custom format ("All
//! Custom Formats and no extra ones need to be present"), both checked by
//! <https://github.com/Radarr/Radarr/blob/develop/src/Radarr.Api.V3/Profiles/Quality/QualityProfileController.cs>.

use super::client::{ArrLanguage, ArrProfile, ArrProfileItem, ArrQualityDefinition, Raw};
use super::desired::{DesiredProfile, DesiredSizes};
use super::guide::GuideCustomFormat;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// `POST`/`PUT /api/v3/customformat`: the guide's field object becomes the
/// *arr's `[{name, value}]` list.
pub fn custom_format_body(guide: &GuideCustomFormat, id: Option<u32>) -> Value {
    let specifications: Vec<Value> = guide
        .specifications
        .iter()
        .map(|spec| {
            let fields: Vec<Value> = spec.fields.iter().map(|(name, value)| json!({ "name": name, "value": value })).collect();
            json!({
                "name": spec.name,
                "implementation": spec.implementation,
                "negate": spec.negate,
                "required": spec.required,
                "fields": fields,
            })
        })
        .collect();
    let mut body = json!({
        "name": guide.name,
        "includeCustomFormatWhenRenaming": guide.include_when_renaming,
        "specifications": specifications,
    });
    if let (Some(id), Some(object)) = (id, body.as_object_mut()) {
        object.insert("id".to_string(), json!(id));
    }
    body
}

/// Every quality object of the schema by name, best first, as served.
fn schema_qualities(schema: &Value) -> Vec<(String, Value)> {
    fn walk(items: &[Value], into: &mut Vec<(String, Value)>) {
        for item in items.iter().rev() {
            if let Some(quality) = item.get("quality").filter(|q| q.is_object()) {
                if let Some(name) = quality.get("name").and_then(Value::as_str) {
                    into.push((name.to_string(), quality.clone()));
                }
            }
            if let Some(children) = item.get("items").and_then(Value::as_array) {
                walk(children, into);
            }
        }
    }
    let mut qualities = Vec::new();
    walk(schema.get("items").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default(), &mut qualities);
    qualities
}

fn group_ids(items: &[ArrProfileItem], into: &mut BTreeMap<String, i64>) {
    for item in items {
        if let (None, Some(id), Some(name)) = (&item.quality, item.id, &item.name) {
            into.insert(name.clone(), id);
        }
        group_ids(&item.items, into);
    }
}

/// The ladder in API order (worst first), and the id the cutoff points at.
fn ladder(desired: &DesiredProfile, base: &ArrProfile, schema: &Value) -> (Vec<Value>, Option<i64>) {
    let qualities = schema_qualities(schema);
    let quality = |name: &str| qualities.iter().find(|(known, _)| known == name).map(|(_, q)| q.clone());
    let mut groups = BTreeMap::new();
    group_ids(&base.items, &mut groups);
    // New groups get ids as the Radarr UI assigns them: above 1000 and above
    // every id in use.
    let mut next = groups.values().copied().max().unwrap_or(0).max(1000) + 1;
    let mut used = Vec::new();
    let mut items = Vec::new();
    let mut cutoff = None;
    for rung in &desired.ladder {
        if rung.members.is_empty() {
            let Some(q) = quality(&rung.name) else { continue };
            if rung.name == desired.cutoff {
                cutoff = q.get("id").and_then(Value::as_i64);
            }
            used.push(rung.name.clone());
            items.push(json!({ "quality": q, "items": [], "allowed": rung.allowed }));
            continue;
        }
        let mut members: Vec<Value> = Vec::new();
        for name in &rung.members {
            if let Some(q) = quality(name) {
                used.push(name.clone());
                members.push(json!({ "quality": q, "items": [], "allowed": rung.allowed }));
            }
        }
        if members.is_empty() {
            continue;
        }
        let id = *groups.entry(rung.name.clone()).or_insert_with(|| {
            next += 1;
            next - 1
        });
        if rung.name == desired.cutoff {
            cutoff = Some(id);
        }
        members.reverse();
        items.push(json!({ "id": id, "name": rung.name, "items": members, "allowed": rung.allowed }));
    }
    for (name, q) in &qualities {
        if !used.contains(name) {
            items.push(json!({ "quality": q, "items": [], "allowed": false }));
        }
    }
    items.reverse();
    (items, cutoff)
}

/// What a profile body needs besides the profile itself.
pub struct ProfileInputs<'a> {
    /// The live profile to update, or the schema for a new one.
    pub base: &'a Raw<ArrProfile>,
    pub schema: &'a Value,
    /// Every custom format the *arr will hold: (id, name).
    pub formats: &'a [(u32, String)],
    /// Desired format trash id → *arr id.
    pub format_ids: &'a BTreeMap<String, u32>,
    pub languages: &'a [ArrLanguage],
    pub create: bool,
}

/// `POST`/`PUT /api/v3/qualityprofile`; `None` when the cutoff names no rung
/// the app knows.
pub fn profile_body(desired: &DesiredProfile, inputs: &ProfileInputs<'_>) -> Option<Value> {
    let (items, cutoff) = ladder(desired, &inputs.base.typed, inputs.schema);
    let cutoff = cutoff?;
    let scores: BTreeMap<u32, i32> =
        desired.scores.iter().filter_map(|(trash_id, score)| inputs.format_ids.get(trash_id).map(|id| (*id, *score))).collect();
    let current = |id: u32| inputs.base.typed.format_items.iter().find(|f| f.format == id).map_or(0, |f| f.score);
    let format_items: Vec<Value> = inputs
        .formats
        .iter()
        .map(|(id, name)| {
            let score = scores.get(id).copied().unwrap_or(if desired.reset_unmatched_scores || inputs.create { 0 } else { current(*id) });
            json!({ "format": id, "name": name, "score": score })
        })
        .collect();
    let mut body = inputs.base.raw.clone();
    let object = body.as_object_mut()?;
    if inputs.create {
        object.remove("id");
    }
    object.insert("name".to_string(), json!(desired.name));
    object.insert("upgradeAllowed".to_string(), json!(desired.upgrade_allowed));
    object.insert("cutoff".to_string(), json!(cutoff));
    object.insert("minFormatScore".to_string(), json!(desired.min_format_score));
    object.insert("cutoffFormatScore".to_string(), json!(desired.cutoff_format_score));
    object.insert("minUpgradeFormatScore".to_string(), json!(desired.min_upgrade_format_score));
    object.insert("items".to_string(), Value::Array(items));
    object.insert("formatItems".to_string(), Value::Array(format_items));
    let language = desired.language.as_deref().and_then(|name| inputs.languages.iter().find(|l| l.name.eq_ignore_ascii_case(name)));
    if let Some(language) = language {
        object.insert("language".to_string(), json!({ "id": language.id, "name": language.name }));
    }
    Some(body)
}

/// `PUT /api/v3/qualitydefinition/update`: the definitions that change,
/// each patched over the object the app served.
pub fn sizes_body(sizes: &DesiredSizes, definitions: &[Raw<ArrQualityDefinition>]) -> Vec<Value> {
    sizes
        .qualities
        .iter()
        .filter_map(|target| {
            let definition = definitions.iter().find(|d| d.typed.quality.name == target.quality)?;
            let mut body = definition.raw.clone();
            let object = body.as_object_mut()?;
            object.insert("minSize".to_string(), json!(target.min));
            object.insert("maxSize".to_string(), json!(target.max));
            object.insert("preferredSize".to_string(), json!(target.preferred));
            Some(body)
        })
        .collect()
}
