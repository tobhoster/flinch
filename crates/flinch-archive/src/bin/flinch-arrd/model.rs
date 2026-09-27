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
/// genre rates give, then the scorecard the refit leaves in force, gated no
/// higher than the priors at the operator's temperature. Returns the scores
/// and the scorecard's description.
pub(super) fn score_cycle(settings: &RuntimeSettings, state_dir: &Path, inputs: Scoring<'_>) -> (Vec<ReclaimScore>, String) {
    refit(state_dir, inputs.now);
    let mut contexts: Vec<HouseholdContext> = inputs.cards.iter().map(|card| inputs.context(card)).collect();
    taste::fill(state_dir, &inputs, &mut contexts);
    let (weights, label, temperature) = resolve(state_dir, settings.score_temperature);
    println!("[flinch-arrd] model: {label}");
    let scored = inputs
        .cards
        .iter()
        .zip(contexts)
        .map(|(card, ctx)| score::score_fenced(card, ctx, &weights, temperature, settings.score_temperature))
        .collect();
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

#[cfg(test)]
mod tests {
    use super::*;
    use flinch_archive::fit::candidate::ModelKind;
    use flinch_archive::watch::WatchSource;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("flinch-model-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// A recalibration as the daemon loads it: the priors' weights with a bias
    /// of 6 at temperature 2, and out-of-fold metrics that clear the gate.
    fn recalibration() -> fit::FittedModel {
        fit::FittedModel {
            fitted_on: "test".to_string(),
            kind: ModelKind::Recalibrated,
            weights: HashMap::new(),
            bias: 6.0,
            temperature: 2.0,
            metrics: fit::Metrics {
                examples: 80,
                validation: 80,
                positives: 70,
                negative_items: 4,
                auc: 0.80,
                brier: 0.05,
                priors_auc: 0.80,
                priors_brier: 0.20,
                ..fit::Metrics::default()
            },
        }
    }

    /// A film finished 100 d ago, with its play in Plex, and one nobody played
    /// with no watch evidence at all.
    fn cards() -> Vec<ArchiveCard> {
        let mut finished = flinch_archive::golden::golden_movie();
        finished.id = "radarr-1".to_string();
        finished.last_watched_days = Some(100.0);
        finished.added_days_ago = 400.0;
        finished.rewatch_score = None;
        finished.size_bytes = 20_000_000_000;
        let mut unseen = finished.clone();
        unseen.id = "radarr-2".to_string();
        unseen.is_watched = Some(false);
        unseen.last_watched_days = None;
        vec![finished, unseen]
    }

    /// One cycle's scores at the operator's `temperature`, and the priors' own
    /// at that temperature in the context the cycle builds: Plex has the
    /// finished film's play, and no watch entry for the other.
    fn cycle(state_dir: &Path, cards: &[ArchiveCard], temperature: f32) -> (Vec<ReclaimScore>, Vec<ReclaimScore>) {
        let played = WatchEntry {
            id: "radarr-1".to_string(),
            last_watched_epoch: None,
            progress: 1.0,
            rewatch_score: None,
            source: WatchSource::Plex,
        };
        let watch = HashMap::from([(played.id.clone(), played)]);
        let (activity, play_log, joins, ended_by_show) = (ShowActivity::default(), PlayLog::default(), HashMap::new(), HashMap::new());
        let inputs = || Scoring {
            cards,
            watch: &watch,
            activity: &activity,
            play_log: &play_log,
            joins: &joins,
            ended_by_show: &ended_by_show,
            movies: &[],
            series: &[],
            now: 1_000_000_000,
        };
        let settings = RuntimeSettings { score_temperature: temperature, ..RuntimeSettings::default() };
        let scored = score_cycle(&settings, state_dir, inputs()).0;
        let inputs = inputs();
        let mut contexts: Vec<HouseholdContext> = cards.iter().map(|card| inputs.context(card)).collect();
        taste::fill(state_dir, &inputs, &mut contexts);
        let priors = cards.iter().zip(contexts).map(|(card, ctx)| score::score(card, ctx, &ScoreWeights::default(), temperature)).collect();
        (scored, priors)
    }

    #[rstest::rstest]
    #[case::at_the_default_temperature(RuntimeSettings::default().score_temperature)]
    #[case::at_the_operators_own(2.5)]
    fn an_adopted_recalibration_is_fenced_by_the_priors(#[case] temperature: f32) {
        let adopted = scratch(&format!("adopted-{temperature}"));
        std::fs::write(adopted.join("weights.json"), serde_json::to_string(&recalibration()).expect("encode")).expect("weights.json");
        assert!(fit::load_model(&adopted).is_some(), "the fixture must load as an adopted fit");
        let cards = cards();
        let (fitted, hand_set) = cycle(&adopted, &cards, temperature);
        let floor = RuntimeSettings::default().score_floor;
        for ((card, fitted), hand_set) in cards.iter().zip(&fitted).zip(&hand_set) {
            // The forecast shown is the fit's, and it clears the floor.
            assert!(fitted.forecast > hand_set.forecast && fitted.forecast >= floor, "{}: forecast {}", card.id, fitted.forecast);
            assert_eq!(fitted.p_safe, hand_set.p_safe, "{}: the plan gates on the priors' P(safe) at {temperature}", card.id);
            assert!(fitted.p_safe < floor, "{}: held under the {floor} floor, got {}", card.id, fitted.p_safe);
        }
        std::fs::remove_dir_all(&adopted).ok();
    }

    #[rstest::rstest]
    #[case::at_the_default_temperature(RuntimeSettings::default().score_temperature)]
    #[case::at_the_operators_own(2.5)]
    fn under_the_priors_a_cycle_scores_as_before(#[case] temperature: f32) {
        let priors = scratch(&format!("priors-{temperature}"));
        let cards = cards();
        let (gated, plain) = cycle(&priors, &cards, temperature);
        let bits = |scored: &ReclaimScore| (scored.p_safe.to_bits(), scored.forecast.to_bits(), scored.raw_logit.to_bits());
        for ((card, gated), plain) in cards.iter().zip(&gated).zip(&plain) {
            assert_eq!(bits(gated), bits(plain), "{}: not a bit may move while the priors run", card.id);
        }
        std::fs::remove_dir_all(&priors).ok();
    }
}
