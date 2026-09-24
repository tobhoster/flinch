//! `flinch-fit --against`: ask a System One server every panel row. The state
//! is the row's as-of state exactly as `--export-panel` writes it, sent as JSON
//! text because every server reads text; the one question is FLINCH's
//! canonical [`played_within`], keyed [`QUESTION`].

use super::QUESTION;
use crate::fit::export::AsOfState;
use crate::fit::panel::Example;
use crate::systemone::{self, played_within, Endpoint, Response, SystemOneError};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::task::{JoinError, JoinSet};

/// Why one row went unanswered.
#[derive(Debug, thiserror::Error)]
pub enum AskError {
    #[error("state did not serialise: {0}")]
    State(#[from] serde_json::Error),
    #[error(transparent)]
    SystemOne(#[from] SystemOneError),
    #[error("request task failed: {0}")]
    Task(#[from] JoinError),
}

/// Every row's answer in panel order (`None` where the row failed), with the
/// failures counted and the first one kept for the report.
#[derive(Debug)]
pub struct Asked {
    pub responses: Vec<Option<Response>>,
    pub failed: usize,
    pub first_error: Option<AskError>,
}

impl Asked {
    fn record(&mut self, done: Result<(usize, Result<Response, AskError>), JoinError>) {
        let error = match done {
            Ok((row, Ok(response))) => {
                if let Some(slot) = self.responses.get_mut(row) {
                    *slot = Some(response);
                }
                return;
            }
            Ok((_, Err(error))) => error,
            Err(error) => AskError::Task(error),
        };
        self.failed += 1;
        if self.first_error.is_none() {
            self.first_error = Some(error);
        }
    }

    /// The model that answered, as the server named it.
    pub fn model(&self) -> Option<&str> {
        self.responses.iter().flatten().map(|response| response.model.as_str()).find(|model| !model.is_empty())
    }
}

/// One request per panel row, at most `concurrency` (≥ 1) in flight. A failed
/// row is recorded, never fatal: the caller decides what "all failed" means.
pub async fn ask_panel(
    http: &reqwest::Client,
    endpoint: &Endpoint,
    dataset: &[Example],
    horizon_days: f32,
    concurrency: usize,
) -> Asked {
    let endpoint = Arc::new(endpoint.clone());
    let questions = BTreeMap::from([(QUESTION.to_string(), played_within(horizon_days))]);
    let mut asked = Asked { responses: vec![None; dataset.len()], failed: 0, first_error: None };
    let mut tasks = JoinSet::new();
    for (row, example) in dataset.iter().enumerate() {
        while tasks.len() >= concurrency.max(1) {
            match tasks.join_next().await {
                Some(done) => asked.record(done),
                None => break,
            }
        }
        let state = serde_json::to_string(&AsOfState::describe(&example.card, &example.ctx));
        let (http, endpoint, questions) = (http.clone(), Arc::clone(&endpoint), questions.clone());
        tasks.spawn(async move {
            let answer = match state {
                Ok(state) => systemone::ask(&http, &endpoint, serde_json::Value::String(state), questions).await.map_err(AskError::from),
                Err(error) => Err(AskError::from(error)),
            };
            (row, answer)
        });
    }
    while let Some(done) = tasks.join_next().await {
        asked.record(done);
    }
    asked
}
