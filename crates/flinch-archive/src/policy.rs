//! The deterministic archival policy — the floor, the labeler, and the thing
//! any trained model must beat.
//!
//! Two roles, deliberately the same code:
//!
//! 1. **Baseline candidate generator.** The model never authorises a delete on
//!    its own; the deterministic rules below are what actually decides, and the
//!    model (once trained) can only sharpen the fuzzy edge.
//! 2. **Labeler.** When the household's real watch history is imported, this
//!    same logic produces the ground-truth `safe_to_delete` label that a head
//!    is trained against: the label comes from the household's own behaviour.

use crate::card::{ArchiveCard, Recency, SeasonState};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ArchivePolicy {
    /// Completed-and-untouched for at least this many days.
    pub retention_days: f32,
    /// Minimum calibrated P(safe) (the scorecard's verdict) before a delete the
    /// policy permits may be planned. 0.0 disables the gate — the deterministic
    /// rules alone decide — which is the library default; the daemon sets it
    /// from the operator's `score_floor`. Cards without a verdict are judged by
    /// the rules alone: absence of a score is not a low score.
    pub score_floor: f32,
    /// Keep the newest aired season regardless of watch state: the household
    /// may be mid-binge or waiting for the next to air.
    pub keep_newest_season: bool,
    /// Delete duplicate movie copies, keeping the largest (best quality).
    pub dedupe_movies: bool,
    /// Reclaim never-played items when the calibrated score clears a floor.
    ///
    /// This is the one place FLINCH exceeds Maintainerr's rule: "watched and
    /// stale" can never free a movie nobody ever opened, which is exactly the
    /// category a household accumulates most of. Off by default — deleting
    /// something on the strength of *absence* of evidence is a product decision,
    /// not a modelling one.
    pub unwatched_reclaim: UnwatchedReclaim,
}

/// Terms under which never-played items may be reclaimed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct UnwatchedReclaim {
    pub enabled: bool,
    /// Minimum calibrated P(safe).
    pub floor: f32,
    /// Minimum time on disk before absence of playback means anything.
    pub min_dwell_days: f32,
}

impl Default for UnwatchedReclaim {
    fn default() -> Self {
        Self { enabled: false, floor: 0.75, min_dwell_days: 90.0 }
    }
}

/// What the calibrated model thinks of one item, as the policy sees it.
///
/// The policy is the arbiter: a model can widen what is reclaimable, never
/// override a protection. `hard_guard` carries the structural caps (favorite,
/// keep-collection, newest season) so a high score can never talk past them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreVerdict {
    pub p_safe: f32,
    pub hard_guard: bool,
    /// Whether another season of the same show has been played.
    pub sibling_played: bool,
}

impl ScoreVerdict {
    /// Does this verdict permit reclaiming an item nobody ever played?
    ///
    /// The sibling term is not decoration. Fitting against a real household's
    /// history found exactly one failure class: a show gets picked up as a whole,
    /// so its seasons look never-played *right up until the first episode plays*;
    /// two seasons read 78% at a cut thirty days before every season was watched.
    /// Nothing predicts that from before the cut, but once *any* season shows
    /// activity the answer is obvious: refuse.
    pub fn permits_unwatched_reclaim(&self, terms: UnwatchedReclaim, dwell_days: f32) -> bool {
        terms.enabled && !self.hard_guard && !self.sibling_played && self.p_safe >= terms.floor && dwell_days >= terms.min_dwell_days
    }
}

impl Default for ArchivePolicy {
    fn default() -> Self {
        Self {
            retention_days: 90.0,
            score_floor: 0.0,
            keep_newest_season: true,
            dedupe_movies: true,
            unwatched_reclaim: UnwatchedReclaim::default(),
        }
    }
}

