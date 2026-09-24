//! Scoring tests.
//!
//! Kept beside the module: the signal contract (what each atomic question
//! contributes, and that a zero-weight signal contributes nothing) is the part
//! that must not drift when a feature is added.

use super::*;
use crate::card::LibraryKind;
use crate::golden::{golden_movie, golden_season};
use proptest::prelude::*;
use rstest::rstest;

fn season(never_played: bool, dwell_days: f32, newest: bool) -> ArchiveCard {
    let mut card = golden_season();
    card.season_state = Some(if never_played { SeasonState::Empty } else { SeasonState::Completed });
    card.added_days_ago = dwell_days;
    card.is_newest_season = Some(newest);
    card.last_watched_days = None;
    card
}

#[test]
fn never_played_and_old_scores_higher_than_recently_played() {
    let w = ScoreWeights::default();
    let stale = score(&season(true, 900.0, false), HouseholdContext::default(), &w, 1.0);
    let mut played = season(true, 900.0, false);
    played.last_watched_days = Some(3.0);
    let recent = score(&played, HouseholdContext::default(), &w, 1.0);
    assert!(
        stale.p_safe > recent.p_safe + 0.2,
        "never-played ({:.3}) must clearly outscore played-yesterday ({:.3})",
        stale.p_safe,
        recent.p_safe
    );
}

#[test]
fn a_watched_sibling_season_protects_its_siblings() {
    let w = ScoreWeights::default();
    let card = season(true, 900.0, false);
    let alone = score(&card, HouseholdContext::default(), &w, 1.0);
    let with_sibling = score(&card, HouseholdContext { sibling_season_completed: true, siblings: 3, ..Default::default() }, &w, 1.0);
    let with_played_sibling = score(&card, HouseholdContext { sibling_season_played: true, siblings: 3, ..Default::default() }, &w, 1.0);
    assert!(with_sibling.p_safe < alone.p_safe, "a completed sibling must lower reclaim confidence");
    assert!(with_played_sibling.p_safe < alone.p_safe, "any played sibling must lower reclaim confidence");
    assert!(with_sibling.p_safe < with_played_sibling.p_safe, "completion is stronger evidence than partial play");
}

#[test]
fn favorites_and_newest_seasons_cannot_be_scored_as_safe() {
    let w = ScoreWeights::default();
    let mut favorite = season(true, 2000.0, false);
    favorite.is_favorite = true;
    let scored = score(&favorite, HouseholdContext::default(), &w, 1.0);
    assert!(scored.p_safe <= HARD_GUARD_CEILING, "favorite must be structurally capped");
    assert_eq!(scored.hard_guard, Some("favorite"));

    let newest = score(&season(true, 2000.0, true), HouseholdContext::default(), &w, 1.0);
    assert_eq!(newest.hard_guard, Some("newest-season"));
    assert!(newest.p_safe <= HARD_GUARD_CEILING);
}

#[test]
fn a_rule_caps_what_the_plan_sees_but_never_the_forecast() {
    // The forecast is what the UI shows and the metrics score. Folding a rule
    // into it made a guarded season read "99% sure to be played".
    let w = ScoreWeights::default();
    let live = HouseholdContext { watch_source: Some(WatchSource::Plex), ..Default::default() };
    let plain = season(true, 400.0, false);
    let mut favorite = plain.clone();
    favorite.is_favorite = true;
    let (guarded, open) = (score(&favorite, live, &w, 1.0), score(&plain, live, &w, 1.0));
    assert!(guarded.p_safe <= HARD_GUARD_CEILING, "the plan still sees the guard");
    assert!((guarded.forecast - open.forecast).abs() < 1e-6, "a rule is not evidence");
    assert!((open.forecast - open.p_safe).abs() < 1e-6, "with evidence and no rule, the two agree");

    // The fail-closed penalty for missing evidence gates the plan, not the forecast.
    let blind = score(&plain, HouseholdContext::default(), &w, 1.0);
    assert!(blind.p_safe < blind.forecast, "no evidence keeps the gate shut");
}

