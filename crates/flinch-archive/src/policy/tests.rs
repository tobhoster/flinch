use super::*;
use crate::card::LibraryKind;
use crate::golden::{golden_movie, golden_season};
use proptest::prelude::*;

fn card(kind: LibraryKind, title: &str) -> ArchiveCard {
    match kind {
        LibraryKind::Movie => golden_movie(),
        LibraryKind::Season => golden_season(),
    }
    .pipe(|mut c| {
        c.title = title.to_string();
        c
    })
}

trait Pipe: Sized {
    fn pipe<F: FnOnce(Self) -> Self>(self, f: F) -> Self {
        f(self)
    }
}
impl<T> Pipe for T {}
#[test]
fn armed_score_reclaims_never_played_and_disabled_never_does() {
    let mut movie = crate::golden::golden_movie();
    movie.is_watched = Some(false);
    movie.added_days_ago = 200.0;
    movie.size_bytes = 10_000_000_000;
    let verdict = ScoreVerdict { p_safe: 0.78, hard_guard: false, sibling_played: false };

    // Off (the default): unchanged behaviour, the sole copy stays.
    assert!(matches!(decide(&movie, &ArchivePolicy::default(), Some(verdict)), Reason::KeepBecauseNeverWatchedIsSoleCopy));

    let armed = ArchivePolicy { unwatched_reclaim: UnwatchedReclaim { enabled: true, ..Default::default() }, ..ArchivePolicy::default() };
    match decide(&movie, &armed, Some(verdict)) {
        Reason::DeleteUnwatchedByScore { p_safe, days, .. } => {
            assert!((p_safe - 0.78).abs() < 0.001);
            assert!((days - 200.0).abs() < 0.001);
        }
        other => panic!("expected a score-gated reclaim, got {other:?}"),
    }
}

#[test]
fn a_score_gated_reclaim_says_nobody_finished_it() {
    // The rule also takes a movie somebody started: "never played" is false
    // beside its play date, and "nobody finished it" is true of both.
    let armed = ArchivePolicy { unwatched_reclaim: UnwatchedReclaim { enabled: true, ..Default::default() }, ..ArchivePolicy::default() };
    let verdict = ScoreVerdict { p_safe: 0.78, hard_guard: false, sibling_played: false };
    for played in [None, Some(200.0)] {
        let mut movie = golden_movie();
        movie.is_watched = Some(false);
        movie.last_watched_days = played;
        movie.added_days_ago = 400.0;
        assert_eq!(decide(&movie, &armed, Some(verdict)).describe(), "Nobody finished it in 400 days, P(safe) 78%");
    }
}

#[test]
fn a_recent_play_is_called_played_not_watched() {
    // The recency rules keep a movie somebody only started as well as one
    // somebody finished: "watched" is false beside the start, and "played" is
    // true of both.
    for is_watched in [false, true] {
        for (days, reason) in [(10.0, "Played in the last 30 days"), (60.0, "Played recently")] {
            let mut movie = golden_movie();
            movie.is_watched = Some(is_watched);
            movie.last_watched_days = Some(days);
            assert_eq!(decide(&movie, &ArchivePolicy::default(), None).describe(), reason, "watched {is_watched}, played {days} d ago");
        }
    }
}

#[test]
fn armed_score_still_cannot_talk_past_a_guard_or_a_thin_dwell() {
    let armed = ArchivePolicy {
        unwatched_reclaim: UnwatchedReclaim { enabled: true, floor: 0.75, min_dwell_days: 90.0 },
        ..ArchivePolicy::default()
    };
    let strong = ScoreVerdict { p_safe: 0.99, hard_guard: false, sibling_played: false };

    let mut guarded = crate::golden::golden_movie();
    guarded.is_watched = Some(false);
    guarded.added_days_ago = 400.0;
    assert!(matches!(
        decide(&guarded, &armed, Some(ScoreVerdict { hard_guard: true, ..strong })),
        Reason::KeepBecauseNeverWatchedIsSoleCopy
    ));

    let mut fresh = crate::golden::golden_movie();
    fresh.is_watched = Some(false);
    fresh.added_days_ago = 12.0;
    assert!(matches!(decide(&fresh, &armed, Some(strong)), Reason::KeepBecauseNeverWatchedIsSoleCopy));

    let weak = ScoreVerdict { p_safe: 0.4, hard_guard: false, sibling_played: false };
    let mut old = crate::golden::golden_movie();
    old.is_watched = Some(false);
    old.added_days_ago = 400.0;
    assert!(matches!(decide(&old, &armed, Some(weak)), Reason::KeepBecauseNeverWatchedIsSoleCopy));
}