/// Why an item is (or is not) a delete candidate. The reason is the action:
/// a fragile rule needs a visible explanation, and the plan file records every
/// one so an operator can audit what the reflex did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Reason {
    KeepBecauseFavorite,
    KeepBecauseInKeepCollection,
    KeepBecauseActive,
    KeepBecauseWarm,
    KeepBecauseNewestSeason,
    KeepBecauseNotCompleted,
    KeepBecauseNeverWatchedIsSoleCopy,
    /// Watched, but the media server never recorded when. Held rather than
    /// treated as stale: an undated "watched" is what a manual mark or a client
    /// that never scrobbled leaves behind, and guessing that it was long ago is
    /// the irreversible mistake this guard exists to prevent.
    KeepBecauseWatchedUndated,
    KeepBecauseLowDuplicateValue,
    DeleteCompletedUntouched {
        days: f32,
        size_bytes: u64,
    },
    /// Never played, old enough, and the calibrated score clears the floor. The
    /// reason carries the probability because that is the whole justification.
    DeleteUnwatchedByScore {
        p_safe: f32,
        days: f32,
        size_bytes: u64,
    },
    DeleteWatchedUntouched {
        days: f32,
        size_bytes: u64,
    },
    DeleteDuplicate {
        size_bytes: u64,
        survivor_size_bytes: u64,
    },
}

/// The deterministic arbitration of one card.
///
/// Order matters and is the safety encyclopaedia: protections are checked
/// before any delete is allowed, so a favorite is never reclaimed even if every
/// other signal screams "gone".
pub fn decide(card: &ArchiveCard, policy: &ArchivePolicy, verdict: Option<ScoreVerdict>) -> Reason {
    let recency = Recency::from_days(card.last_watched_days);

    // Protections first: never overridden by any learned or heuristic signal.
    if card.is_favorite {
        return Reason::KeepBecauseFavorite;
    }
    if card.in_keep_collection {
        return Reason::KeepBecauseInKeepCollection;
    }
    if recency == Recency::Active {
        return Reason::KeepBecauseActive;
    }

    // "Watched" and "watched a long time ago" are different claims. Plex reports
    // `viewedLeafCount 10/10` with no `lastViewedAt` for manual marks and for
    // clients that never scrobbled (seen on live seasons watched to the end),
    // so an absent date is missing information, not evidence of staleness.
    let watched = card.season_state == Some(SeasonState::Completed) || card.is_watched == Some(true);
    if watched && card.last_watched_days.is_none() {
        return Reason::KeepBecauseWatchedUndated;
    }

    match card.kind {
        crate::card::LibraryKind::Season => decide_season(card, policy, recency, verdict),
        crate::card::LibraryKind::Movie => decide_movie(card, policy, recency, verdict),
    }
}

fn decide_season(card: &ArchiveCard, policy: &ArchivePolicy, recency: Recency, verdict: Option<ScoreVerdict>) -> Reason {
    // The newest aired season is the catch-up queue; reclaiming it would delete
    // the thing most likely to be resumed next.
    if policy.keep_newest_season && card.is_newest_season == Some(true) {
        return Reason::KeepBecauseNewestSeason;
    }
    if card.season_state != Some(SeasonState::Completed) {
        // Proven zero playback is the one un-completed state the score may act
        // on: nothing was ever watched, so there is no language-track
        // ambiguity to resolve.
        if card.season_state == Some(SeasonState::Empty)
            && verdict.is_some_and(|v| v.permits_unwatched_reclaim(policy.unwatched_reclaim, card.added_days_ago))
        {
            return Reason::DeleteUnwatchedByScore {
                p_safe: verdict.map(|v| v.p_safe).unwrap_or(0.0),
                days: card.added_days_ago,
                size_bytes: card.size_bytes,
            };
        }
        // Anime dubs/subs asymmetry can look like "partial" on the EN measure
        // while the household just watches a different language track. Partial
        // is never deleted; that is the conservative reading.
        return Reason::KeepBecauseNotCompleted;
    }
    match recency {
        Recency::Active | Recency::Warm => Reason::KeepBecauseWarm,
        Recency::Cold | Recency::Coldest => {
            let days = card.last_watched_days.unwrap_or(policy.retention_days + 1.0);
            if recency == Recency::Cold && days < policy.retention_days {
                return Reason::KeepBecauseWarm;
            }
            Reason::DeleteCompletedUntouched { days, size_bytes: card.size_bytes }
        }
    }
}

