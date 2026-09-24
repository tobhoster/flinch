//! FLINCH as a System One model: a TypeSafe-compatible client asks about one
//! library item the way it asks JEV, Kev or Laya (`POST /v1/systemone`), and
//! the published snapshot answers. Read-only, like the rest of the JSON API.
//!
//! FLINCH cannot read free-text instructions, so the question KEY carries the
//! meaning; `instructions` is accepted and ignored.

use flinch_archive::systemone::{Answer, Criteria, Question, Request};
use flinch_archive::ItemSnapshot;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Every question FLINCH answers, so a refusal teaches the client.
const ANSWERS: &str = "`safe` (noul: P(nobody plays it within the horizon)), \
     `played` (noul: 1 - safe), `decision` (choice: the plan's verdict, e.g. keep or delete)";

/// Why a request cannot be answered; every variant is the client's to fix.
#[derive(Debug, thiserror::Error)]
pub enum AskError {
    #[error("ask at least one question; FLINCH answers {}", ANSWERS)]
    NoQuestions,
    #[error("FLINCH does not answer {0:?}; it answers {answers}", answers = ANSWERS)]
    UnknownKey(String),
    #[error("question {key:?} must have type {expected:?}; FLINCH answers {}", ANSWERS)]
    WrongType { key: String, expected: &'static str },
    #[error(
        "state must name one item: an id such as \"radarr-7\", {{\"id\": …}}, \
         or {{\"title\": …, \"year\"?: …, \"season\"?: …}}"
    )]
    BadState,
    #[error("no item has id {0:?}")]
    UnknownId(String),
    #[error("{wanted} matches no item{}", same_title(.candidates))]
    NoMatch { wanted: String, candidates: Vec<String> },
    #[error("{wanted} matches {} items; ask by id: {}", .candidates.len(), .candidates.join(", "))]
    Ambiguous { wanted: String, candidates: Vec<String> },
    #[error("{id} has no forecast: {why}")]
    NoForecast { id: String, why: String },
    #[error("{id} is {decision:?}, which the criteria ({}) do not include", .offered.join(", "))]
    DecisionNotOffered { id: String, decision: String, offered: Vec<String> },
}

fn same_title(candidates: &[String]) -> String {
    if candidates.is_empty() {
        String::new()
    } else {
        format!("; items with that title: {}", candidates.join(", "))
    }
}

/// One question, understood by its key.
enum Ask<'q> {
    Safe,
    Played,
    Decision(&'q Criteria),
}

impl<'q> Ask<'q> {
    fn parse(key: &str, question: &'q Question) -> Result<Self, AskError> {
        let wrong = |expected| AskError::WrongType { key: key.to_string(), expected };
        match (key, question) {
            ("safe", Question::Noul { .. }) => Ok(Ask::Safe),
            ("played", Question::Noul { .. }) => Ok(Ask::Played),
            ("decision", Question::Choice { criteria, .. }) => Ok(Ask::Decision(criteria)),
            ("safe" | "played", _) => Err(wrong("noul")),
            ("decision", _) => Err(wrong("choice")),
            _ => Err(AskError::UnknownKey(key.to_string())),
        }
    }
}

/// Answer every question about the one item `request.state` names. Questions
/// are checked before the state, so a malformed question is reported even when
/// the item is unknown.
pub fn answer(items: &[ItemSnapshot], request: &Request) -> Result<BTreeMap<String, Answer>, AskError> {
    if request.questions.is_empty() {
        return Err(AskError::NoQuestions);
    }
    let asks = request
        .questions
        .iter()
        .map(|(key, question)| Ask::parse(key, question).map(|ask| (key, ask)))
        .collect::<Result<Vec<_>, AskError>>()?;
    let item = resolve(items, &request.state)?;
    asks.into_iter()
        .map(|(key, ask)| respond(item, &ask).map(|reply| (key.clone(), reply)))
        .collect()
}

fn respond(item: &ItemSnapshot, ask: &Ask) -> Result<Answer, AskError> {
    match ask {
        Ask::Safe => Ok(Answer::Noul { noul: p_safe(item)? }),
        Ask::Played => Ok(Answer::Noul { noul: 1.0 - p_safe(item)? }),
        Ask::Decision(criteria) => {
            let offered = criteria.names();
            if !offered.contains(&item.decision.as_str()) {
                return Err(AskError::DecisionNotOffered {
                    id: item.id.clone(),
                    decision: item.decision.clone(),
                    offered: offered.into_iter().map(str::to_string).collect(),
                });
            }
            // The plan is a rule, not a guess: the chosen option is certain and
            // every other offered option is impossible.
            let probabilities = offered
                .into_iter()
                .map(|name| (name.to_string(), if name == item.decision { 1.0 } else { 0.0 }))
                .collect();
            Ok(Answer::Choice { choice: item.decision.clone(), confidence: Some(1.0), probabilities })
        }
    }
}

