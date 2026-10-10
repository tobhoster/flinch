//! Replay a PCD's SQL ops into an in-memory SQLite database and read it into
//! the sync's guide model, so desired, diff and apply serve both sources.
//!
//! The ops are the database's own SQL, so the database runs with what could
//! reach outside it shut: in memory, ATTACH limited to none (no file can be
//! opened or created), and extension loading compiled out (rusqlite without
//! `load_extension`).
//!
//! How the schema maps (<https://github.com/Dictionarry-Hub/schema/blob/main/ops/0.schema.sql>):
//! - a profile's qualities are best first by `position`; the row with
//!   `upgrade_until` is the cutoff; qualities go through
//!   `quality_api_mappings` to the app's own names;
//! - scores live per profile, so each profile is its own score set;
//! - a profile language of type `simple` is Radarr's profile language; `must`,
//!   `only` and `not` become a language format scored [`LANGUAGE_REJECT`], as
//!   Profilarr does;
//! - `{app}_quality_definitions` are size tables named by `name`.

use super::values::{self, Condition};
use super::{id, PcdError};
use crate::capacity::App;
use crate::trash::guide::{AppGuide, GuideCustomFormat, GuideProfile, GuideQualityItem, GuideSize, GuideSizeQuality, GuideSpecification};
use rusqlite::{params, Connection};
use std::collections::BTreeMap;

/// Far below any PCD profile's minimum score, so the release is refused.
pub const LANGUAGE_REJECT: i32 = -999_999;

/// Run every op in order; an op that fails names itself.
pub fn replay(ops: &[(String, String)]) -> Result<Connection, PcdError> {
    let db = Connection::open_in_memory().map_err(|source| PcdError::Sql { op: "open".to_string(), source })?;
    db.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_ATTACHED, 0).map_err(|source| PcdError::Sql { op: "limit".to_string(), source })?;
    for (name, sql) in ops {
        db.execute_batch(sql).map_err(|source| PcdError::Sql { op: name.clone(), source })?;
    }
    Ok(db)
}

fn arr(app: App) -> &'static str {
    app.label()
}

fn conditions(db: &Connection, app: App) -> rusqlite::Result<BTreeMap<String, Vec<Condition>>> {
    let mut statement = db.prepare(
        "SELECT c.custom_format_name, c.name, c.type, c.negate, c.required, r.pattern, l.language_name, l.except_language,
                COALESCE(s.source, res.resolution, qm.quality_modifier, rt.release_type, f.flag),
                COALESCE(sz.min_bytes, y.min_year), COALESCE(sz.max_bytes, y.max_year)
         FROM custom_format_conditions c
         LEFT JOIN condition_patterns p ON p.custom_format_name = c.custom_format_name AND p.condition_name = c.name
         LEFT JOIN regular_expressions r ON r.name = p.regular_expression_name
         LEFT JOIN condition_languages l ON l.custom_format_name = c.custom_format_name AND l.condition_name = c.name
         LEFT JOIN condition_sources s ON s.custom_format_name = c.custom_format_name AND s.condition_name = c.name
         LEFT JOIN condition_resolutions res ON res.custom_format_name = c.custom_format_name AND res.condition_name = c.name
         LEFT JOIN condition_quality_modifiers qm ON qm.custom_format_name = c.custom_format_name AND qm.condition_name = c.name
         LEFT JOIN condition_release_types rt ON rt.custom_format_name = c.custom_format_name AND rt.condition_name = c.name
         LEFT JOIN condition_indexer_flags f ON f.custom_format_name = c.custom_format_name AND f.condition_name = c.name
         LEFT JOIN condition_sizes sz ON sz.custom_format_name = c.custom_format_name AND sz.condition_name = c.name
         LEFT JOIN condition_years y ON y.custom_format_name = c.custom_format_name AND y.condition_name = c.name
         WHERE c.arr_type IN ('all', ?1)
         ORDER BY c.custom_format_name, c.id",
    )?;
    let mut out: BTreeMap<String, Vec<Condition>> = BTreeMap::new();
    let rows = statement.query_map(params![arr(app)], |row| {
        let language: Option<String> = row.get(6)?;
        let except: Option<bool> = row.get(7)?;
        let condition = Condition {
            name: row.get(1)?,
            kind: row.get(2)?,
            negate: row.get(3)?,
            required: row.get(4)?,
            pattern: row.get(5)?,
            language: language.map(|name| (name, except.unwrap_or(false))),
            value: row.get(8)?,
            range: (row.get(9)?, row.get(10)?),
        };
        Ok((row.get::<_, String>(0)?, condition))
    })?;
    for row in rows {
        let (format, condition) = row?;
        out.entry(format).or_default().push(condition);
    }
    Ok(out)
}

