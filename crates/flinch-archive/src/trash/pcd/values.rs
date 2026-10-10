//! A PCD condition as the *arr custom format specification it stands for.
//! PCDs name values (`web_dl`, `1080p`, `English`); the *arrs store enum
//! numbers, and Radarr's and Sonarr's numbers differ. Each table is copied
//! from the app's source (develop branch, 2026-10-10):
//! - languages: `src/NzbDrone.Core/Languages/Language.cs` of each app;
//! - sources: Radarr `Qualities/QualitySource.cs`, Sonarr `Qualities/QualitySource.cs`;
//! - modifiers: Radarr `Qualities/Modifier.cs`; release types: Sonarr
//!   `Parser/Model/ReleaseType.cs`;
//! - specifications per app: `CustomFormats/Specifications/` of each.
//!
//! A value or condition type without a counterpart is an error naming it:
//! the format is left out rather than synced half.

use crate::capacity::App;
use crate::trash::guide::GuideSpecification;
use serde_json::{json, Map, Value};

const RADARR_LANGUAGES: &[(&str, i64)] = &[
    ("Unknown", 0),
    ("English", 1),
    ("French", 2),
    ("Spanish", 3),
    ("German", 4),
    ("Italian", 5),
    ("Danish", 6),
    ("Dutch", 7),
    ("Japanese", 8),
    ("Icelandic", 9),
    ("Chinese", 10),
    ("Russian", 11),
    ("Polish", 12),
    ("Vietnamese", 13),
    ("Swedish", 14),
    ("Norwegian", 15),
    ("Finnish", 16),
    ("Turkish", 17),
    ("Portuguese", 18),
    ("Flemish", 19),
    ("Greek", 20),
    ("Korean", 21),
    ("Hungarian", 22),
    ("Hebrew", 23),
    ("Lithuanian", 24),
    ("Czech", 25),
    ("Hindi", 26),
    ("Romanian", 27),
    ("Thai", 28),
    ("Bulgarian", 29),
    ("Portuguese (Brazil)", 30),
    ("Arabic", 31),
    ("Ukrainian", 32),
    ("Persian", 33),
    ("Bengali", 34),
    ("Slovak", 35),
    ("Latvian", 36),
    ("Spanish (Latino)", 37),
    ("Catalan", 38),
    ("Croatian", 39),
    ("Serbian", 40),
    ("Bosnian", 41),
    ("Estonian", 42),
    ("Tamil", 43),
    ("Indonesian", 44),
    ("Telugu", 45),
    ("Macedonian", 46),
    ("Slovenian", 47),
    ("Malayalam", 48),
    ("Kannada", 49),
    ("Albanian", 50),
    ("Afrikaans", 51),
    ("Marathi", 52),
    ("Tagalog", 53),
    ("Urdu", 54),
    ("Romansh", 55),
    ("Mongolian", 56),
    ("Georgian", 57),
    ("Any", -1),
    ("Original", -2),
];

const SONARR_LANGUAGES: &[(&str, i64)] = &[
    ("Unknown", 0),
    ("English", 1),
    ("French", 2),
    ("Spanish", 3),
    ("German", 4),
    ("Italian", 5),
    ("Danish", 6),
    ("Dutch", 7),
    ("Japanese", 8),
    ("Icelandic", 9),
    ("Chinese", 10),
    ("Russian", 11),
    ("Polish", 12),
    ("Vietnamese", 13),
    ("Swedish", 14),
    ("Norwegian", 15),
    ("Finnish", 16),
    ("Turkish", 17),
    ("Portuguese", 18),
    ("Flemish", 19),
    ("Greek", 20),
    ("Korean", 21),
    ("Hungarian", 22),
    ("Hebrew", 23),
    ("Lithuanian", 24),
    ("Czech", 25),
    ("Arabic", 26),
    ("Hindi", 27),
    ("Bulgarian", 28),
    ("Malayalam", 29),
    ("Ukrainian", 30),
    ("Slovak", 31),
    ("Thai", 32),
    ("Portuguese (Brazil)", 33),
    ("Spanish (Latino)", 34),
    ("Romanian", 35),
    ("Latvian", 36),
    ("Persian", 37),
    ("Catalan", 38),
    ("Croatian", 39),
    ("Serbian", 40),
    ("Bosnian", 41),
    ("Estonian", 42),
    ("Tamil", 43),
    ("Indonesian", 44),
    ("Macedonian", 45),
    ("Slovenian", 46),
    ("Original", -2),
];

pub fn language_id(app: App, name: &str) -> Option<i64> {
    let table = match app {
        App::Radarr => RADARR_LANGUAGES,
        App::Sonarr => SONARR_LANGUAGES,
    };
    table.iter().find(|(known, _)| known.eq_ignore_ascii_case(name)).map(|(_, id)| *id)
}