fn decide_movie(card: &ArchiveCard, policy: &ArchivePolicy, recency: Recency, verdict: Option<ScoreVerdict>) -> Reason {
    match recency {
        Recency::Active | Recency::Warm => Reason::KeepBecauseWarm,
        Recency::Cold | Recency::Coldest => {
            if policy.dedupe_movies && card.duplicate_count > 0 {
                return Reason::DeleteDuplicate { size_bytes: card.size_bytes, survivor_size_bytes: 0 };
            }
            // A never-watched movie is not a "watched movie", and deleting the
            // sole copy of something the household may still intend to watch is
            // the one mistake this policy exists to prevent.
            if card.is_watched != Some(true) {
                if verdict.is_some_and(|v| v.permits_unwatched_reclaim(policy.unwatched_reclaim, card.added_days_ago)) {
                    return Reason::DeleteUnwatchedByScore {
                        p_safe: verdict.map(|v| v.p_safe).unwrap_or(0.0),
                        days: card.added_days_ago,
                        size_bytes: card.size_bytes,
                    };
                }
                return Reason::KeepBecauseNeverWatchedIsSoleCopy;
            }
            if card.rewatch_score.unwrap_or(0.0) >= 0.7 {
                return Reason::KeepBecauseLowDuplicateValue;
            }
            let days = card.last_watched_days.unwrap_or(policy.retention_days + 1.0);
            Reason::DeleteWatchedUntouched { days, size_bytes: card.size_bytes }
        }
    }
}

/// Is this a delete action that reclaims bytes?
pub fn reclaims_bytes(reason: &Reason) -> u64 {
    match reason {
        Reason::DeleteCompletedUntouched { size_bytes, .. }
        | Reason::DeleteWatchedUntouched { size_bytes, .. }
        | Reason::DeleteUnwatchedByScore { size_bytes, .. }
        | Reason::DeleteDuplicate { size_bytes, .. } => *size_bytes,
        _ => 0,
    }
}

/// Whether an eviction is announced before it goes: nobody finished it, so the
/// household gets a Leaving Soon window to claim it. A finished item, or a
/// duplicate whose other copy stays, leaves directly. Exhaustive on purpose:
/// every new way to reclaim must decide whether it warns first.
pub fn announces(reason: &Reason) -> bool {
    match reason {
        Reason::DeleteUnwatchedByScore { .. } => true,
        Reason::DeleteCompletedUntouched { .. } | Reason::DeleteWatchedUntouched { .. } | Reason::DeleteDuplicate { .. } => false,
        Reason::KeepBecauseFavorite
        | Reason::KeepBecauseInKeepCollection
        | Reason::KeepBecauseActive
        | Reason::KeepBecauseWarm
        | Reason::KeepBecauseNewestSeason
        | Reason::KeepBecauseNotCompleted
        | Reason::KeepBecauseNeverWatchedIsSoleCopy
        | Reason::KeepBecauseWatchedUndated
        | Reason::KeepBecauseLowDuplicateValue => false,
    }
}

impl Reason {
    /// The decision in the operator's words: short, plain, and true to what the
    /// policy actually did. The UI shows this verbatim, so it names the rule,
    /// not the machinery.
    pub fn describe(&self) -> String {
        match self {
            Reason::KeepBecauseFavorite => "Favorite".into(),
            Reason::KeepBecauseInKeepCollection => "Kept by you (Maintainerr exclusion or Plex keep marker)".into(),
            Reason::KeepBecauseActive => "Watched in the last 30 days".into(),
            Reason::KeepBecauseWarm => "Watched recently".into(),
            Reason::KeepBecauseNewestSeason => "Newest aired season".into(),
            Reason::KeepBecauseNotCompleted => "Not fully watched".into(),
            Reason::KeepBecauseNeverWatchedIsSoleCopy => "Never played".into(),
            Reason::KeepBecauseWatchedUndated => "Watched, no date recorded".into(),
            Reason::KeepBecauseLowDuplicateValue => "Likely to be rewatched".into(),
            Reason::DeleteCompletedUntouched { days, .. } | Reason::DeleteWatchedUntouched { days, .. } => {
                format!("Watched, untouched for {days:.0} days")
            }
            Reason::DeleteUnwatchedByScore { p_safe, days, .. } => {
                format!("Never played in {days:.0} days, P(safe) {:.0}%", p_safe * 100.0)
            }
            Reason::DeleteDuplicate { .. } => "Duplicate copy".into(),
        }
    }
}

/// Ground-truth label for training a head: safe to delete or not.
pub fn is_safe_label(reason: &Reason) -> bool {
    reclaims_bytes(reason) > 0
}

#[cfg(test)]
mod tests;