/// The evidence-only forecast, else the gated score the plan used.
fn p_safe(item: &ItemSnapshot) -> Result<f32, AskError> {
    item.forecast
        .or(item.p_safe)
        .filter(|p| p.is_finite())
        .map(|p| p.clamp(0.0, 1.0))
        .ok_or_else(|| AskError::NoForecast {
            id: item.id.clone(),
            why: item
                .reasons
                .first()
                .cloned()
                .unwrap_or_else(|| "the daemon published no score for it".to_string()),
        })
}

fn resolve<'a>(items: &'a [ItemSnapshot], state: &Value) -> Result<&'a ItemSnapshot, AskError> {
    match state {
        Value::String(id) => by_id(items, id),
        Value::Object(fields) => match fields.get("id") {
            Some(Value::String(id)) => by_id(items, id),
            Some(_) => Err(AskError::BadState),
            None => by_title(items, fields),
        },
        _ => Err(AskError::BadState),
    }
}

fn by_id<'a>(items: &'a [ItemSnapshot], id: &str) -> Result<&'a ItemSnapshot, AskError> {
    let id = id.trim();
    items.iter().find(|item| item.id == id).ok_or_else(|| AskError::UnknownId(id.to_string()))
}

/// Exact, case-insensitive title match, narrowed by year and season. A season
/// row answers to its own title ("Severance S2") and to its show's title.
fn by_title<'a>(items: &'a [ItemSnapshot], fields: &Map<String, Value>) -> Result<&'a ItemSnapshot, AskError> {
    let Some(Value::String(title)) = fields.get("title") else {
        return Err(AskError::BadState);
    };
    let title = title.trim();
    let year = number(fields, "year")?;
    let season = number(fields, "season")?;
    let titled: Vec<&ItemSnapshot> = items
        .iter()
        .filter(|item| same(&item.title, title) || show_title(item).is_some_and(|show| same(show, title)))
        .collect();
    let matches: Vec<&ItemSnapshot> = titled
        .iter()
        .copied()
        .filter(|item| year.is_none_or(|year| item.year == Some(year)))
        .filter(|item| season.is_none_or(|season| season_of(item) == Some(season)))
        .collect();
    match matches.as_slice() {
        [item] => Ok(*item),
        [] => Err(AskError::NoMatch { wanted: wanted(title, year, season), candidates: ids(&titled) }),
        several => Err(AskError::Ambiguous { wanted: wanted(title, year, season), candidates: ids(several) }),
    }
}

fn wanted(title: &str, year: Option<u32>, season: Option<u32>) -> String {
    let mut out = format!("title {title:?}");
    if let Some(year) = year {
        out.push_str(&format!(", year {year}"));
    }
    if let Some(season) = season {
        out.push_str(&format!(", season {season}"));
    }
    out
}

fn ids(rows: &[&ItemSnapshot]) -> Vec<String> {
    rows.iter().map(|item| item.id.clone()).collect()
}

/// A year or season as a number or a string ("2", "S2"); `null` is absent.
fn number(fields: &Map<String, Value>, key: &str) -> Result<Option<u32>, AskError> {
    match fields.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n.as_u64().and_then(|n| u32::try_from(n).ok()).map(Some).ok_or(AskError::BadState),
        Some(Value::String(text)) => season_number(text).map(Some).ok_or(AskError::BadState),
        Some(_) => Err(AskError::BadState),
    }
}

/// "S2", "s02" or "2" → 2.
fn season_number(label: &str) -> Option<u32> {
    let label = label.trim();
    label.strip_prefix(['S', 's']).unwrap_or(label).parse().ok()
}

fn season_of(item: &ItemSnapshot) -> Option<u32> {
    item.season_label.as_deref().and_then(season_number)
}

/// A season row's title minus its label: "Severance S2" → "Severance".
fn show_title(item: &ItemSnapshot) -> Option<&str> {
    let label = item.season_label.as_deref()?;
    item.title.strip_suffix(label)?.strip_suffix(' ')
}

fn same(a: &str, b: &str) -> bool {
    a.chars().flat_map(char::to_lowercase).eq(b.chars().flat_map(char::to_lowercase))
}