#[test]
fn temperature_softens_without_reordering() {
    let w = ScoreWeights::default();
    let live = HouseholdContext { watch_source: Some(WatchSource::Plex), ..Default::default() };
    let card = season(true, 700.0, false);
    let sharp = score(&card, live, &w, 1.0);
    let soft = score(&card, live, &w, 2.5);
    assert!(soft.p_safe < sharp.p_safe, "higher temperature pulls toward 0.5");
    let other = score(&season(false, 400.0, false), live, &w, 1.0);
    let other_soft = score(&season(false, 400.0, false), live, &w, 2.5);
    assert!(
        (sharp.p_safe - other.p_safe).signum() == (soft.p_safe - other_soft.p_safe).signum(),
        "temperature must not reorder candidates"
    );
}

#[test]
fn duplicates_and_big_items_raise_the_score() {
    let w = ScoreWeights::default();
    let live = HouseholdContext { watch_source: Some(WatchSource::Plex), ..Default::default() };
    let base = score(&season(true, 500.0, false), live, &w, 1.0);
    let mut dup = season(true, 500.0, false);
    dup.duplicate_count = 1;
    dup.size_bytes = 20_000_000_000;
    let scored = score(&dup, live, &w, 1.0);
    assert!(scored.p_safe > base.p_safe);
}

#[test]
fn a_played_item_pays_no_ignorance_penalty() {
    let w = ScoreWeights::default();
    let mut played = season(false, 400.0, false);
    played.season_state = Some(SeasonState::Completed);
    played.last_watched_days = Some(13.0);
    let scored = score(
        &played,
        HouseholdContext { watch_source: Some(WatchSource::Plex), ..Default::default() },
        &w,
        1.0,
    );
    assert!(
        !scored.signals.iter().any(|s| s.name == "no_evidence"),
        "a watched item is not undecidable: {:?}",
        scored.signals
    );
    assert!(scored.p_safe < 0.45, "recently played must not look reclaimable");

    // The penalty still applies when there is genuinely nothing to go on.
    let mut blank = played.clone();
    blank.last_watched_days = None;
    let unknown = score(&blank, HouseholdContext::default(), &w, 1.0);
    assert!(unknown.signals.iter().any(|s| s.name == "no_evidence"));
}

#[test]
fn ignorance_is_never_scored_as_safety() {
    let w = ScoreWeights::default();
    // Four years of dwell and 20 GiB on disk: every weak signal a hoarder
    // could hope for, and no watch evidence at all. It must NOT clear a
    // 0.75 floor — the model cannot reward never having asked.
    let mut unknown = season(true, 1460.0, false);
    unknown.size_bytes = 20_000_000_000;
    unknown.season_state = None;
    unknown.is_watched = None;
    let scored = score(&unknown, HouseholdContext::default(), &w, 1.6);
    assert!(scored.p_safe < 0.75, "unknown scored {} — too confident", scored.p_safe);
    assert!(scored.signals.iter().any(|s| s.name == "no_evidence"));
}

#[test]
fn stale_export_evidence_is_worth_less_than_a_live_query() {
    let w = ScoreWeights::default();
    let card = season(true, 700.0, false);
    let live = score(&card, HouseholdContext { watch_source: Some(WatchSource::Plex), ..Default::default() }, &w, 1.0);
    let stale = score(&card, HouseholdContext { watch_source: Some(WatchSource::Export), ..Default::default() }, &w, 1.0);
    assert!(live.p_safe > stale.p_safe, "live evidence must outweigh a file");
    assert!(live.signals.iter().any(|s| s.detail.starts_with("plex:")));
    assert!(stale.signals.iter().any(|s| s.detail.starts_with("export:")));
}

#[test]
fn reasons_name_the_signals_that_moved_the_score() {
    let mut card = golden_movie();
    card.is_watched = Some(false);
    card.added_days_ago = 1200.0;
    let live = HouseholdContext { watch_source: Some(WatchSource::Plex), ..Default::default() };
    let scored = score(&card, live, &ScoreWeights::default(), 1.0);
    let names: Vec<&str> = scored.signals.iter().map(|s| s.name).collect();
    assert!(names.contains(&"never_played"), "got {names:?}");
    assert!(names.contains(&"dwell"));
    let top = scored.top_reasons(2);
    assert_eq!(top.len(), 2);
    assert!(top[0].detail.len() > 2, "reason must be human-readable");
}