#[rstest::rstest]
#[case::movie(LibraryKind::Movie)]
#[case::season(LibraryKind::Season)]
fn an_item_without_watch_evidence_is_kept_however_large_old_and_safe_it_scores(#[case] kind: LibraryKind) {
    // Size is uncapped in the score and dwell counts up to four years, so a
    // large, old file with no evidence can clear the floor on the priors alone.
    let armed = ArchivePolicy {
        unwatched_reclaim: UnwatchedReclaim { enabled: true, floor: 0.75, min_dwell_days: 90.0 },
        ..ArchivePolicy::default()
    };
    let strong = ScoreVerdict { p_safe: 0.99, hard_guard: false, sibling_played: false };
    // What `watch::apply` leaves on an item no source reported on.
    let mut item = card(kind, "unknown");
    item.is_watched = None;
    item.season_state = None;
    item.last_watched_days = None;
    item.added_days_ago = 4.0 * 365.0;
    item.size_bytes = 300_000_000_000;
    assert_eq!(decide(&item, &armed, Some(strong)), Reason::KeepBecauseNoWatchEvidence);

    // A source that saw it and found no play is the evidence the rule acts on.
    match kind {
        LibraryKind::Movie => item.is_watched = Some(false),
        LibraryKind::Season => item.season_state = Some(SeasonState::Empty),
    }
    assert!(matches!(decide(&item, &armed, Some(strong)), Reason::DeleteUnwatchedByScore { .. }));
}

proptest! {
    #[test]
    fn an_item_without_watch_evidence_never_leaves_as_unplayed(
        season in any::<bool>(),
        p_safe in 0.0f32..=1.0,
        verdict_flags in (any::<bool>(), any::<bool>(), any::<bool>()),
        terms in (0.0f32..=1.0, 0.0f32..400.0),
        added_days_ago in 0.0f32..3000.0,
        size_bytes in 0u64..2_000_000_000_000,
        last_watched_days in proptest::option::of(0.0f32..800.0),
        is_newest_season in proptest::option::of(any::<bool>()),
        duplicates in (any::<bool>(), 0u32..3),
    ) {
        let (has_verdict, hard_guard, sibling_played) = verdict_flags;
        let (floor, min_dwell_days) = terms;
        let (dedupe_movies, duplicate_count) = duplicates;
        let armed = ArchivePolicy {
            unwatched_reclaim: UnwatchedReclaim { enabled: true, floor, min_dwell_days },
            dedupe_movies,
            ..ArchivePolicy::default()
        };
        let mut item = card(if season { LibraryKind::Season } else { LibraryKind::Movie }, "unknown");
        // No source reported on it: whatever the card said, the watch state is
        // what `watch::apply` leaves.
        crate::watch::apply(std::slice::from_mut(&mut item), &std::collections::HashMap::new());
        item.added_days_ago = added_days_ago;
        item.size_bytes = size_bytes;
        item.last_watched_days = last_watched_days;
        item.is_newest_season = is_newest_season;
        item.duplicate_count = duplicate_count;
        let verdict = has_verdict.then_some(ScoreVerdict { p_safe, hard_guard, sibling_played });

        let reason = decide(&item, &armed, verdict);
        prop_assert!(!matches!(reason, Reason::DeleteUnwatchedByScore { .. }), "{reason:?}");
        // The one way out without evidence is as a duplicate, whose other copy stays.
        prop_assert!(reclaims_bytes(&reason) == 0 || matches!(reason, Reason::DeleteDuplicate { .. }), "{reason:?}");
    }
}

