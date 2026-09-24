//! Inflow: how fast the library grows, steered per item.
//!
//! Recyclarr (the nightly TRaSH sync) owns what quality profiles *are* —
//! qualities, cutoffs, custom-format scores, size tables — and rewrites them
//! every night, so FLINCH never writes a profile or a quality definition. It
//! decides only which of the operator's profiles an item *uses*, from the same
//! calibrated household evidence that decides eviction: a film nobody will play
//! again does not need its next 4K upgrade; a favourite does.
//!
//! The advice is published every cycle; acting on it is a separate, explicit
//! switch. Moving an item that already has a file into a smaller profile can
//! trigger a replacement download (bandwidth, two copies until import, then
//! the recycle-bin window), so the doctrine for a new reflex applies: observe
//! for a cycle before it is trusted.

use crate::card::ArchiveCard;
use crate::score::ReclaimScore;
use serde::{Deserialize, Serialize};

/// The operator's two tiers, by profile *name* (ids differ per *arr instance
/// and are resolved at the edge). Either empty disables inflow advice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InflowProfiles {
    pub premium: String,
    pub compact: String,
}

impl InflowProfiles {
    pub fn configured(&self) -> bool {
        !self.premium.trim().is_empty() && !self.compact.trim().is_empty()
    }
}

/// Calibrated P(safe) at or above which an item is low value: nobody is
/// expected to play it again within the horizon. Deliberately high — a
/// profile move is cheap to undo, but a wasted re-download is not free.
pub const COMPACT_FLOOR: f32 = 0.85;
/// Calibrated P(safe) below which the household is expected to come back to
/// the item: it earns the premium tier.
pub const PREMIUM_CEILING: f32 = 0.40;

/// Which tier an item should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Premium,
    Compact,
}

/// One item's inflow advice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InflowAdvice {
    pub tier: Tier,
    /// Why, in the operator's words.
    pub reason: String,
}

/// Advise one item's tier, or `None` when the evidence says nothing either way
/// (leave it where the operator put it).
///
/// Guards come first and always mean premium: a favourite, a keep tag, a
/// keep-collection or the newest aired season never loses quality on a score.
/// Missing watch evidence means no advice — an unscored item is not a
/// low-value item.
pub fn advise(card: &ArchiveCard, score: &ReclaimScore) -> Option<InflowAdvice> {
    if let Some(guard) = score.hard_guard {
        return Some(InflowAdvice { tier: Tier::Premium, reason: format!("guarded ({guard})") });
    }
    if card.is_favorite || card.in_keep_collection {
        return Some(InflowAdvice { tier: Tier::Premium, reason: "kept by the operator".to_string() });
    }
    let p_safe = score.p_safe;
    if !p_safe.is_finite() {
        return None;
    }
    if p_safe >= COMPACT_FLOOR {
        return Some(InflowAdvice {
            tier: Tier::Compact,
            reason: format!("P(safe) {:.0}%: nobody is expected to play it again", p_safe * 100.0),
        });
    }
    if p_safe < PREMIUM_CEILING {
        return Some(InflowAdvice {
            tier: Tier::Premium,
            reason: format!("P(safe) {:.0}%: the household is expected back", p_safe * 100.0),
        });
    }
    None
}

/// A season-level advice rolled up to its show: Sonarr profiles apply to the
/// whole series. Any season that earns premium keeps the show premium; the
/// show goes compact only when every advised season agrees.
pub fn roll_up(seasons: impl IntoIterator<Item = Option<Tier>>) -> Option<Tier> {
    let mut rolled = None;
    for tier in seasons.into_iter().flatten() {
        match tier {
            Tier::Premium => return Some(Tier::Premium),
            Tier::Compact => rolled = Some(Tier::Compact),
        }
    }
    rolled
}

#[cfg(test)]
mod tests;
