//! `state/eviction-plan.json`: the plan as written every run, dry or not, so
//! an operator can read what would leave before anything does. Moves to the
//! archive root are listed apart from `items`, which only ever leave.

use super::knapsack::Method;
use super::{EvictionPlan, PlanItem, PlanMove};
use crate::capacity::VolumeForecast;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Manifest<'a> {
    pub timestamp: String,
    pub dry_run: bool,
    pub forecast: &'a [VolumeForecast],
    pub reclaim_target_bytes: u64,
    pub total_reclaimed_bytes: u64,
    pub total_regret: f64,
    pub candidates_count: usize,
    pub method: Option<Method>,
    pub solver_error: Option<&'a str>,
    pub items: &'a [PlanItem],
    pub total_moved_bytes: u64,
    pub moves: &'a [PlanMove],
}

impl<'a> Manifest<'a> {
    pub fn new(plan: &'a EvictionPlan, forecasts: &'a [VolumeForecast], dry_run: bool, now: u64) -> Self {
        Self {
            timestamp: crate::presence::format_utc(now),
            dry_run,
            forecast: forecasts,
            reclaim_target_bytes: plan.target_bytes,
            total_reclaimed_bytes: plan.total_reclaimed_bytes,
            total_regret: plan.total_regret,
            candidates_count: plan.candidates_count,
            method: plan.method,
            solver_error: plan.solver_error.as_deref(),
            items: &plan.items,
            total_moved_bytes: plan.moved_bytes(),
            moves: &plan.moves,
        }
    }
}
