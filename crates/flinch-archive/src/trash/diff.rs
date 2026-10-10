//! The preview: every change the sync would make to one app, each with a
//! stable id the operator selects by. Nothing here writes.
//!
//! Matching is by what FLINCH created first (trash id → *arr id it recorded),
//! then by name, as Recyclarr adopts a format or profile of the same name.
//! Custom formats are deleted here: those FLINCH created and no profile uses
//! any more, and the operator's own only when `delete_unmanaged_custom_formats`
//! is on. Profiles are deleted only through [`super::prune`], opt-in.

use super::client::{ArrCustomFormat, ArrProfile, ArrProfileItem, Live, Raw};
use super::desired::{limits, Desired, DesiredProfile};
use super::guide::{GuideCustomFormat, GuideSpecification};
use crate::capacity::App;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    CustomFormat,
    QualityProfile,
    QualityDefinition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Create,
    Update,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    /// `{app}:{kind}:{trash id or *arr id}`, the same from one preview to the next.
    pub id: String,
    pub app: App,
    pub kind: Kind,
    pub action: Action,
    pub name: String,
    #[serde(default)]
    pub trash_id: Option<String>,
    /// The *arr's id of what is updated or deleted.
    #[serde(default)]
    pub arr_id: Option<u32>,
    #[serde(default)]
    pub fields: Vec<FieldChange>,
    /// Changes this one needs first: a profile scores formats that must exist.
    #[serde(default)]
    pub requires: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldChange {
    pub field: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

fn field(field: impl Into<String>, from: Option<String>, to: Option<String>) -> FieldChange {
    FieldChange { field: field.into(), from, to }
}

/// What FLINCH created in one app: trash id → *arr id. Only these are FLINCH's
/// to delete.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Owned {
    #[serde(default)]
    pub custom_formats: BTreeMap<String, u32>,
    #[serde(default)]
    pub profiles: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Diff {
    pub changes: Vec<Change>,
    /// Managed formats, profiles and size tables already as asked.
    pub in_sync: u32,
    pub compact_profile_id: Option<u32>,
    pub managed_profile_ids: Vec<u32>,
    pub problems: Vec<String>,
}

pub fn match_cf<'l>(live: &'l Live, owned: &Owned, trash_id: &str, name: &str) -> Option<&'l ArrCustomFormat> {
    let by_id = owned.custom_formats.get(trash_id).and_then(|id| live.custom_formats.iter().find(|cf| cf.id == *id));
    by_id.or_else(|| live.custom_formats.iter().find(|cf| cf.name.eq_ignore_ascii_case(name)))
}

pub fn match_profile<'l>(live: &'l Live, owned: &Owned, trash_id: &str, name: &str) -> Option<&'l Raw<ArrProfile>> {
    let by_id = owned.profiles.get(trash_id).and_then(|id| live.profiles.iter().find(|p| p.typed.id == *id));
    by_id.or_else(|| live.profiles.iter().find(|p| p.typed.name.eq_ignore_ascii_case(name)))
}

pub fn change_id(app: App, kind: &str, key: &str) -> String {
    format!("{}:{kind}:{key}", app.label())
}

fn spec_summary(implementation: &str, negate: bool, required: bool, values: &[(String, String)]) -> String {
    let values: Vec<String> =
        values.iter().map(|(name, value)| if name == "value" { value.clone() } else { format!("{name}={value}") }).collect();
    format!(
        "{}{}{}: {}",
        implementation.trim_end_matches("Specification"),
        if negate { ", negated" } else { "" },
        if required { ", required" } else { "" },
        values.join(" ")
    )
}

fn plain(value: &serde_json::Value) -> String {
    value.as_str().map_or_else(|| value.to_string(), str::to_string)
}

fn guide_spec_summary(spec: &GuideSpecification) -> String {
    let values: Vec<(String, String)> = spec.fields.iter().map(|(name, value)| (name.clone(), plain(value))).collect();
    spec_summary(&spec.implementation, spec.negate, spec.required, &values)
}