fn custom_formats(db: &Connection, app: App, guide: &mut AppGuide) -> Result<(), rusqlite::Error> {
    let mut conditions = conditions(db, app)?;
    let mut statement = db.prepare("SELECT name, include_in_rename FROM custom_formats ORDER BY name")?;
    let formats = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)))?;
    for format in formats {
        let (name, rename) = format?;
        let trash_id = id("cf", &name);
        let specs: Result<Vec<GuideSpecification>, String> =
            conditions.remove(&name).unwrap_or_default().iter().map(|c| values::specification(app, c)).collect();
        match specs {
            Ok(specs) if specs.is_empty() => {
                guide.skipped.insert(trash_id, format!("{name} has no condition for {}", app.label()));
            }
            Ok(specifications) => {
                let format = GuideCustomFormat {
                    trash_id: trash_id.clone(),
                    name,
                    trash_scores: BTreeMap::new(),
                    include_when_renaming: rename,
                    specifications,
                };
                guide.custom_formats.insert(trash_id, format);
            }
            Err(why) => {
                guide.skipped.insert(trash_id, format!("{name}: {why}"));
            }
        }
    }
    Ok(())
}

/// Profilarr's language rule as a format: it matches what the rule refuses.
fn language_format(app: App, kind: &str, language: &str) -> Option<GuideCustomFormat> {
    let id_value = values::language_id(app, language)?;
    let (title, negate, except) = match kind {
        "must" => ("Must", true, false),
        "only" => ("Only", false, true),
        "not" => ("Not", false, false),
        _ => return None,
    };
    let name = format!("Language: {title} {language}");
    let fields = [("value".to_string(), serde_json::json!(id_value)), ("exceptLanguage".to_string(), serde_json::json!(except))]
        .into_iter()
        .collect();
    let spec =
        GuideSpecification { name: name.clone(), implementation: "LanguageSpecification".to_string(), negate, required: false, fields };
    Some(GuideCustomFormat {
        trash_id: id("cf", &name),
        name,
        trash_scores: BTreeMap::new(),
        include_when_renaming: false,
        specifications: vec![spec],
    })
}

