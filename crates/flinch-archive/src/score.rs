//! Reclaim scoring — atomic signals composed into a calibrated probability.
//!
//! Why this exists: Maintainerr decides with one hand-written rule per
//! collection ("watched + complete + 7 days"). That cannot express *why* an item
//! is safe, cannot weigh competing evidence, and cannot produce a probability to
//! gate on. This module answers several atomic `noul`-shaped questions about an
//! item, composes them with explicit weights, and returns a probability plus the
//! ordered reasons behind it — the Laya/Typevec deployment pattern (atomic
//! questions, code-composed decision, confidence gate) applied to disk.
//!
//! Two design rules borrowed from measured practice:
//!
//! 1. **Calibrate before thresholding.** Raw scores are overconfident; a
//!    temperature parameter (fit on local data, defaulting to a mild shrink)
//!    maps the raw logit to a usable probability. Thresholds mean nothing until
//!    this is done — the published example went from ECE 0.466 to 0.081.
//! 2. **Atomic signals with an audit trail.** Every point of score names its
//!    signal, so the UI can show "why", and a wrong verdict points at one signal
//!    instead of the whole model.

use crate::card::{ArchiveCard, Recency, SeasonState};
use crate::watch::WatchSource;

mod household;
mod weights;

pub use household::{ShowActivity, Siblings};
pub use weights::ScoreWeights;

/// One atomic question's contribution.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    pub name: &'static str,
    /// Logit contribution before temperature scaling.
    pub logit: f32,
    /// Operator-facing explanation.
    pub detail: String,
}

/// Context the card alone cannot know: what the rest of the household does.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HouseholdContext {
    /// Any season of the same show was played at least partly.
    pub sibling_season_played: bool,
    /// The show has at least one season played to completion.
    pub sibling_season_completed: bool,
    /// Number of items in the same show/franchise.
    pub siblings: u32,
    /// Where the watch evidence came from, if any. Score must reflect evidence
    /// strength, never its absence: an undecidable item is not a safe item.
    pub watch_source: Option<WatchSource>,
    /// The movie, or some episode of this season, was played on two separate
    /// occasions before the as-of date: the household comes back to it.
    pub rewatched: bool,
    /// Distinct household viewers of this movie, or of any season of its show,
    /// before the as-of date. Counted within one play source, because a Plex
    /// account id and a Tautulli user are different names for the same person.
    pub viewers: u32,
    /// The series has ended and its final episode had aired by the as-of date.
    pub series_ended: bool,
    /// The household's play rate for this item's genres, as P(played within the
    /// horizon), for an item nobody had played before the as-of date; `None`
    /// for every other item, and without genres or closed outcomes (see
    /// [`crate::taste`]).
    pub taste: Option<f32>,
}

/// Extra viewers beyond the first that still add evidence; past this a
/// household-wide favourite is simply a household-wide favourite.
pub const MAX_EXTRA_VIEWERS: u32 = 3;

/// Days since the last play after which a *finished* item counts as cold: the
/// end of the half-weight recency band, so "recently finished" and "finished
/// and cold" never overlap.
pub const COMPLETED_COLD_DAYS: f32 = 180.0;

/// Whether a series had ended as of `as_of`: Sonarr says it has ended *and* its
/// final episode had already aired by then.
///
/// The airing date is what keeps a backtest honest: a show that ended after a
/// cut date was still running at it, so today's status alone would leak the
/// future into the past. Without an airing date nothing is claimed.
pub fn series_ended_as_of(status: Option<&str>, last_aired_epoch: Option<u64>, as_of: u64) -> bool {
    status.is_some_and(|status| status.eq_ignore_ascii_case("ended"))
        && last_aired_epoch.is_some_and(|aired| aired < as_of)
}

