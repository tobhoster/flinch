//! The taste judge's verdicts: the question taste asks, the answers cache,
//! and the lookup the pure taste functions go through.
//!
//! The question is one yes/no ("noul") per audience and title: given the
//! title's description and its nearest titles, each marked by whether that
//! audience played it, would they play this one within the horizon? The
//! state is booleans and texts, never decayed rates or dates, so the same
//! library and outcomes ask the same question day after day and its answer
//! is looked up instead of asked again.
//!
//! Answers live in `taste_answers.json` in the state directory, keyed by the
//! SHA-256 of (model, [`QUESTION_VERSION`], the canonical state). A miss is
//! never asked inline: the lookup ([`Judge`]) only records it, the daemon's
//! taste step asks the pending questions within the daily budget and writes
//! the answers, and the next lookup finds them. Answers nobody looked up for
//! [`KEEP_DAYS`] are pruned, and a new judge model starts the cache over.

use crate::ollama::{Question, Questions};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The answers cache, in the state directory.
pub const ANSWERS_FILE: &str = "taste_answers.json";
/// Bumped whenever the question or the shape of its state changes, which
/// invalidates every cached answer.
pub const QUESTION_VERSION: u32 = 1;
/// The one question asked, by name.
pub const QUESTION: &str = "would_play";
/// The Ollama model judging taste unless the operator names another.
pub const DEFAULT_MODEL: &str = "kev:4b-q4_K_M";
/// Bounds of [`TasteConfig::judge_daily_budget`].
pub const MAX_DAILY_BUDGET: u32 = 20_000;
/// Days an answer (or a pending question) stays without being looked up.
pub const KEEP_DAYS: u64 = 30;
/// Most questions waiting at once. A state carries a description of up to
/// 2,000 characters and 20 titles (about 2.5 KB), and the panel alone can ask
/// one per never-played row, cut and viewer — some 100,000 for 3,000 titles
/// and 3 viewers, far more than a daily budget asks. Capped, the file stays
/// near 2.5 MB of waiting questions; the panel's give way to the cycle's, and
/// what was dropped is asked for again by the next lookup.
pub const MAX_PENDING: usize = 1_000;
const DAY_SECS: u64 = 86_400;
/// The longest model name accepted.
const MAX_MODEL_CHARS: usize = 200;

/// Taste as the operator configured it (`settings.json` `taste`). Every field
/// defaults individually; a file still holding the removed `half_life_days`
/// loads and ignores it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TasteConfig {
    /// Ask the judge model. Off, taste is absent everywhere (0, no evidence).
    pub judge_enabled: bool,
    /// The Ollama decision model answering `/v1/systemone` questions
    /// ([`DEFAULT_MODEL`], or another such as `clef-flash`, `clef` or
    /// `clef:cloud`).
    pub judge_model: String,
    /// Most questions asked per UTC day.
    pub judge_daily_budget: u32,
}

impl Default for TasteConfig {
    fn default() -> Self {
        Self { judge_enabled: false, judge_model: DEFAULT_MODEL.to_string(), judge_daily_budget: 500 }
    }
}

/// A [`TasteConfig`] outside its bounds; the message names the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidTasteConfig(pub &'static str);

impl TasteConfig {
    pub fn validate(&self) -> Result<(), InvalidTasteConfig> {
        let model = self.judge_model.trim();
        if model.is_empty() || model.chars().count() > MAX_MODEL_CHARS || model.chars().any(char::is_whitespace) {
            return Err(InvalidTasteConfig("the taste judge model (taste.judge_model) must be an Ollama model name, without spaces"));
        }
        if !(1..=MAX_DAILY_BUDGET).contains(&self.judge_daily_budget) {
            return Err(InvalidTasteConfig("the taste judge's daily budget (taste.judge_daily_budget) must be 1 to 20000 questions"));
        }
        Ok(())
    }

    /// The judge model asked, trimmed; `None` while the judge is off.
    pub fn active_model(&self) -> Option<&str> {
        self.judge_enabled.then(|| self.judge_model.trim())
    }
}

/// What the judge is shown about one title for one audience. Fields are declared
/// in alphabetical order, so the serialised state is canonical.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// The nearest titles with a closed outcome for the audience, nearest first.
    pub nearest: Vec<Neighbour>,
    /// The title's description ([`crate::embedding::movie_text`]).
    pub title: String,
}

/// One nearest title: its name and whether the audience played it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Neighbour {
    pub played: bool,
    pub title: String,
}