#[rstest]
#[case::ended_after_its_last_airing("ended", Some(100), 200, true)]
#[case::status_is_case_insensitive("Ended", Some(100), 200, true)]
// Backtest honesty: at a cut before the final episode aired, the show was
// still running, whatever Sonarr says today.
#[case::final_episode_after_the_cut("ended", Some(300), 200, false)]
#[case::no_airing_date_proves_nothing("ended", None, 200, false)]
#[case::continuing_is_not_ended("continuing", Some(100), 200, false)]
fn a_series_counts_as_ended_only_once_its_final_episode_had_aired(
    #[case] status: &str,
    #[case] last_aired: Option<u64>,
    #[case] as_of: u64,
    #[case] expected: bool,
) {
    assert_eq!(series_ended_as_of(Some(status), last_aired, as_of), expected);
}

/// A household context that switches on exactly one zero-prior signal. The
/// taste answer is a low P(played): a positive weight must read it as "safe".
fn with_signal(name: &str) -> HouseholdContext {
    let live = HouseholdContext { watch_source: Some(WatchSource::Plex), ..Default::default() };
    match name {
        "rewatched" => HouseholdContext { rewatched: true, ..live },
        "viewer_breadth" => HouseholdContext { viewers: 3, ..live },
        "series_ended" => HouseholdContext { series_ended: true, ..live },
        "taste" => HouseholdContext { taste: Some(0.1), ..live },
        other => panic!("no context switch for {other}"),
    }
}

#[rstest]
fn a_learnt_household_weight_reaches_the_score(#[values("rewatched", "viewer_breadth", "series_ended", "taste")] name: &str) {
    // The zero-weight invariant below would pass trivially if these signals
    // never reached `score`; a non-zero weight must move P(safe) its way.
    let card = season(true, 700.0, false);
    let ctx = with_signal(name);
    let base = score(&card, ctx, &ScoreWeights::default(), 1.0).p_safe;
    for (weight, direction) in [(1.0f32, 1.0f32), (-1.0, -1.0)] {
        let mut weights = ScoreWeights::default();
        assert!(weights.set(name, weight), "{name} must be a weight the table carries");
        let moved = score(&card, ctx, &weights, 1.0);
        assert!((moved.p_safe - base) * direction > 0.0, "{name} at {weight} left P(safe) at {base}");
        assert!(moved.signals.iter().any(|signal| signal.name == name), "{name} must be named as a reason");
    }
}

fn watch_source() -> impl Strategy<Value = Option<WatchSource>> {
    proptest::option::of(proptest::sample::select(WatchSource::ALL.to_vec()))
}

fn season_state() -> impl Strategy<Value = Option<SeasonState>> {
    prop_oneof![
        Just(None),
        Just(Some(SeasonState::Empty)),
        Just(Some(SeasonState::Partial)),
        Just(Some(SeasonState::Completed)),
    ]
}

prop_compose! {
    fn any_card()(
        movie in any::<bool>(),
        size_bytes in 0u64..80_000_000_000,
        added_days_ago in 0.0f32..3000.0,
        last_watched_days in proptest::option::of(0.0f32..800.0),
        flags in (any::<bool>(), any::<bool>(), proptest::option::of(any::<bool>()), proptest::option::of(any::<bool>())),
        duplicate_count in 0u32..3,
        season_state in season_state(),
    ) -> ArchiveCard {
        let (is_favorite, in_keep_collection, is_newest_season, is_watched) = flags;
        let mut card = if movie { golden_movie() } else { golden_season() };
        card.kind = if movie { LibraryKind::Movie } else { LibraryKind::Season };
        card.size_bytes = size_bytes;
        card.added_days_ago = added_days_ago;
        card.last_watched_days = last_watched_days;
        card.is_favorite = is_favorite;
        card.in_keep_collection = in_keep_collection;
        card.is_newest_season = is_newest_season;
        card.is_watched = is_watched;
        card.duplicate_count = duplicate_count;
        card.season_state = season_state;
        card
    }
}

