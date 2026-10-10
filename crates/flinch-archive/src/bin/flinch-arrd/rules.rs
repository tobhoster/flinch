//! The operator's rules for one cycle: the facts they ask about, the
//! pre-rule inputs a preview re-plans from (`plan-inputs.json`), then the
//! rules applied to the candidates before the planner sees them.

use super::{state_dir, write_state};
use flinch_archive::capacity::VolumeForecast;
use flinch_archive::plan::candidates::Library;
use flinch_archive::plan::MediaCandidate;
use flinch_archive::rules::facts::{self, Sources};
use flinch_archive::rules::preview::{PlanInputs, INPUTS_FILE};
use flinch_archive::rules::{Rule, RulesStatus};

/// Conflicts named in the log; the status lists more.
const LOGGED_CONFLICTS: usize = 5;

/// Publish this cycle's inputs, then apply `rules` to `candidates`. The
/// inputs are written with or without rules, so the first preview works.
pub(super) fn apply(
    rules: &[Rule],
    candidates: &mut [MediaCandidate],
    library: &Library,
    sources: &Sources,
    forecasts: &[VolumeForecast],
    now: u64,
) -> Option<RulesStatus> {
    let inputs = PlanInputs {
        computed_at: now,
        forecasts: forecasts.to_vec(),
        candidates: candidates.to_vec(),
        facts: facts::gather(library, sources),
    };
    write_state(&state_dir().join(INPUTS_FILE), &inputs);
    if rules.is_empty() {
        return None;
    }
    let outcome = flinch_archive::rules::apply(rules, candidates, &inputs.facts, now);
    let kept: usize =
        candidates.iter().filter(|candidate| matches!(candidate.exclusion, Some(flinch_archive::plan::Exclusion::Rule(_)))).count();
    println!(
        "[flinch-arrd] rules: {} enabled, {kept} item(s) kept ({} for missing facts), {} forced, {} conflict(s)",
        outcome.rules.len(),
        outcome.uncertain,
        outcome.forced,
        outcome.conflicts.len()
    );
    for conflict in outcome.conflicts.iter().take(LOGGED_CONFLICTS) {
        eprintln!("[flinch-arrd] rules: {:?} kept by {:?} over {:?}", conflict.title, conflict.keep, conflict.evict);
    }
    RulesStatus::of(rules, outcome)
}