/// One unweighted signal value.
///
/// The single definition of "what the model looks at", shared by inference and
/// by training. Anything the fitter learns is a weight over these exact values,
/// so a fitted model can never drift away from what the daemon computes.
#[derive(Debug, Clone)]
pub struct Feature {
    pub name: &'static str,
    /// Unweighted value (0/1 for flags, the clamped quantity for continuous ones).
    pub value: f32,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct ReclaimScore {
    /// The probability the plan gates on: policy terms included, and capped
    /// under a hard guard so no weighting can talk past a rule.
    pub p_safe: f32,
    /// The forecast behind it: the probability that nobody plays the item
    /// within the horizon, from the household's evidence alone. The frozen
    /// policy terms and the guard ceiling decide the plan; they predict
    /// nothing, and folding them in made a guarded season read "99% sure to
    /// be played" whether or not anyone would. This is the number shown and
    /// the one every accuracy metric scores.
    pub forecast: f32,
    /// Raw logit before temperature, retained for fitting and for debugging.
    pub raw_logit: f32,
    pub signals: Vec<Signal>,
    /// A guard that is structural, not probabilistic: favorites, keep-collections
    /// and the newest aired season cannot be reclaimed no matter how many weak
    /// signals line up behind them. Weighted opinions must not outvote a rule.
    pub hard_guard: Option<&'static str>,
}

/// Ceiling applied when a hard guard is present.
pub const HARD_GUARD_CEILING: f32 = 0.01;

impl ReclaimScore {
    /// The strongest two reasons, ordered by absolute contribution.
    pub fn top_reasons(&self, limit: usize) -> Vec<&Signal> {
        let mut sorted: Vec<&Signal> = self.signals.iter().collect();
        sorted.sort_by(|a, b| b.logit.abs().total_cmp(&a.logit.abs()));
        sorted.truncate(limit);
        sorted
    }
}

/// Score one item.
///
/// `temperature` > 1 softens the probability (more honest for a young model);
/// `1.0` is the identity. Fit it against observed outcomes before trusting any
/// threshold.
pub fn score(
    card: &ArchiveCard,
    ctx: HouseholdContext,
    weights: &ScoreWeights,
    temperature: f32,
) -> ReclaimScore {
    let mut signals = Vec::new();
    let mut logit = weights.bias;
    let mut policy = 0.0;

    for feature in features(card, ctx) {
        let contribution = weights.get(feature.name) * feature.value;
        if contribution.abs() < f32::EPSILON {
            continue;
        }
        logit += contribution;
        if ScoreWeights::frozen().contains(&feature.name) {
            policy += contribution;
        }
        signals.push(Signal { name: feature.name, logit: contribution, detail: feature.detail });
    }

    let temperature = if temperature <= 0.05 { 1.0 } else { temperature };
    let probability = |logit: f32| 1.0 / (1.0 + (-(logit / temperature)).exp());
    let forecast = probability(logit - policy);
    let mut p_safe = probability(logit);

    let hard_guard = hard_guard(card);
    if hard_guard.is_some() {
        p_safe = p_safe.min(HARD_GUARD_CEILING);
    }

    ReclaimScore { p_safe, forecast, raw_logit: logit, signals, hard_guard }
}

/// The structural guard on a card, if any: favorites, keep-collections and the
/// newest aired season, in that order of precedence.
pub fn hard_guard(card: &ArchiveCard) -> Option<&'static str> {
    if card.is_favorite {
        Some("favorite")
    } else if card.in_keep_collection {
        Some("keep-collection")
    } else if card.is_newest_season == Some(true) {
        Some("newest-season")
    } else {
        None
    }
}