/// The question, as Ollama's `/v1/systemone` takes it.
pub fn questions() -> Questions {
    let instructions = format!(
        "The nearest titles are ones this viewer had in their library, each marked by whether they played it. Judging by \
         those, would this viewer play the title described in `title` within {} days of it arriving?",
        crate::regret::HORIZON_DAYS
    );
    Questions::from([(QUESTION.to_string(), Question::Noul { instructions })])
}

/// The cache key of `state` asked of `model`: hex SHA-256 of the model, the
/// question version and the canonical state.
pub fn key(model: &str, state: &State) -> String {
    let canonical = serde_json::to_vec(state).unwrap_or_default();
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    context.update(model.as_bytes());
    context.update(b"\n");
    context.update(QUESTION_VERSION.to_string().as_bytes());
    context.update(b"\n");
    context.update(&canonical);
    context.finish().as_ref().iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug, thiserror::Error)]
pub enum AnswersError {
    #[error("{ANSWERS_FILE} unreadable: {0}")]
    Io(#[from] std::io::Error),
    #[error("{ANSWERS_FILE} malformed: {0}")]
    Json(#[from] serde_json::Error),
}

/// `taste_answers.json`: the model the answers are from, every answer by
/// key, the questions still to ask, and the day's asking count (so a restart
/// does not reset the daily budget).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnswerCache {
    #[serde(default)]
    model: String,
    #[serde(default)]
    entries: BTreeMap<String, Verdict>,
    #[serde(default)]
    pending: BTreeMap<String, Pending>,
    #[serde(default)]
    budget: Budget,
}

/// One answer: the probability of yes, and the last UTC day (days since the
/// epoch) a lookup used it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct Verdict {
    p: f64,
    used_day: u64,
}

/// A question still to ask, the last UTC day a lookup wanted it, and whether
/// only the daily refit's panel asked it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    state: State,
    wanted_day: u64,
    #[serde(default)]
    panel: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct Budget {
    day: u64,
    used: u32,
}