fn api_names(db: &Connection, app: App) -> rusqlite::Result<BTreeMap<String, String>> {
    let mut statement = db.prepare("SELECT quality_name, api_name FROM quality_api_mappings WHERE arr_type = ?1")?;
    let rows = statement.query_map(params![arr(app)], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

struct ProfileRow {
    name: String,
    upgrades: bool,
    min_score: i32,
    until_score: i32,
    increment: i32,
}

fn ladder(db: &Connection, profile: &str, names: &BTreeMap<String, String>) -> rusqlite::Result<(Vec<GuideQualityItem>, Option<String>)> {
    let mut rows = db.prepare(
        "SELECT quality_name, quality_group_name, enabled, upgrade_until FROM quality_profile_qualities WHERE quality_profile_name = ?1 ORDER BY position, id",
    )?;
    let mut members = db.prepare(
        "SELECT quality_name FROM quality_group_members WHERE quality_profile_name = ?1 AND quality_group_name = ?2 ORDER BY position, quality_name",
    )?;
    let mut items = Vec::new();
    let mut cutoff = None;
    let entries: Vec<(Option<String>, Option<String>, bool, bool)> = rows
        .query_map(params![profile], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (quality, group, allowed, until) in entries {
        let item = match (quality, group) {
            (Some(quality), _) => names.get(&quality).map(|name| GuideQualityItem { name: name.clone(), allowed, items: Vec::new() }),
            (None, Some(group)) => {
                let inside: Vec<String> = members
                    .query_map(params![profile, group], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
                    .iter()
                    .filter_map(|q| names.get(q).cloned())
                    .collect();
                (!inside.is_empty()).then_some(GuideQualityItem { name: group, allowed, items: inside })
            }
            (None, None) => None,
        };
        if let Some(item) = item {
            if until {
                cutoff = Some(item.name.clone());
            }
            items.push(item);
        }
    }
    Ok((items, cutoff))
}

fn profiles(db: &Connection, app: App, guide: &mut AppGuide) -> rusqlite::Result<()> {
    let names = api_names(db, app)?;
    let mut statement =
        db.prepare("SELECT name, upgrades_allowed, minimum_custom_format_score, upgrade_until_score, upgrade_score_increment FROM quality_profiles ORDER BY name")?;
    let rows: Vec<ProfileRow> = statement
        .query_map([], |row| {
            Ok(ProfileRow {
                name: row.get(0)?,
                upgrades: row.get(1)?,
                min_score: row.get(2)?,
                until_score: row.get(3)?,
                increment: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut scores = db.prepare(
        "SELECT custom_format_name, score FROM quality_profile_custom_formats WHERE quality_profile_name = ?1 AND arr_type IN ('all', ?2) ORDER BY arr_type = 'all' DESC",
    )?;
    let mut languages = db.prepare("SELECT language_name, type FROM quality_profile_languages WHERE quality_profile_name = ?1")?;
    for row in rows {
        let profile_id = id("profile", &row.name);
        let (items, cutoff) = ladder(db, &row.name, &names)?;
        let Some(cutoff) = cutoff.or_else(|| items.iter().find(|item| item.allowed).map(|item| item.name.clone())) else {
            guide.skipped.insert(profile_id, format!("{} has no quality {} knows", row.name, app.label()));
            continue;
        };
        let mut format_items = BTreeMap::new();
        for scored in scores.query_map(params![row.name, arr(app)], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i32>(1)?)))? {
            let (format, score) = scored?;
            let format_id = id("cf", &format);
            if let Some(found) = guide.custom_formats.get_mut(&format_id) {
                found.trash_scores.insert(profile_id.clone(), score);
            }
            format_items.insert(format, format_id);
        }
        let mut language = None;
        for rule in languages.query_map(params![row.name], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (name, kind) = rule?;
            if kind == "simple" {
                language = (app == App::Radarr).then_some(name);
                continue;
            }
            match language_format(app, &kind, &name) {
                Some(format) => {
                    format_items.insert(format.name.clone(), format.trash_id.clone());
                    guide
                        .custom_formats
                        .entry(format.trash_id.clone())
                        .or_insert(format)
                        .trash_scores
                        .insert(profile_id.clone(), LANGUAGE_REJECT);
                }
                None => {
                    guide.skipped.insert(
                        id("cf", &format!("{kind} {name}")),
                        format!("{}: language rule {kind} {name} has no {} counterpart", row.name, app.label()),
                    );
                }
            }
        }
        let profile = GuideProfile {
            trash_id: profile_id.clone(),
            name: row.name,
            trash_score_set: Some(profile_id.clone()),
            upgrade_allowed: row.upgrades,
            cutoff,
            min_format_score: row.min_score,
            cutoff_format_score: row.until_score,
            min_upgrade_format_score: Some(row.increment.max(1)),
            language,
            items,
            format_items,
        };
        guide.profiles.insert(profile_id, profile);
    }
    Ok(())
}

fn sizes(db: &Connection, app: App, guide: &mut AppGuide) -> rusqlite::Result<()> {
    let names = api_names(db, app)?;
    let table = format!("{}_quality_definitions", arr(app));
    let mut statement =
        db.prepare(&format!("SELECT name, quality_name, min_size, max_size, preferred_size FROM {table} ORDER BY name, quality_name"))?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, f64>(2)?, row.get::<_, f64>(3)?, row.get::<_, f64>(4)?))
    })?;
    for row in rows {
        let (kind, quality, min, max, preferred) = row?;
        let Some(quality) = names.get(&quality).cloned() else { continue };
        let entry = GuideSizeQuality { quality, min, preferred: Some(preferred), max: Some(max) };
        match guide.sizes.iter_mut().find(|size| size.kind == kind) {
            Some(size) => size.qualities.push(entry),
            None => guide.sizes.push(GuideSize { trash_id: id("size", &kind), kind, qualities: vec![entry] }),
        }
    }
    Ok(())
}

/// One app's view of the replayed database.
pub fn app_guide(db: &Connection, app: App) -> Result<AppGuide, PcdError> {
    let mut guide = AppGuide::default();
    let read = |what: &str, source| PcdError::Sql { op: format!("reading {what} for {}", app.label()), source };
    custom_formats(db, app, &mut guide).map_err(|e| read("custom formats", e))?;
    profiles(db, app, &mut guide).map_err(|e| read("quality profiles", e))?;
    sizes(db, app, &mut guide).map_err(|e| read("quality definitions", e))?;
    Ok(guide)
}
