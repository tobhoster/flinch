//! The planner's knobs (`settings.json` `planner`).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const MIB: u64 = 1 << 20;

/// The planner's knobs (`settings.json` `planner`). Every field defaults
/// individually.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlannerConfig {
    /// Write the plan, hand nothing to Maintainerr. On until the operator has
    /// read a few plans and turns it off.
    pub dry_run: bool,
    /// Sizes and targets are rounded up to this many MiB for the solver.
    pub quantum_mb: u64,
    /// Items on disk fewer days than this are never candidates.
    pub grace_period_days: u32,
    /// Seerr display name → weight of that user's watchlist and requests.
    /// Unlisted users weigh 1.0; names match case-insensitively.
    pub user_weights: BTreeMap<String, f64>,
}

impl Default for PlannerConfig {
    fn default() -> Self {
        Self { dry_run: true, quantum_mb: 100, grace_period_days: 30, user_weights: BTreeMap::new() }
    }
}

/// A [`PlannerConfig`] outside its bounds; the message names the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidPlannerConfig(pub &'static str);

impl PlannerConfig {
    pub fn validate(&self) -> Result<(), InvalidPlannerConfig> {
        if !(1..=10_240).contains(&self.quantum_mb) {
            return Err(InvalidPlannerConfig("the size quantum must be 1 to 10240 MiB"));
        }
        if self.grace_period_days > 3_650 {
            return Err(InvalidPlannerConfig("the grace period must be at most 3650 days"));
        }
        if self.user_weights.values().any(|weight| !(weight.is_finite() && *weight >= 0.0)) {
            return Err(InvalidPlannerConfig("user weights must be 0 or more"));
        }
        Ok(())
    }

    pub fn quantum_bytes(&self) -> u64 {
        self.quantum_mb.saturating_mul(MIB)
    }

    /// The weight of a Seerr user; 1.0 when unlisted.
    pub fn weight(&self, user: &str) -> f64 {
        self.user_weights.iter().find(|(name, _)| name.eq_ignore_ascii_case(user)).map_or(1.0, |(_, weight)| *weight)
    }
}
