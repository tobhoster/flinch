//! What a draft rule list would change, before it is saved: the planner run
//! twice on the inputs the daemon last published (`plan-inputs.json`), once
//! with the saved rules and once with the draft, and the difference.
//!
//! Both runs use the same candidates, facts and forecasts, so the diff is the
//! rules' doing alone, never a newer library's.

use super::{apply, Conflict, Facts, Outcome, Rule, RuleCount};
use crate::capacity::VolumeForecast;
use crate::plan::{generate_eviction_plan, EvictionPlan, Kept, MediaCandidate, PlanError, PlannerConfig};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Where the daemon writes [`PlanInputs`], in the state directory.
pub const INPUTS_FILE: &str = "plan-inputs.json";

/// Everything the planner read in one cycle, before any rule.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanInputs {
    /// The cycle's clock, unix seconds: `keep_until` windows count from it.
    pub computed_at: u64,
    pub forecasts: Vec<VolumeForecast>,
    pub candidates: Vec<MediaCandidate>,
    pub facts: HashMap<String, Facts>,
}

/// One plan's totals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Totals {
    pub items: usize,
    pub bytes: u64,
    pub regret: f64,
    /// Every volume's target is met.
    pub covered: bool,
}

impl Totals {
    fn of(plan: &EvictionPlan) -> Self {
        Self { items: plan.items.len(), bytes: plan.total_reclaimed_bytes, regret: plan.total_regret, covered: plan.covered() }
    }
}

/// An item one plan takes and the other does not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    pub id: String,
    pub title: String,
    pub size_bytes: u64,
    pub regret: f64,
    pub volume: String,
    /// Why the plan without it keeps it.
    pub kept_because: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preview {
    pub computed_at: u64,
    /// The plan under the saved rules, then under the draft.
    pub saved: Totals,
    pub draft: Totals,
    /// Taken under the draft only.
    pub added: Vec<Change>,
    /// Taken under the saved rules only.
    pub removed: Vec<Change>,
    pub conflicts: Vec<Conflict>,
    /// What each enabled draft rule covers.
    pub rules: Vec<RuleCount>,
    pub uncertain: usize,
    pub forced: usize,
}

/// Plan `inputs` under the saved rules and under `draft`, and diff the two.
/// The caller validates `draft` first.
pub fn preview(inputs: &PlanInputs, planner: &PlannerConfig, saved: &[Rule], draft: &[Rule]) -> Result<Preview, PlanError> {
    let (before, _) = plan(inputs, planner, saved)?;
    let (after, outcome) = plan(inputs, planner, draft)?;
    let changes = |from: &EvictionPlan, other: &EvictionPlan| -> Vec<Change> {
        let taken: HashSet<&str> = other.items.iter().map(|item| item.id.as_str()).collect();
        from.items
            .iter()
            .filter(|item| !taken.contains(item.id.as_str()))
            .map(|item| Change {
                id: item.id.clone(),
                title: item.title.clone(),
                size_bytes: item.size_bytes,
                regret: item.regret,
                volume: item.volume.clone(),
                kept_because: other.kept.get(&item.id).map_or_else(|| Kept::NotNeeded.to_string(), ToString::to_string),
            })
            .collect()
    };
    Ok(Preview {
        computed_at: inputs.computed_at,
        saved: Totals::of(&before),
        draft: Totals::of(&after),
        added: changes(&after, &before),
        removed: changes(&before, &after),
        conflicts: outcome.conflicts,
        rules: outcome.rules,
        uncertain: outcome.uncertain,
        forced: outcome.forced,
    })
}

fn plan(inputs: &PlanInputs, planner: &PlannerConfig, rules: &[Rule]) -> Result<(EvictionPlan, Outcome), PlanError> {
    let mut candidates = inputs.candidates.clone();
    let outcome = apply(rules, &mut candidates, &inputs.facts, inputs.computed_at);
    Ok((generate_eviction_plan(&candidates, &inputs.forecasts, planner)?, outcome))
}