/// How a live format differs from the guide's: name, rename flag, and each
/// specification by name. Only the fields the guide sets are compared; the
/// *arr adds defaults for the rest.
fn cf_fields(guide: &GuideCustomFormat, live: &ArrCustomFormat) -> Vec<FieldChange> {
    let mut fields = Vec::new();
    if live.name != guide.name {
        fields.push(field("name", Some(live.name.clone()), Some(guide.name.clone())));
    }
    if live.include_custom_format_when_renaming != guide.include_when_renaming {
        fields.push(field(
            "include when renaming",
            Some(live.include_custom_format_when_renaming.to_string()),
            Some(guide.include_when_renaming.to_string()),
        ));
    }
    for spec in &guide.specifications {
        let found = live.specifications.iter().find(|s| s.name == spec.name);
        let same = found.is_some_and(|s| {
            s.implementation == spec.implementation
                && s.negate == spec.negate
                && s.required == spec.required
                && spec.fields.iter().all(|(name, value)| s.fields.iter().any(|f| &f.name == name && &f.value == value))
        });
        if !same {
            let from = found.map(|s| {
                let values: Vec<(String, String)> = spec
                    .fields
                    .keys()
                    .map(|name| (name.clone(), s.fields.iter().find(|f| &f.name == name).map_or_else(String::new, |f| plain(&f.value))))
                    .collect();
                spec_summary(&s.implementation, s.negate, s.required, &values)
            });
            fields.push(field(format!("condition {}", spec.name), from, Some(guide_spec_summary(spec))));
        }
    }
    for spec in live.specifications.iter().filter(|s| !guide.specifications.iter().any(|g| g.name == s.name)) {
        fields.push(field(
            format!("condition {}", spec.name),
            Some(spec.implementation.trim_end_matches("Specification").to_string()),
            None,
        ));
    }
    fields
}

/// The rungs a profile grabs, best first: `Name` or `Group (a, b)`.
fn live_ladder(profile: &ArrProfile) -> Vec<String> {
    profile
        .items
        .iter()
        .rev()
        .filter(|item| item.allowed)
        .map(|item| rung_label(item.label(), item.items.iter().map(ArrProfileItem::label)))
        .collect()
}

fn rung_label<'a>(name: &str, members: impl Iterator<Item = &'a str>) -> String {
    let mut members: Vec<&str> = members.collect();
    if members.is_empty() {
        return name.to_string();
    }
    members.sort_unstable();
    format!("{name} ({})", members.join(", "))
}

/// Every quality name the app knows, from the schema.
pub fn known_qualities(schema: &ArrProfile) -> BTreeSet<&str> {
    fn walk<'a>(items: &'a [ArrProfileItem], into: &mut BTreeSet<&'a str>) {
        for item in items {
            if let Some(quality) = &item.quality {
                into.insert(quality.name.as_str());
            }
            walk(&item.items, into);
        }
    }
    let mut known = BTreeSet::new();
    walk(&schema.items, &mut known);
    known
}

fn desired_ladder(profile: &DesiredProfile, known: &BTreeSet<&str>) -> Vec<String> {
    profile
        .ladder
        .iter()
        .filter(|rung| rung.allowed)
        .filter_map(|rung| {
            if rung.members.is_empty() {
                return known.contains(rung.name.as_str()).then(|| rung.name.clone());
            }
            let members: Vec<&str> = rung.members.iter().map(String::as_str).filter(|m| known.contains(m)).collect();
            (!members.is_empty()).then(|| rung_label(&rung.name, members.into_iter()))
        })
        .collect()
}

fn cutoff_name(profile: &ArrProfile) -> Option<&str> {
    profile.items.iter().find(|item| item.cutoff_id() == Some(profile.cutoff)).map(ArrProfileItem::label)
}

fn number<T: PartialEq + ToString>(fields: &mut Vec<FieldChange>, name: &str, from: T, to: T) {
    if from != to {
        fields.push(field(name, Some(from.to_string()), Some(to.to_string())));
    }
}

struct ProfileContext<'a, 'g> {
    app: App,
    desired: &'a Desired<'g>,
    live: &'a Live,
    owned: &'a Owned,
    known: BTreeSet<&'a str>,
}

