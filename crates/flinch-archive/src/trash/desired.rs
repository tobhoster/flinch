//! What one app should look like: the operator's config laid over the pinned
//! guide (TRaSH-Guides or a Profilarr Compliant Database, read into the same
//! model). Scores layer like Recyclarr's: the guide's score for the profile's
//! score set, scaled by the profile's `score_multiplier`, then the operator's
//! absolute override or relative adjustment, then the language preset.
//! Anything the config names that the guide lacks is a problem the preview
//! shows, never a guess.

use super::config::{InstanceConfig, ProfileConfig};
use super::guide::{AppGuide, GuideCustomFormat, GuideProfile};
use crate::capacity::App;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub struct Desired<'g> {
    /// Every custom format some profile scores, by trash id.
    pub custom_formats: BTreeMap<&'g str, &'g GuideCustomFormat>,
    pub profiles: Vec<DesiredProfile>,
    pub sizes: Option<DesiredSizes>,
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredProfile {
    pub trash_id: String,
    pub name: String,
    pub compact: bool,
    pub upgrade_allowed: bool,
    /// The rung upgrades stop at, by name.
    pub cutoff: String,
    pub min_format_score: i32,
    pub cutoff_format_score: i32,
    pub min_upgrade_format_score: i32,
    pub language: Option<String>,
    /// Best first. Qualities not on it are added disabled below it.
    pub ladder: Vec<Rung>,
    /// Custom format trash id → score.
    pub scores: BTreeMap<String, i32>,
    pub reset_unmatched_scores: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rung {
    pub name: String,
    /// A group's qualities, best first; empty for a single quality.
    pub members: Vec<String>,
    pub allowed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredSizes {
    pub kind: String,
    pub qualities: Vec<SizeTarget>,
}

/// MB per minute, as the *arrs store them.
#[derive(Debug, Clone, PartialEq)]
pub struct SizeTarget {
    pub quality: String,
    pub min: f64,
    pub max: f64,
    pub preferred: f64,
}

/// The highest max and preferred size each app accepts, as Recyclarr holds
/// them for current releases (Radarr ≥ 5.9, Sonarr ≥ 4.0.8:
/// <https://github.com/recyclarr/recyclarr/tree/master/src/Recyclarr.Cli/Pipelines/QualitySize/PipelinePhases/Limits>).
/// A server's `null` reads as these.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SizeLimits {
    pub max: f64,
    pub preferred: f64,
}

pub fn limits(app: App) -> SizeLimits {
    match app {
        App::Radarr => SizeLimits { max: 2000.0, preferred: 1999.0 },
        App::Sonarr => SizeLimits { max: 1000.0, preferred: 995.0 },
    }
}

/// The guide's score for a format in a score set, falling back to `default`.
fn guide_score(format: &GuideCustomFormat, score_set: Option<&str>) -> Option<i32> {
    score_set.and_then(|set| format.trash_scores.get(set)).or_else(|| format.trash_scores.get("default")).copied()
}

fn ladder(config: &ProfileConfig, guide: &GuideProfile) -> Vec<Rung> {
    if config.qualities.is_empty() {
        return guide
            .items
            .iter()
            .map(|item| Rung { name: item.name.clone(), members: item.items.clone(), allowed: item.allowed })
            .collect();
    }
    config.qualities.iter().map(|q| Rung { name: q.name.clone(), members: q.qualities.clone(), allowed: q.enabled }).collect()
}

fn profile<'g>(
    config: &ProfileConfig,
    guide: &'g AppGuide,
    instance: &InstanceConfig,
    app: App,
    problems: &mut Vec<String>,
) -> Option<(DesiredProfile, Vec<&'g str>)> {
    let Some(base) = guide.profiles.get(&config.trash_id) else {
        let reason = guide.skipped.get(&config.trash_id).map_or_else(String::new, |why| format!(" ({why})"));
        // A PCD names no trash ids: the list is how an operator finds them.
        let known: Vec<String> = guide.profiles.values().map(|p| format!("{} = {}", p.name, p.trash_id)).collect();
        problems.push(format!(
            "quality profile {} is not in the guide at this commit{reason}; it has: {}",
            config.trash_id,
            known.join(", ")
        ));
        return None;
    };
    let score_set = base.trash_score_set.as_deref();
    // Recyclarr #208: a guide score scaled per profile; overrides stay absolute.
    let scaled = |score: i32| config.score_multiplier.map_or(score, |m| (f64::from(score) * m).round() as i32);
    let mut scores = BTreeMap::new();
    let mut used = Vec::new();
    for (name, trash_id) in &base.format_items {
        let Some((key, format)) = guide.custom_formats.get_key_value(trash_id) else {
            let reason = guide.skipped.get(trash_id).map_or_else(String::new, |why| format!(": {why}"));
            problems.push(format!("{}: custom format {name} ({trash_id}) is not in the guide{reason}", base.name));
            continue;
        };
        if let Some(score) = guide_score(format, score_set) {
            scores.insert(trash_id.clone(), scaled(score));
            used.push(key.as_str());
        }
    }
    let applies = |format: &&super::config::CustomFormatConfig| format.profiles.is_empty() || format.profiles.contains(&config.trash_id);
    for wanted in instance.custom_formats.iter().filter(applies) {
        let Some((key, format)) = guide.custom_formats.get_key_value(&wanted.trash_id) else {
            problems.push(format!("custom format {} is not in the guide at this commit", wanted.trash_id));
            continue;
        };
        let relative = || guide_score(format, score_set).map(|score| scaled(score).saturating_add(wanted.adjust_score.unwrap_or(0)));
        match wanted.score.or_else(relative) {
            Some(score) => {
                scores.insert(wanted.trash_id.clone(), score);
                used.push(key.as_str());
            }
            None => problems.push(format!("{}: custom format {} has no guide score; set one", base.name, format.name)),
        }
    }
    let mut min_format_score = base.min_format_score;
    let mut language = base.language.clone();
    if let Some(rule) = instance.language.as_ref().map(|preset| preset.rule(app)) {
        match guide.custom_formats.get_key_value(rule.trash_id) {
            Some((key, _)) => {
                scores.insert(rule.trash_id.to_string(), rule.score);
                used.push(key.as_str());
                min_format_score = min_format_score.saturating_add(rule.min_shift);
                if let Some(any) = rule.profile_language {
                    language = Some(any.to_string());
                }
            }
            None => problems
                .push(format!("{}: the language format {} is not in the guide; the language preset is skipped", base.name, rule.trash_id)),
        }
    }
    let upgrade = config.upgrade.as_ref();
    let desired = DesiredProfile {
        trash_id: config.trash_id.clone(),
        name: config.name.clone().unwrap_or_else(|| base.name.clone()),
        compact: config.compact,
        upgrade_allowed: upgrade.map_or(base.upgrade_allowed, |upgrade| upgrade.allowed),
        cutoff: upgrade.and_then(|upgrade| upgrade.until_quality.clone()).unwrap_or_else(|| base.cutoff.clone()),
        min_format_score,
        cutoff_format_score: upgrade.and_then(|upgrade| upgrade.until_score).unwrap_or(base.cutoff_format_score),
        min_upgrade_format_score: config.min_upgrade_format_score.or(base.min_upgrade_format_score).unwrap_or(1).max(1),
        language,
        ladder: ladder(config, base),
        scores,
        reset_unmatched_scores: config.reset_unmatched_scores,
    };
    if !desired.ladder.iter().any(|rung| rung.allowed && rung.name == desired.cutoff) {
        problems.push(format!(
            "{}: upgrades stop at {}, which is not an enabled quality of the profile; it is left as it is",
            desired.name, desired.cutoff
        ));
        return None;
    }
    Some((desired, used))
}

