//! What to do with an item's quality, from the same numbers the planner
//! solves with: P(watch), regret and re-download difficulty.
//!
//! - **Keep the original** when the household is likely to watch it, someone is
//!   partway through, losing it costs a lot, or it would be hard to get back.
//! - **Downgrade** a large file with moderate demand: the title stays, most of
//!   its bytes come back.
//! - **Eligible for eviction** when demand is low and it is easy to replace.
//!
//! The advice is published; FLINCH never changes a quality profile itself.
//! Recyclarr owns what profiles are, and a move can trigger a re-download.

use crate::regret::Regret;
use serde::{Deserialize, Serialize};

const GIB: f64 = (1u64 << 30) as f64;

/// At or above this P(watch) the original stays.
pub const KEEP_P_WATCH: f64 = 0.50;
/// Below this P(watch) an easily replaced item may go.
pub const EVICT_P_WATCH: f64 = 0.15;
/// Regret at or above which the original stays whatever its P(watch).
pub const PROTECT_REGRET: f64 = 1.0;
/// Re-download difficulty above which an item is kept rather than evicted.
pub const MAX_EVICT_FRICTION: f64 = 3.0;
/// Files at least this large are worth a downgrade.
pub const DOWNGRADE_MIN_BYTES: u64 = 15 * (1 << 30);
/// Share of a file a compact release is assumed to give back.
pub const DOWNGRADE_SHARE: f64 = 0.80;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeepReason {
    HighWatchLikelihood,
    ActiveProgress,
    HouseholdPriority,
    HighReacquisitionFriction,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QualityAction {
    KeepOriginal { reason: KeepReason },
    DowngradeQuality { estimated_reclaim_bytes: u64 },
    EligibleForEviction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityAdvice {
    pub action: QualityAction,
    /// Regret per GiB its bytes would free.
    pub marginal_regret_per_gb: f64,
    pub explanation: String,
}

/// Advice for one item of `size_bytes`; `partway` when an active viewer is
/// partway through it.
pub fn advise(regret: &Regret, partway: bool, size_bytes: u64) -> QualityAdvice {
    let gib = size_bytes as f64 / GIB;
    let marginal_regret_per_gb = if gib > 0.0 { regret.value / gib } else { regret.value };
    let p = regret.p_watch * 100.0;
    let keep = |reason, explanation: String| (QualityAction::KeepOriginal { reason }, explanation);
    let (action, explanation) = if partway {
        keep(KeepReason::ActiveProgress, "Someone is partway through it".to_string())
    } else if regret.p_watch >= KEEP_P_WATCH {
        keep(KeepReason::HighWatchLikelihood, format!("Likely watched soon: {p:.0}% P(watch)"))
    } else if regret.value >= PROTECT_REGRET {
        keep(KeepReason::HouseholdPriority, format!("Costly to lose: regret {:.2}", regret.value))
    } else if regret.p_watch >= EVICT_P_WATCH && size_bytes >= DOWNGRADE_MIN_BYTES {
        let freed = (size_bytes as f64 * DOWNGRADE_SHARE) as u64;
        (
            QualityAction::DowngradeQuality { estimated_reclaim_bytes: freed },
            format!("Moderate demand ({p:.0}% P(watch)): a compact release frees about {:.0} GiB", freed as f64 / GIB),
        )
    } else if regret.friction > MAX_EVICT_FRICTION {
        keep(KeepReason::HighReacquisitionFriction, format!("Hard to get back: friction {:.1}", regret.friction))
    } else {
        (QualityAction::EligibleForEviction, format!("Low demand ({p:.0}% P(watch)) and easy to replace"))
    };
    QualityAdvice { action, marginal_regret_per_gb, explanation }
}

/// How safe evicting is, 0..1: never above `1 − P(watch)`, and lower the
/// harder the item is to get back. A 38% P(watch) never reads above 62%.
pub fn eviction_safety(regret: &Regret) -> f64 {
    ((1.0 - regret.p_watch) - (regret.friction - 1.0).max(0.0) / 10.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests;