/// Scores the profile should end with, keyed by the *arr's format id, and the
/// creations it waits for.
fn profile_fields(cx: &ProfileContext<'_, '_>, profile: &DesiredProfile, live: Option<&ArrProfile>) -> (Vec<FieldChange>, Vec<String>) {
    let mut fields = Vec::new();
    let mut requires = Vec::new();
    let ladder = desired_ladder(profile, &cx.known);
    match live {
        Some(live) => {
            if live.name != profile.name {
                fields.push(field("name", Some(live.name.clone()), Some(profile.name.clone())));
            }
            number(&mut fields, "upgrades allowed", live.upgrade_allowed, profile.upgrade_allowed);
            if cutoff_name(live) != Some(profile.cutoff.as_str()) {
                fields.push(field("upgrade until quality", cutoff_name(live).map(str::to_string), Some(profile.cutoff.clone())));
            }
            number(&mut fields, "upgrade until score", live.cutoff_format_score, profile.cutoff_format_score);
            number(&mut fields, "minimum score", live.min_format_score, profile.min_format_score);
            number(&mut fields, "minimum upgrade score", live.min_upgrade_format_score, profile.min_upgrade_format_score);
            if cx.app == App::Radarr {
                let (from, to) = (live.language.as_ref().map(|l| l.name.clone()), profile.language.clone());
                if to.is_some() && !from.as_deref().zip(to.as_deref()).is_some_and(|(a, b)| a.eq_ignore_ascii_case(b)) {
                    fields.push(field("language", from, to));
                }
            }
            let from = live_ladder(live);
            if from != ladder {
                fields.push(field("qualities", Some(from.join(" > ")), Some(ladder.join(" > "))));
            }
        }
        None => {
            fields.push(field("qualities", None, Some(ladder.join(" > "))));
            fields.push(field("upgrade until quality", None, Some(profile.cutoff.clone())));
            fields.push(field("upgrade until score", None, Some(profile.cutoff_format_score.to_string())));
            fields.push(field("minimum upgrade score", None, Some(profile.min_upgrade_format_score.to_string())));
        }
    }
    // Scores: every format the profile asks for, then every other format the
    // *arr has, which a reset sets to 0.
    let mut wanted_ids = BTreeSet::new();
    for (trash_id, score) in &profile.scores {
        let Some(guide) = cx.desired.custom_formats.get(trash_id.as_str()) else { continue };
        match match_cf(cx.live, cx.owned, trash_id, &guide.name) {
            Some(cf) => {
                wanted_ids.insert(cf.id);
                let current = live.and_then(|p| p.format_items.iter().find(|f| f.format == cf.id)).map(|f| f.score);
                if current != Some(*score) && (live.is_some() || *score != 0) {
                    fields.push(field(format!("score {}", guide.name), current.map(|s| s.to_string()), Some(score.to_string())));
                }
            }
            None => {
                requires.push(change_id(cx.app, "cf", trash_id));
                fields.push(field(format!("score {}", guide.name), None, Some(score.to_string())));
            }
        }
    }
    if profile.reset_unmatched_scores {
        for item in live.map(|p| p.format_items.as_slice()).unwrap_or_default() {
            if !wanted_ids.contains(&item.format) && item.score != 0 {
                fields.push(field(format!("score {}", item.name), Some(item.score.to_string()), Some("0".to_string())));
            }
        }
    }
    (fields, requires)
}

fn sizes_change(app: App, desired: &Desired<'_>, live: &Live, problems: &mut Vec<String>) -> Option<Change> {
    let sizes = desired.sizes.as_ref()?;
    let limit = limits(app);
    let close = |a: f64, b: f64| (a - b).abs() < 0.05;
    let show = |min: f64, preferred: f64, max: f64| format!("min {min} · preferred {preferred} · max {max}");
    let mut fields = Vec::new();
    for target in &sizes.qualities {
        let Some(definition) = live.definitions.iter().find(|d| d.typed.quality.name == target.quality) else {
            problems.push(format!("{} has no quality definition for {}; it is skipped", app.label(), target.quality));
            continue;
        };
        let d = &definition.typed;
        let (min, max, preferred) =
            (d.min_size.unwrap_or(0.0), d.max_size.unwrap_or(limit.max), d.preferred_size.unwrap_or(limit.preferred));
        if !(close(min, target.min) && close(max, target.max) && close(preferred, target.preferred)) {
            fields.push(field(
                target.quality.clone(),
                Some(show(min, preferred, max)),
                Some(show(target.min, target.preferred, target.max)),
            ));
        }
    }
    (!fields.is_empty()).then(|| Change {
        id: change_id(app, "sizes", &sizes.kind),
        app,
        kind: Kind::QualityDefinition,
        action: Action::Update,
        name: format!("Quality sizes ({})", sizes.kind),
        trash_id: None,
        arr_id: None,
        fields,
        requires: Vec::new(),
    })
}

