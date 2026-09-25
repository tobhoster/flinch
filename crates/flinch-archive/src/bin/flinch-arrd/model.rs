//! Which scorecard a cycle runs, the daily refit that decides it, and the
//! household context every card is scored in.

use super::taste;
use flinch_archive::arr::{ArrMovie, ArrSeries};
use flinch_archive::daemon::RuntimeSettings;
use flinch_archive::fit::plays::PlayLog;
use flinch_archive::fit::{self, adopt};
use flinch_archive::plex::PlayJoin;
use flinch_archive::score::{self, HouseholdContext, ReclaimScore, ScoreWeights, ShowActivity};
use flinch_archive::watch::WatchEntry;
use flinch_archive::ArchiveCard;
use std::collections::HashMap;
use std::path::Path;

/// What a cycle scores, and everything the household context is built from.
pub(super) struct Scoring<'a> {
    pub(super) cards: &'a [ArchiveCard],
    pub(super) watch: &'a HashMap<String, WatchEntry>,
    pub(super) activity: &'a ShowActivity,
    pub(super) play_log: &'a PlayLog,
    pub(super) joins: &'a HashMap<&'a str, PlayJoin>,
    pub(super) ended_by_show: &'a HashMap<&'a str, bool>,
    pub(super) movies: &'a [ArrMovie],
    pub(super) series: &'a [ArrSeries],
    pub(super) now: u64,
}

impl Scoring<'_> {
    /// How this card's plays were joined; an unresolved card joins none.
    pub(super) fn join(&self, card: &ArchiveCard) -> &PlayJoin {
        self.joins.get(card.id.as_str()).unwrap_or(&PlayJoin::Unresolved)
    }

    /// Household context: does the household still engage with this show? The
    /// *other* seasons are the evidence — a season's own completion never counts
    /// as a reason to keep it (the fitter's definition, so they agree).
    fn context(&self, card: &ArchiveCard) -> HouseholdContext {
        let evidence = self.play_log.evidence(self.join(card), card.kind, self.now);
        let siblings = self.activity.siblings(card, self.watch);
        let show = card.show_title.as_deref();
        HouseholdContext {
            sibling_season_played: siblings.played,
            sibling_season_completed: siblings.completed,
            siblings: self.cards.iter().filter(|c| c.show_title.is_some() && c.show_title == card.show_title).count() as u32,
            // Provenance, not just presence: a stale export must not outvote a live query.
            watch_source: self.watch.get(&card.id).map(|entry| entry.source),
            rewatched: evidence.rewatched,
            viewers: evidence.viewers,
            series_ended: show.is_some_and(|s| self.ended_by_show.get(s).copied().unwrap_or(false)),
            // Filled from the daily fit's genre rates ([`taste::fill`]).
            taste: None,
        }
    }
}

/// Score every card: the daily refit, household context with the taste its
/// genre rates give, then the scorecard the refit leaves in force. Returns the
/// scores and the scorecard's description.
pub(super) fn score_cycle(settings: &RuntimeSettings, state_dir: &Path, inputs: Scoring<'_>) -> (Vec<ReclaimScore>, String) {
    refit(state_dir, inputs.now);
    let mut contexts: Vec<HouseholdContext> = inputs.cards.iter().map(|card| inputs.context(card)).collect();
    taste::fill(state_dir, &inputs, &mut contexts);
    let (weights, label, temperature) = resolve(state_dir, settings.score_temperature);
    println!("[flinch-arrd] model: {label}");
    let scored = inputs.cards.iter().zip(contexts).map(|(card, ctx)| score::score(card, ctx, &weights, temperature)).collect();
    (scored, label)
}

/// Refit the household panel once a day has passed since the last fit. Runs
/// before the scorecard is chosen, so an adoption applies to this cycle.
fn refit(state_dir: &Path, now: u64) {
    match adopt::refit_if_due(state_dir, now) {
        None => {}
        Some(Ok(status)) => match &status.shortfall {
            None => println!(
                "[flinch-arrd] fit: adopted the {}, better than the hand-set priors out of fold ({})",
                status.kind.label(),
                status.fitted_on
            ),
            Some(shortfall) => println!("[flinch-arrd] fit: priors kept — {shortfall} ({})", status.fitted_on),
        },
        Some(Err(error)) => eprintln!("[flinch-arrd] fit failed, priors kept: {error}"),
    }
}

/// Which scorecard to run: an adopted fit if one exists, the priors otherwise.
///
/// The choice is reported, never silent — a scorecard that changes without
/// saying so is indistinguishable from a bug when the numbers move.
fn resolve(state_dir: &Path, prior_temperature: f32) -> (ScoreWeights, String, f32) {
    match fit::load_model(state_dir) {
        Some(model) => (
            model.weights(),
            format!(
                "{} on {} example(s) — out-of-fold AUC {:.2}, Brier {:.3}, ECE {:.2} ({}), temperature {:.2}",
                model.kind.label(),
                model.metrics.examples,
                model.metrics.auc,
                model.metrics.brier,
                model.metrics.ece,
                model.fitted_on,
                model.temperature,
            ),
            model.temperature,
        ),
        None => (ScoreWeights::default(), "priors (no fit has beaten them out of fold yet)".to_string(), prior_temperature),
    }
}