/// The model name a reply carries: `flinch`, plus the scorecard the daemon
/// published in status.json, so a client sees when the model changed.
pub fn model_name(status_json: &str) -> String {
    let label = serde_json::from_str::<Value>(status_json)
        .ok()
        .and_then(|status| status.get("model")?.as_str().map(str::trim).map(str::to_string))
        .filter(|label| !label.is_empty());
    match label {
        Some(label) => format!("flinch ({label})"),
        None => "flinch".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use serde_json::json;

    fn row(id: &str, title: &str, extra: Value) -> Value {
        let mut row = json!({
            "id": id, "title": title, "kind": "movie", "size_bytes": 1, "decision": "keep",
            "reason": "", "delete_probability": 0.0, "protected": false,
        });
        if let (Some(row), Value::Object(extra)) = (row.as_object_mut(), extra) {
            row.extend(extra);
        }
        row
    }

    fn items() -> Vec<ItemSnapshot> {
        serde_json::from_value(json!([
            row("radarr-7", "Heat", json!({"year": 1995, "forecast": 0.8, "p_safe": 0.6, "decision": "delete"})),
            row("radarr-8", "Heat", json!({"year": 1986, "forecast": 0.4})),
            row("radarr-10", "Fallback", json!({"p_safe": 0.25})),
            row("radarr-9", "Gone", json!({"decision": "none", "reasons": ["no file on disk — nothing to reclaim"]})),
            row("sonarr-21-s1", "Severance S1", json!({"kind": "season", "season_label": "S1", "year": 2022, "forecast": 0.3})),
            row("sonarr-21-s2", "Severance S2", json!({"kind": "season", "season_label": "S2", "year": 2022, "forecast": 0.1})),
        ]))
        .unwrap()
    }

    fn request(state: Value, questions: Value) -> Request {
        serde_json::from_value(json!({"state": state, "questions": questions})).unwrap()
    }

    fn noul(answers: &BTreeMap<String, Answer>, key: &str) -> f32 {
        match answers.get(key) {
            Some(Answer::Noul { noul }) => *noul,
            other => panic!("{key}: expected a noul answer, got {other:?}"),
        }
    }

    #[rstest]
    #[case::forecast_wins_over_the_gated_score("radarr-7", 0.8)]
    #[case::gated_score_when_no_forecast("radarr-10", 0.25)]
    fn safe_and_played_are_complementary(#[case] id: &str, #[case] safe: f32) {
        let answers = answer(
            &items(),
            &request(json!(id), json!({"safe": {"type": "noul"}, "played": {"type": "noul"}})),
        )
        .unwrap();
        assert!((noul(&answers, "safe") - safe).abs() < 1e-6);
        assert!((noul(&answers, "played") - (1.0 - safe)).abs() < 1e-6);
    }

    #[rstest]
    #[case::bare_id(json!("sonarr-21-s1"), "sonarr-21-s1")]
    #[case::id_object(json!({"id": "radarr-7"}), "radarr-7")]
    #[case::title_and_year(json!({"title": "heat", "year": 1995}), "radarr-7")]
    #[case::show_and_season_number(json!({"title": "SEVERANCE", "season": 2}), "sonarr-21-s2")]
    #[case::show_and_season_label(json!({"title": "severance", "season": "S1"}), "sonarr-21-s1")]
    #[case::full_season_title(json!({"title": "Severance S2"}), "sonarr-21-s2")]
    fn each_state_form_names_one_item(#[case] state: Value, #[case] id: &str) {
        assert_eq!(resolve(&items(), &state).map(|item| item.id.as_str()).unwrap(), id);
    }

    #[test]
    fn an_ambiguous_title_names_its_candidates() {
        let error = resolve(&items(), &json!({"title": "Heat"})).unwrap_err();
        assert!(
            matches!(&error, AskError::Ambiguous { candidates, .. } if candidates == &["radarr-7", "radarr-8"]),
            "{error:?}"
        );
        let error = resolve(&items(), &json!({"title": "Severance", "season": 3})).unwrap_err();
        assert!(
            matches!(&error, AskError::NoMatch { candidates, .. } if candidates == &["sonarr-21-s1", "sonarr-21-s2"]),
            "{error:?}"
        );
    }

    #[test]
    fn decision_answers_the_plans_verdict_with_certainty() {
        let answers = answer(
            &items(),
            &request(json!("radarr-7"), json!({"decision": {"type": "choice", "criteria": ["keep", "delete"]}})),
        )
        .unwrap();
        let expected = Answer::Choice {
            choice: "delete".to_string(),
            confidence: Some(1.0),
            probabilities: BTreeMap::from([("delete".to_string(), 1.0), ("keep".to_string(), 0.0)]),
        };
        assert_eq!(answers.get("decision"), Some(&expected));
    }

    #[rstest]
    #[case::unknown_key(json!("radarr-7"), json!({"rating": {"type": "noul"}}), |e: &AskError| matches!(e, AskError::UnknownKey(_)))]
    #[case::known_key_wrong_type(json!("radarr-7"), json!({"safe": {"type": "choice", "criteria": ["yes", "no"]}}), |e: &AskError| matches!(e, AskError::WrongType { .. }))]
    #[case::decision_not_offered(json!("radarr-7"), json!({"decision": {"type": "choice", "criteria": ["keep"]}}), |e: &AskError| matches!(e, AskError::DecisionNotOffered { .. }))]
    #[case::nothing_on_disk(json!("radarr-9"), json!({"safe": {"type": "noul"}}), |e: &AskError| matches!(e, AskError::NoForecast { why, .. } if why.contains("no file on disk")))]
    #[case::unknown_id(json!("radarr-404"), json!({"safe": {"type": "noul"}}), |e: &AskError| matches!(e, AskError::UnknownId(_)))]
    #[case::state_is_not_an_item(json!(7), json!({"safe": {"type": "noul"}}), |e: &AskError| matches!(e, AskError::BadState))]
    fn unanswerable_requests_are_refused(#[case] state: Value, #[case] questions: Value, #[case] expected: fn(&AskError) -> bool) {
        let error = answer(&items(), &request(state, questions)).unwrap_err();
        assert!(expected(&error), "{error:?}");
    }
}