pub fn diff(app: App, desired: &Desired<'_>, live: &Live, owned: &Owned, delete_unmanaged: bool) -> Diff {
    let mut out = Diff { problems: desired.problems.clone(), ..Diff::default() };
    let mut matched = BTreeSet::new();
    for (trash_id, guide) in &desired.custom_formats {
        let found = match_cf(live, owned, trash_id, &guide.name);
        let (action, fields) = match found {
            None => (
                Action::Create,
                guide.specifications.iter().map(|s| field(format!("condition {}", s.name), None, Some(guide_spec_summary(s)))).collect(),
            ),
            Some(cf) => {
                matched.insert(cf.id);
                (Action::Update, cf_fields(guide, cf))
            }
        };
        if action == Action::Update && fields.is_empty() {
            out.in_sync += 1;
            continue;
        }
        out.changes.push(Change {
            id: change_id(app, "cf", trash_id),
            app,
            kind: Kind::CustomFormat,
            action,
            name: guide.name.clone(),
            trash_id: Some(trash_id.to_string()),
            arr_id: found.map(|cf| cf.id),
            fields,
            requires: Vec::new(),
        });
    }
    let cx = ProfileContext { app, desired, live, owned, known: known_qualities(&live.schema.typed) };
    for profile in &desired.profiles {
        let found = match_profile(live, owned, &profile.trash_id, &profile.name).map(|p| &p.typed);
        if let Some(found) = found {
            out.managed_profile_ids.push(found.id);
            if profile.compact {
                out.compact_profile_id = Some(found.id);
            }
        }
        for rung in &profile.ladder {
            let unknown = if rung.members.is_empty() { vec![&rung.name] } else { rung.members.iter().collect() };
            for name in unknown.into_iter().filter(|name| !cx.known.contains(name.as_str())) {
                out.problems.push(format!("{}: {} has no quality named {name}; it is left out", profile.name, app.label()));
            }
        }
        let (fields, requires) = profile_fields(&cx, profile, found);
        if found.is_some() && fields.is_empty() {
            out.in_sync += 1;
            continue;
        }
        out.changes.push(Change {
            id: change_id(app, "profile", &profile.trash_id),
            app,
            kind: Kind::QualityProfile,
            action: if found.is_some() { Action::Update } else { Action::Create },
            name: profile.name.clone(),
            trash_id: Some(profile.trash_id.clone()),
            arr_id: found.map(|p| p.id),
            fields,
            requires,
        });
    }
    match sizes_change(app, desired, live, &mut out.problems) {
        Some(change) => out.changes.push(change),
        None if desired.sizes.is_some() => out.in_sync += 1,
        None => {}
    }
    let created: BTreeSet<u32> = owned.custom_formats.values().copied().collect();
    for cf in live.custom_formats.iter().filter(|cf| !matched.contains(&cf.id)) {
        let ours = created.contains(&cf.id);
        if !(ours || delete_unmanaged) {
            continue;
        }
        let why = if ours {
            "created by FLINCH; no synced profile uses it"
        } else {
            "not FLINCH's; deleted because delete_unmanaged_custom_formats is on"
        };
        out.changes.push(Change {
            id: change_id(app, "cf-delete", &cf.id.to_string()),
            app,
            kind: Kind::CustomFormat,
            action: Action::Delete,
            name: cf.name.clone(),
            trash_id: owned.custom_formats.iter().find(|(_, id)| **id == cf.id).map(|(trash_id, _)| trash_id.clone()),
            arr_id: Some(cf.id),
            fields: vec![field("reason", None, Some(why.to_string()))],
            requires: Vec::new(),
        });
    }
    out
}