prop_compose! {
    fn any_context()(
        sibling_season_played in any::<bool>(),
        sibling_season_completed in any::<bool>(),
        siblings in 0u32..8,
        watch_source in watch_source(),
        rewatched in any::<bool>(),
        viewers in 0u32..9,
        series_ended in any::<bool>(),
        taste in proptest::option::of(0.0f32..=1.0),
    ) -> HouseholdContext {
        HouseholdContext {
            sibling_season_played,
            sibling_season_completed,
            siblings,
            watch_source,
            rewatched,
            viewers,
            series_ended,
            taste,
        }
    }
}

proptest! {
    /// Adding a household signal with its zero prior must not move a single bit
    /// of the deployed score: until a fit on this household earns it a weight,
    /// the daemon decides exactly as it did before the signal existed.
    #[test]
    fn zero_weight_household_signals_leave_the_score_bit_identical(
        card in any_card(),
        ctx in any_context(),
        temperature in 0.4f32..4.0,
    ) {
        let weights = ScoreWeights::default();
        let silent = HouseholdContext { rewatched: false, viewers: 0, series_ended: false, taste: None, ..ctx };
        let with = score(&card, ctx, &weights, temperature);
        let without = score(&card, silent, &weights, temperature);
        prop_assert_eq!(with.p_safe.to_bits(), without.p_safe.to_bits());
        prop_assert_eq!(with.raw_logit.to_bits(), without.raw_logit.to_bits());
        prop_assert_eq!(with.signals, without.signals);
        prop_assert_eq!(with.hard_guard, without.hard_guard);
    }

    /// `get` and `set` are two views of one name → field table; a name added
    /// to one and not the other would silently drop a fitted weight.
    #[test]
    fn every_weight_name_round_trips_through_set_and_get(value in -8.0f32..8.0, index in 0usize..64) {
        let names = ScoreWeights::names();
        let name = names[index % names.len()];
        let mut weights = ScoreWeights::default();
        prop_assert!(weights.set(name, value));
        prop_assert_eq!(weights.get(name).to_bits(), value.to_bits());
        for other in names.iter().filter(|other| **other != name) {
            prop_assert_eq!(weights.get(other).to_bits(), ScoreWeights::default().get(other).to_bits());
        }
        prop_assert!(!weights.set("not-a-signal", value));
    }
}

/// A film watched `days` ago, on disk ~1.1 years, 20 GiB.
fn watched_film(days: f32) -> ArchiveCard {
    let mut card = crate::golden::golden_movie();
    card.is_watched = Some(true);
    card.last_watched_days = Some(days);
    card.added_days_ago = 400.0;
    card.size_bytes = 20 * (1u64 << 30);
    card
}

fn plex_context() -> HouseholdContext {
    HouseholdContext { watch_source: Some(WatchSource::Plex), ..Default::default() }
}

fn under_defaults(card: &ArchiveCard, ctx: HouseholdContext) -> f32 {
    let defaults = crate::daemon::RuntimeSettings::default();
    score(card, ctx, &ScoreWeights::default(), defaults.score_temperature).p_safe
}

#[test]
fn a_title_finished_long_ago_clears_the_default_floor() {
    // The 80% goal is only reachable if the shipped priors let watched, cold
    // content through the shipped floor; before `completed_cold` it scored ~0.60
    // and only never-played items could ever be evicted.
    let floor = crate::daemon::RuntimeSettings::default().score_floor;
    let p = under_defaults(&watched_film(250.0), plex_context());
    assert!(p >= floor, "finished 250 d ago must clear the default floor {floor}, got {p:.3}");
}

#[test]
fn a_recently_finished_title_stays_below_the_default_floor() {
    let floor = crate::daemon::RuntimeSettings::default().score_floor;
    let p = under_defaults(&watched_film(60.0), plex_context());
    assert!(p < floor, "finished 60 d ago must be held, got {p:.3}");
}

#[test]
fn a_finished_season_is_not_kept_alive_by_its_own_show() {
    // A completed sibling argues for an *unfinished* season; for a finished one
    // it only says the household finished the show.
    let mut card = watched_film(400.0);
    card.kind = crate::card::LibraryKind::Season;
    card.is_watched = None;
    card.season_state = Some(SeasonState::Completed);
    card.show_title = Some("Done Show".to_string());
    let alone = under_defaults(&card, plex_context());
    let finished_show = under_defaults(&card, HouseholdContext { sibling_season_completed: true, siblings: 3, ..plex_context() });
    assert_eq!(alone, finished_show);
}