#[test]
fn a_watched_item_without_a_date_is_held_not_called_stale() {
    // Live shape: Plex says "10 of 10 episodes watched" and gives no
    // timestamp. Nothing about that says the household watched it long ago.
    let mut season = crate::golden::golden_season();
    season.season_state = Some(SeasonState::Completed);
    season.last_watched_days = None;
    season.added_days_ago = 900.0;
    assert_eq!(decide(&season, &ArchivePolicy::default(), None), Reason::KeepBecauseWatchedUndated);

    let mut movie = crate::golden::golden_movie();
    movie.is_watched = Some(true);
    movie.last_watched_days = None;
    movie.added_days_ago = 900.0;
    assert_eq!(decide(&movie, &ArchivePolicy::default(), None), Reason::KeepBecauseWatchedUndated);

    // With a date, the normal retention rule applies again.
    movie.last_watched_days = Some(400.0);
    assert!(matches!(decide(&movie, &ArchivePolicy::default(), None), Reason::DeleteWatchedUntouched { .. }));
}

#[test]
fn an_unplayed_season_of_an_active_show_is_never_reclaimed() {
    // The one failure class a real household's history produced: two seasons
    // scored 78% (never played) thirty days before the whole show was watched.
    // Once a sibling shows activity, the answer must be no.
    let armed = ArchivePolicy {
        unwatched_reclaim: UnwatchedReclaim { enabled: true, floor: 0.75, min_dwell_days: 90.0 },
        ..ArchivePolicy::default()
    };
    let verdict = ScoreVerdict { p_safe: 0.78, hard_guard: false, sibling_played: true };
    let mut season = crate::golden::golden_season();
    season.season_state = Some(SeasonState::Empty);
    season.is_newest_season = Some(false);
    season.added_days_ago = 400.0;
    assert!(matches!(decide(&season, &armed, Some(verdict)), Reason::KeepBecauseNotCompleted));

    // Same item, no sibling activity: the armed rule applies.
    let quiet = ScoreVerdict { sibling_played: false, ..verdict };
    assert!(matches!(decide(&season, &armed, Some(quiet)), Reason::DeleteUnwatchedByScore { .. }));
}

#[test]
fn armed_score_covers_unplayed_seasons_but_never_partial_ones() {
    let armed = ArchivePolicy { unwatched_reclaim: UnwatchedReclaim { enabled: true, ..Default::default() }, ..ArchivePolicy::default() };
    let strong = ScoreVerdict { p_safe: 0.9, hard_guard: false, sibling_played: false };

    let mut empty = crate::golden::golden_season();
    empty.season_state = Some(SeasonState::Empty);
    empty.is_newest_season = Some(false);
    empty.added_days_ago = 200.0;
    assert!(matches!(decide(&empty, &armed, Some(strong)), Reason::DeleteUnwatchedByScore { .. }));

    let mut partial = crate::golden::golden_season();
    partial.season_state = Some(SeasonState::Partial);
    partial.is_newest_season = Some(false);
    partial.added_days_ago = 200.0;
    assert!(matches!(decide(&partial, &armed, Some(strong)), Reason::KeepBecauseNotCompleted));
}

#[test]
fn watched_old_movie_high_rewatch_value_is_kept_unwatched_sole_copy_is_kept() {
    let policy = ArchivePolicy::default();
    let mut m = card(LibraryKind::Movie, "kept");
    m.is_watched = Some(true);
    m.rewatch_score = Some(0.9);
    m.last_watched_days = Some(300.0);
    assert!(matches!(decide(&m, &policy, None), Reason::KeepBecauseLowDuplicateValue));

    m.rewatch_score = Some(0.0);
    m.is_watched = Some(false);
    assert!(matches!(decide(&m, &policy, None), Reason::KeepBecauseNeverWatchedIsSoleCopy));
}