/// The unweighted feature vector for one item.
///
/// Split out of `score` so training sees exactly what inference sees: a fitted
/// weight can never describe a quantity the daemon does not compute.
pub fn features(card: &ArchiveCard, ctx: HouseholdContext) -> Vec<Feature> {
    let mut out = Vec::new();
    let push = |name: &'static str, value: f32, detail: String, out: &mut Vec<Feature>| {
        if value.abs() < f32::EPSILON {
            return;
        }
        out.push(Feature { name, value, detail });
    };

    // --- atomic question 1: has anyone ever played it? ---
    // Evidence is worth what its provenance is worth, and silence is evidence of
    // nothing: an item with no watch data is undecidable, not safe.
    let source = ctx.watch_source;
    match (card.season_state, card.is_watched) {
        (Some(SeasonState::Empty), _) | (_, Some(false)) => match source {
            Some(source) => push(
                "never_played",
                source.evidence_factor(),
                format!("{}: zero playback", source.label()),
                &mut out,
            ),
            None => push(
                "no_evidence",
                1.0,
                "no watch evidence — undecidable, fail-closed".into(),
                &mut out,
            ),
        },
        (Some(SeasonState::Partial), _) => match source {
            Some(source) => push(
                "partially_played",
                source.evidence_factor(),
                format!("{}: partially played", source.label()),
                &mut out,
            ),
            None => push(
                "no_evidence",
                1.0,
                "no watch evidence — undecidable, fail-closed".into(),
                &mut out,
            ),
        },
        // Watched/completed: the evidence exists, it just argues the other way.
        // Paying the ignorance penalty here would mislabel a household favourite
        // as "undecidable" — the recency and sibling signals already cover it.
        _ => {
            if source.is_none() {
                push(
                    "no_evidence",
                    1.0,
                    "no watch evidence — undecidable, fail-closed".into(),
                    &mut out,
                );
            }
        }
    }
    // --- atomic question 2: how recently was it played? ---
    if let Some(days) = card.last_watched_days {
        if days < 30.0 {
            push("recent_play", 1.0, format!("played {} d ago", days.round()), &mut out);
        } else if days < 180.0 {
            push("recent_play", 0.5, format!("played {} d ago", days.round()), &mut out);
        }
    }
    let finished = card.season_state == Some(SeasonState::Completed) || card.is_watched == Some(true);
    if let Some(days) = card.last_watched_days.filter(|days| finished && *days >= COMPLETED_COLD_DAYS) {
        push("completed_cold", 1.0, format!("finished, untouched for {} d", days.round()), &mut out);
    }

    // --- atomic question 3: how long has it occupied disk? ---
    let years = (card.added_days_ago / 365.0).clamp(0.0, 4.0);
    push(
        "dwell",
        years,
        format!("on disk {:.0} d", card.added_days_ago),
        &mut out,
    );

    // --- atomic question 4: does the household still care about this show? ---
    // Only for an item the household has not finished: siblings being watched
    // predicts *this* season will be too. For a finished season it only says the
    // household finished the show — the opposite of a reason to keep it.
    if !finished {
        if ctx.sibling_season_completed {
            push("sibling_completed", 1.0, "another season of this show is watched to completion".into(), &mut out);
        } else if ctx.sibling_season_played {
            push("sibling_played", 1.0, "another season of this show was played".into(), &mut out);
        }
    }

    // --- atomic question 5: structural guards ---
    if card.is_newest_season == Some(true) {
        push("newest_season", 1.0, "newest aired season".into(), &mut out);
    }
    if card.is_favorite || card.in_keep_collection {
        push(
            "protected_by_tag",
            1.0,
            if card.is_favorite { "favorite".into() } else { "keep-collection".to_string() },
            &mut out,
        );
    }

    // --- atomic question 6: economics ---
    if card.duplicate_count > 0 {
        push("duplicate", 1.0, format!("{} duplicate file(s)", card.duplicate_count), &mut out);
    }
    let gib = card.size_bytes as f32 / (1u64 << 30) as f32;
    push("size", card.size_bytes as f32 / 1e10, format!("frees {gib:.1} GiB"), &mut out);

    // Recency bucket as a coarse guard: an actively-watched item is not a target.
    if matches!(Recency::from_days(card.last_watched_days), Recency::Active) {
        push("active", 1.0, "watched within 30 d".into(), &mut out);
    }

    // --- atomic question 7: does the household keep coming back? ---
    if ctx.rewatched {
        push("rewatched", 1.0, "played again on a separate occasion".into(), &mut out);
    }
    let extra_viewers = ctx.viewers.saturating_sub(1).min(MAX_EXTRA_VIEWERS);
    push("viewer_breadth", extra_viewers as f32, format!("{} household viewers", ctx.viewers), &mut out);
    if ctx.series_ended {
        push("series_ended", 1.0, "the series has ended".into(), &mut out);
    }
    // --- atomic question 8: would this household want it at all? ---
    // Only for an item nobody has played: once plays exist they speak for the
    // household better than its genres do.
    if let Some(played) = ctx.taste.filter(|p| p.is_finite()) {
        let played = played.clamp(0.01, 0.99);
        push("taste", ((1.0 - played) / played).ln(), format!("the household plays {:.0}% of its genres within 30 d", played * 100.0), &mut out);
    }

    out
}

#[cfg(test)]
mod tests;