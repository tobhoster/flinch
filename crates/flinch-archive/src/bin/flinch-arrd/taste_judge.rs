//! The taste judge's step: once a cycle, the judge model is asked the taste
//! questions still waiting in `taste_answers.json` — this cycle's cards
//! first, then what the daily refit's panel left — within
//! `taste.judge_daily_budget` and a per-cycle time budget, through the
//! operator's Ollama (`/v1/systemone`).
//!
//! The cycle's taste lookups are pure ([`flinch_archive::taste`]): they run
//! once to learn what is missing, the answers that fit the budgets are asked
//! and kept, and they run again on the answers. Nothing here fails the cycle:
//! Ollama unreachable, or without the model, leaves the cache as it was and
//! says why in the status, and the unanswered titles carry no taste.

use super::state_dir;
use flinch_archive::ollama::{self, Answer, OllamaConfig};
use flinch_archive::taste::{judge, AnswerCache, Judge, TasteConfig, TasteStatus};
use std::time::{Duration, Instant};

/// The longest one cycle spends asking: a judge model on CPU answers in
/// seconds (a 4B one) to a minute (a 27B one), and the plan waits for it.
const CYCLE_BUDGET: Duration = Duration::from_secs(180);

/// What the cycle's lookups returned on the answers, and the judge's status.
pub(super) struct Judged<T> {
    pub(super) out: T,
    pub(super) status: TasteStatus,
}

/// Run `read` (the cycle's taste lookups) on the answers cache, ask the
/// questions waiting within the budgets, and run `read` again on the answers.
/// Off, `read` runs once with the judge off and nothing is read or written.
pub(super) async fn judge<T>(
    http: &reqwest::Client,
    ollama: &OllamaConfig,
    config: &TasteConfig,
    now: u64,
    mut read: impl FnMut(&mut Judge<'_>) -> T,
) -> Judged<T> {
    let model = config.judge_model.trim();
    if !config.judge_enabled {
        let out = read(&mut Judge::off());
        println!("[flinch-arrd] taste: the judge is off, no title has taste");
        return Judged { out, status: TasteStatus { configured: false, model: model.to_string(), ..TasteStatus::default() } };
    }
    let dir = state_dir();
    let mut problem = None;
    let mut cache = AnswerCache::read(&dir).unwrap_or_else(|error| {
        problem = Some(format!("{error}; the cache starts over"));
        AnswerCache::default()
    });
    let mut changed = cache.retarget(model);
    let asked = {
        let mut judge = Judge::new(&cache);
        read(&mut judge);
        judge.finish()
    };
    changed |= cache.note(&asked, now);
    let budget = cache.budget_left(config.judge_daily_budget, now) as usize;
    let questions = judge::questions();
    let started = Instant::now();
    let mut answered = 0usize;
    for (key, state) in cache.queue(&asked).into_iter().take(budget) {
        if started.elapsed() >= CYCLE_BUDGET {
            break;
        }
        let state = match serde_json::to_value(&state) {
            Ok(state) => state,
            Err(error) => {
                problem = Some(format!("a taste question could not be written: {error}"));
                break;
            }
        };
        match ollama::decide(http, ollama, model, &state, &questions).await {
            Ok(answers) => {
                if let Some(Answer::Noul { noul }) = answers.get(judge::QUESTION) {
                    cache.answer(key, *noul, now);
                    answered += 1;
                }
            }
            Err(error) => {
                problem = Some(error.to_string());
                break;
            }
        }
    }
    cache.spend(answered as u32, now);
    changed |= answered > 0;
    changed |= cache.prune(now);
    if changed {
        if let Err(error) = cache.write(&dir) {
            problem = Some(format!("{} write failed: {error}", judge::ANSWERS_FILE));
        }
    }
    let out = read(&mut Judge::new(&cache));
    let status = TasteStatus {
        configured: true,
        model: model.to_string(),
        answered: cache.answered(),
        pending: cache.pending(),
        asked: answered,
        budget_left: cache.budget_left(config.judge_daily_budget, now),
        problem,
    };
    println!(
        "[flinch-arrd] taste: {} answers from {} · {} asked this cycle · {} still to ask · {} left today",
        status.answered, status.model, status.asked, status.pending, status.budget_left,
    );
    if let Some(problem) = &status.problem {
        eprintln!("[flinch-arrd] taste: {problem}");
    }
    Judged { out, status }
}