fn sizes(instance: &InstanceConfig, guide: &AppGuide, app: App, problems: &mut Vec<String>) -> Option<DesiredSizes> {
    let config = instance.quality_definition.as_ref()?;
    let Some(table) = guide.sizes.iter().find(|table| table.kind == config.kind) else {
        problems.push(format!("the guide has no {} size table for {}", config.kind, app.label()));
        return None;
    };
    let limit = limits(app);
    let qualities = table
        .qualities
        .iter()
        .map(|q| {
            let max = q.max.unwrap_or(limit.max).min(limit.max);
            let preferred = match config.preferred_ratio {
                // Recyclarr's formula, so a migrated config previews no change.
                Some(ratio) => ((q.min + (max.min(limit.preferred) - q.min) * ratio) * 10.0).round() / 10.0,
                None => q.preferred.unwrap_or(limit.preferred).min(limit.preferred),
            };
            SizeTarget { quality: q.quality.clone(), min: q.min, max, preferred }
        })
        .collect();
    Some(DesiredSizes { kind: config.kind.clone(), qualities })
}

pub fn resolve<'g>(instance: &InstanceConfig, guide: &'g AppGuide, app: App) -> Desired<'g> {
    let mut problems = Vec::new();
    let mut custom_formats = BTreeMap::new();
    let mut profiles = Vec::new();
    for config in &instance.quality_profiles {
        if let Some((desired, used)) = profile(config, guide, instance, app, &mut problems) {
            for trash_id in used {
                if let Some(format) = guide.custom_formats.get(trash_id) {
                    custom_formats.insert(trash_id, format);
                }
            }
            profiles.push(desired);
        }
    }
    let sizes = sizes(instance, guide, app, &mut problems);
    Desired { custom_formats, profiles, sizes, problems }
}