fn source(app: App, value: &str) -> Option<i64> {
    match (app, value) {
        (App::Radarr, "cam") => Some(1),
        (App::Radarr, "telesync") => Some(2),
        (App::Radarr, "telecine") => Some(3),
        (App::Radarr, "workprint") => Some(4),
        (App::Radarr, "dvd") => Some(5),
        (App::Radarr, "television") => Some(6),
        (App::Radarr, "web_dl") => Some(7),
        (App::Radarr, "webrip") => Some(8),
        (App::Radarr, "bluray") => Some(9),
        (App::Sonarr, "television") => Some(1),
        (App::Sonarr, "television_raw") => Some(2),
        (App::Sonarr, "web_dl") => Some(3),
        (App::Sonarr, "webrip") => Some(4),
        (App::Sonarr, "dvd") => Some(5),
        (App::Sonarr, "bluray") => Some(6),
        (App::Sonarr, "bluray_raw") => Some(7),
        _ => None,
    }
}

/// Both apps' `Resolution` enums hold the height itself.
fn resolution(value: &str) -> Option<i64> {
    let height: i64 = value.strip_suffix('p')?.parse().ok()?;
    [360, 480, 540, 576, 720, 1080, 2160].contains(&height).then_some(height)
}

fn modifier(value: &str) -> Option<i64> {
    ["regional", "screener", "rawhd", "brdisk", "remux"].iter().position(|known| *known == value).and_then(|at| i64::try_from(at + 1).ok())
}

fn release_type(value: &str) -> Option<i64> {
    ["single_episode", "multi_episode", "season_pack"].iter().position(|known| *known == value).and_then(|at| i64::try_from(at + 1).ok())
}

/// One condition row with whatever detail table answered for it.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Condition {
    pub name: String,
    pub kind: String,
    pub negate: bool,
    pub required: bool,
    pub pattern: Option<String>,
    pub language: Option<(String, bool)>,
    pub value: Option<String>,
    pub range: (Option<i64>, Option<i64>),
}

const GIB: f64 = 1_073_741_824.0;

/// The specification for `app`, or why there is none.
pub fn specification(app: App, condition: &Condition) -> Result<GuideSpecification, String> {
    let missing = || format!("condition {} ({}) has no value", condition.name, condition.kind);
    let value = || condition.value.as_deref().ok_or_else(missing);
    let unknown = |v: &str| format!("condition {}: {} has no {} value {v}", condition.name, app.label(), condition.kind);
    let (implementation, fields): (&str, Vec<(&str, Value)>) = match (condition.kind.as_str(), app) {
        ("release_title", _) => ("ReleaseTitleSpecification", vec![("value", json!(condition.pattern.as_ref().ok_or_else(missing)?))]),
        ("release_group", _) => ("ReleaseGroupSpecification", vec![("value", json!(condition.pattern.as_ref().ok_or_else(missing)?))]),
        ("edition", App::Radarr) => ("EditionSpecification", vec![("value", json!(condition.pattern.as_ref().ok_or_else(missing)?))]),
        ("language", _) => {
            let (name, except) = condition.language.as_ref().ok_or_else(missing)?;
            let id = language_id(app, name).ok_or_else(|| unknown(name))?;
            ("LanguageSpecification", vec![("value", json!(id)), ("exceptLanguage", json!(except))])
        }
        ("source", _) => ("SourceSpecification", vec![("value", json!(value().and_then(|v| source(app, v).ok_or_else(|| unknown(v)))?))]),
        ("resolution", _) => {
            ("ResolutionSpecification", vec![("value", json!(value().and_then(|v| resolution(v).ok_or_else(|| unknown(v)))?))])
        }
        ("quality_modifier", App::Radarr) => {
            ("QualityModifierSpecification", vec![("value", json!(value().and_then(|v| modifier(v).ok_or_else(|| unknown(v)))?))])
        }
        ("release_type", App::Sonarr) => {
            ("ReleaseTypeSpecification", vec![("value", json!(value().and_then(|v| release_type(v).ok_or_else(|| unknown(v)))?))])
        }
        ("size", _) => {
            let gib = |bytes: Option<i64>, default: f64| bytes.map_or(default, |b| (b as f64 / GIB * 100.0).round() / 100.0);
            ("SizeSpecification", vec![("min", json!(gib(condition.range.0, 0.0))), ("max", json!(gib(condition.range.1, 1_000_000.0)))])
        }
        ("year", App::Radarr) => {
            ("YearSpecification", vec![("min", json!(condition.range.0.unwrap_or(0))), ("max", json!(condition.range.1.unwrap_or(9999)))])
        }
        (kind, _) => return Err(format!("condition {}: {} has no {kind} condition FLINCH maps", condition.name, app.label())),
    };
    let fields: Map<String, Value> = fields.into_iter().map(|(name, value)| (name.to_string(), value)).collect();
    Ok(GuideSpecification {
        name: condition.name.clone(),
        implementation: implementation.to_string(),
        negate: condition.negate,
        required: condition.required,
        fields,
    })
}
