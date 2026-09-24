//! TypeSafe's System One wire format: the typed-decision API JEV serves and
//! Kev, Nimble and Laya-compatible servers mirror (`POST /v1/systemone`).
//!
//! FLINCH speaks it both ways. `flinch-fit --against` asks
//! any such model a question; `flinch-web` answers FLINCH's own questions in
//! the same shape, so one client library works against all of them.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

/// The endpoint path, appended to a server's base URL.
pub const PATH: &str = "/v1/systemone";
/// How long one answer may take: JEV answers in under a second, a local 9B
/// model on a CPU can take tens of seconds.
pub const TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// What to judge. Every server reads text; FLINCH's own endpoint also
    /// accepts an object.
    pub state: serde_json::Value,
    pub questions: BTreeMap<String, Question>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// True or false: answered with the probability that it is true.
    Noul {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
    /// One of a closed set of options.
    Choice {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
        criteria: Criteria,
    },
    /// A level on an ordered scale.
    Score {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
        criteria: Vec<String>,
    },
}

/// Choice options: a list of names, or names mapped to a description.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Criteria {
    Names(Vec<String>),
    Described(BTreeMap<String, Option<String>>),
}

impl Criteria {
    pub fn names(&self) -> Vec<&str> {
        match self {
            Criteria::Names(names) => names.iter().map(String::as_str).collect(),
            Criteria::Described(described) => described.keys().map(String::as_str).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    #[serde(default)]
    pub model: String,
    pub answers: BTreeMap<String, Answer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    Noul {
        #[serde(alias = "probability")]
        noul: f32,
    },
    Choice {
        choice: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confidence: Option<f32>,
        #[serde(default)]
        probabilities: BTreeMap<String, f32>,
    },
    Score {
        score: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confidence: Option<f32>,
        #[serde(default)]
        probabilities: BTreeMap<String, f32>,
        #[serde(default)]
        legend: BTreeMap<String, String>,
    },
}

impl Response {
    /// The probability a `noul` answer gives; `None` when the answer is
    /// missing, of another type, or not a probability.
    pub fn noul(&self, key: &str) -> Option<f32> {
        match self.answers.get(key)? {
            Answer::Noul { noul } if (0.0..=1.0).contains(noul) => Some(*noul),
            _ => None,
        }
    }
}

/// Where a System One model is served.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Endpoint {
    /// Base URL, such as `https://api.typesafe.ai` or `http://localhost:8009`.
    pub base_url: String,
    pub model: Option<String>,
    /// Sent as a bearer token; never logged.
    pub api_key: Option<String>,
}

impl Endpoint {
    pub fn is_configured(&self) -> bool {
        !self.base_url.trim().is_empty()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SystemOneError {
    #[error("request failed: {0}")]
    Transport(#[source] reqwest::Error),
    #[error("HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("answer did not parse: {0}")]
    Shape(#[source] serde_json::Error),
}

/// Ask one state the given questions. The URL is stripped from transport
/// errors, like every other request FLINCH makes.
pub async fn ask(
    http: &reqwest::Client,
    endpoint: &Endpoint,
    state: serde_json::Value,
    questions: BTreeMap<String, Question>,
) -> Result<Response, SystemOneError> {
    let url = format!("{}{PATH}", endpoint.base_url.trim().trim_end_matches('/'));
    let request = Request { model: endpoint.model.clone().filter(|model| !model.is_empty()), state, questions };
    let mut builder = http.post(&url).timeout(TIMEOUT).json(&request);
    if let Some(key) = endpoint.api_key.as_deref().filter(|key| !key.is_empty()) {
        builder = builder.bearer_auth(key);
    }
    let response = builder.send().await.map_err(|error| SystemOneError::Transport(error.without_url()))?;
    let status = response.status();
    let body = response.text().await.map_err(|error| SystemOneError::Transport(error.without_url()))?;
    if !status.is_success() {
        return Err(SystemOneError::Status { status: status.as_u16(), body: body.chars().take(300).collect() });
    }
    serde_json::from_str(&body).map_err(SystemOneError::Shape)
}

/// The question FLINCH's panel asks every model: will anyone play this within
/// the horizon? One wording for all of them, so a comparison compares models,
/// not prompts. The answer is P(played); P(safe) is its complement.
pub fn played_within(horizon_days: f32) -> Question {
    Question::Noul {
        instructions: Some(format!(
            "The state describes one item (a movie, or one season of a show) in a single household's Plex library, as it \
             stood on a cut date. Every field is as of that cut date: days_on_disk is how long the files had been in the \
             library, played_fraction and days_since_last_play summarise the household's plays before the cut, and \
             nothing after the cut is shown. Will anyone in this household play this item within the {horizon_days:.0} \
             days after the cut?"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kev_style_response_parses_and_only_real_probabilities_count() {
        let body = r#"{"model":"kev-latest","answers":{
            "escalate":{"type":"noul","noul":0.93},
            "department":{"type":"choice","choice":"returns","confidence":0.21,"probabilities":{"returns":0.47,"billing":0.53}},
            "odd":{"type":"noul","noul":1.7}},"latency_ms":495}"#;
        let response: Response = serde_json::from_str(body).expect("Kev's documented shape");
        assert_eq!(response.noul("escalate"), Some(0.93));
        assert_eq!(response.noul("department"), None, "a choice is not a probability");
        assert_eq!(response.noul("odd"), None, "outside [0, 1] is not a probability");
        assert_eq!(response.noul("absent"), None);
    }
}