impl AnswerCache {
    /// The cache in `state_dir`; a missing file is an empty cache.
    pub fn read(state_dir: &Path) -> Result<Self, AnswersError> {
        match std::fs::read_to_string(state_dir.join(ANSWERS_FILE)) {
            Ok(text) => Ok(serde_json::from_str(&text)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn write(&self, state_dir: &Path) -> Result<(), AnswersError> {
        crate::persist::replace(&state_dir.join(ANSWERS_FILE), &serde_json::to_vec(self)?)?;
        Ok(())
    }

    /// The model every answer came from; empty when unknown.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Point the cache at `model`: another model's answers are another
    /// judge's, so any change empties it. Returns whether it changed. The
    /// day's budget use stands.
    pub fn retarget(&mut self, model: &str) -> bool {
        if self.model == model {
            return false;
        }
        self.entries.clear();
        self.pending.clear();
        self.model = model.to_string();
        true
    }

    /// Answers held.
    pub fn answered(&self) -> usize {
        self.entries.len()
    }

    /// Questions waiting to be asked.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Record what the cycle's lookups (cards, inflow) used and missed on the
    /// UTC day of `now`. Returns whether anything changed.
    pub fn note(&mut self, asked: &Asked, now: u64) -> bool {
        self.note_from(asked, now, false)
    }

    /// Record what the refit's panel rows used and missed. Panel questions
    /// only take free room under [`MAX_PENDING`]; the cycle's push them out.
    pub fn note_panel(&mut self, asked: &Asked, now: u64) -> bool {
        self.note_from(asked, now, true)
    }

    /// Days only move forward: an offline fit pinned to a past `now` never
    /// ages an answer the daemon used today.
    fn note_from(&mut self, asked: &Asked, now: u64, panel: bool) -> bool {
        let day = now / DAY_SECS;
        let mut changed = false;
        for key in &asked.used {
            if let Some(verdict) = self.entries.get_mut(key).filter(|verdict| verdict.used_day < day) {
                verdict.used_day = day;
                changed = true;
            }
        }
        for (key, state) in &asked.missing {
            if self.entries.contains_key(key) {
                continue;
            }
            if let Some(pending) = self.pending.get_mut(key) {
                if pending.wanted_day < day {
                    pending.wanted_day = day;
                    changed = true;
                }
                if pending.panel && !panel {
                    pending.panel = false;
                    changed = true;
                }
                continue;
            }
            if self.pending.len() >= MAX_PENDING {
                let evicted = self.pending.iter().rev().find(|(_, pending)| pending.panel).map(|(key, _)| key.clone());
                match evicted.filter(|_| !panel) {
                    Some(evicted) => self.pending.remove(&evicted),
                    None => continue,
                };
            }
            self.pending.insert(key.clone(), Pending { state: state.clone(), wanted_day: day, panel });
            changed = true;
        }
        changed
    }

    /// Every pending question in asking order: the keys in `first` (this
    /// cycle's cards), then the cycle's earlier ones, then the panel's, each
    /// group by key.
    pub fn queue(&self, first: &Asked) -> Vec<(String, State)> {
        let rank = |key: &str, pending: &Pending| (!first.missing.contains_key(key), pending.panel);
        let mut queue: Vec<(&String, &Pending)> = self.pending.iter().collect();
        queue.sort_by_key(|(key, pending)| rank(key, pending));
        queue.into_iter().map(|(key, pending)| (key.clone(), pending.state.clone())).collect()
    }

    /// Keep the judge's answer `p` to the question under `key`, asked on the UTC
    /// day of `now`.
    pub fn answer(&mut self, key: String, p: f64, now: u64) {
        self.pending.remove(&key);
        self.entries.insert(key, Verdict { p, used_day: now / DAY_SECS });
    }

    /// Drop answers and pending questions no lookup touched for [`KEEP_DAYS`].
    /// Returns whether anything was dropped.
    pub fn prune(&mut self, now: u64) -> bool {
        let oldest = (now / DAY_SECS).saturating_sub(KEEP_DAYS);
        let before = (self.entries.len(), self.pending.len());
        self.entries.retain(|_, verdict| verdict.used_day >= oldest);
        self.pending.retain(|_, pending| pending.wanted_day >= oldest);
        before != (self.entries.len(), self.pending.len())
    }

    /// Questions that may still be asked on the UTC day of `now`.
    pub fn budget_left(&self, daily: u32, now: u64) -> u32 {
        if self.budget.day == now / DAY_SECS {
            daily.saturating_sub(self.budget.used)
        } else {
            daily
        }
    }

    /// Count `asked` questions against the UTC day of `now`.
    pub fn spend(&mut self, asked: u32, now: u64) {
        let day = now / DAY_SECS;
        if self.budget.day != day {
            self.budget = Budget { day, used: 0 };
        }
        self.budget.used = self.budget.used.saturating_add(asked);
    }

    fn p(&self, key: &str) -> Option<f64> {
        self.entries.get(key).map(|verdict| verdict.p)
    }
}

/// The lookup the pure taste functions go through: answers from the cache,
/// and a record of what was used and what is still to ask. No I/O.
pub struct Judge<'a> {
    /// `None` while the judge is switched off: nothing is answered or asked.
    cache: Option<&'a AnswerCache>,
    asked: Asked,
}

/// What lookups used and missed, for [`AnswerCache::note`].
#[derive(Debug, Clone, Default)]
pub struct Asked {
    used: BTreeSet<String>,
    missing: BTreeMap<String, State>,
}

impl Asked {
    /// Questions no answer was found for.
    pub fn missing(&self) -> usize {
        self.missing.len()
    }
}

impl<'a> Judge<'a> {
    /// The judge switched off: every lookup is unanswered and nothing is asked.
    pub fn off() -> Self {
        Self { cache: None, asked: Asked::default() }
    }

    /// Lookups in `cache`, under its model.
    pub fn new(cache: &'a AnswerCache) -> Self {
        Self { cache: Some(cache), asked: Asked::default() }
    }

    /// The judge's probability that the audience plays the title `state`
    /// describes; `None` until it has answered (the question is recorded).
    pub fn p(&mut self, state: State) -> Option<f64> {
        let cache = self.cache?;
        let key = key(&cache.model, &state);
        match cache.p(&key) {
            Some(p) => {
                self.asked.used.insert(key);
                Some(p)
            }
            None => {
                self.asked.missing.insert(key, state);
                None
            }
        }
    }

    /// What the lookups used and missed.
    pub fn finish(self) -> Asked {
        self.asked
    }
}

/// What status.json reports about the taste judge (`taste`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasteStatus {
    /// The judge is switched on; off, taste is absent everywhere.
    pub configured: bool,
    pub model: String,
    /// Answers in the cache.
    pub answered: usize,
    /// Questions waiting to be asked.
    pub pending: usize,
    /// Questions answered this cycle.
    pub asked: usize,
    /// Questions that may still be asked today (UTC).
    pub budget_left: u32,
    /// Why this cycle asked less than it could, in words.
    pub problem: Option<String>,
}

#[cfg(test)]
mod tests;
