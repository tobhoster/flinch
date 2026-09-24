//! The planner's rules, one scenario each.

use super::super::{
    operator_keeps, plan_sync, Blocked, Caps, CollectionTitles, Desired, ExclusionRow, Handover, MaintainerrVersion,
    Observed, OwnedState, ProtectedEntry, ScheduledEntry, SyncAction, SyncItem,
};
use super::{
    current, item, leaving, movie, movie_ids, row, season, season_ids, titles, valid_collections, GIB, MOVIES, SEASONS,
};
use crate::card::LibraryKind;
use rstest::rstest;
use std::collections::{BTreeMap, BTreeSet};

/// What Maintainerr would answer for these items, given all of its rows.
fn observed(items: &[&SyncItem], rows: &[ExclusionRow], members: &[(i64, &str)]) -> Observed {
    let mut exclusions = BTreeMap::new();
    for target in items.iter().filter_map(|item| item.target()) {
        let key = target.media_id().to_string();
        let found = rows.iter().filter(|r| r.media_server_id == key || r.parent.as_deref() == Some(key.as_str())).cloned().collect();
        exclusions.insert(key, found);
    }
    let mut observed_members: BTreeMap<i64, BTreeSet<String>> =
        valid_collections().iter().map(|c| (c.id, BTreeSet::new())).collect();
    for (collection_id, key) in members {
        observed_members.entry(*collection_id).or_default().insert(key.to_string());
    }
    Observed { version: current(), collections: valid_collections(), members: observed_members, exclusions }
}

fn desired(protect: &[&SyncItem], evict: &[&SyncItem]) -> Desired {
    let owned = |items: &[&SyncItem]| items.iter().map(|item| (*item).clone()).collect();
    Desired {
        protect: owned(protect),
        evict: owned(evict),
        announced: BTreeSet::new(),
        collections: titles(),
        gone: BTreeSet::new(),
        seerr_configured: false,
    }
}

fn film(card: &str, rating_key: &str, gib: u64) -> SyncItem {
    item(card, LibraryKind::Movie, Some(movie_ids(rating_key)), gib * GIB)
}

const OPEN: Caps = Caps { max_items: 100, max_bytes: u64::MAX };

#[test]
fn an_eviction_first_releases_flinchs_own_exclusion_then_schedules() {
    let evicted = film("radarr-1", "100", 5);
    let mut owned = OwnedState::default();
    owned.protected.insert("radarr-1".into(), ProtectedEntry { target: movie("100"), exclusion_ids: vec![7] });

    let plan = plan_sync(&desired(&[], &[&evicted]), &observed(&[&evicted], &[row(7, "100", "100")], &[]), &owned, &OPEN);

    assert_eq!(
        plan.actions,
        [
            SyncAction::RemoveExclusion { card_id: "radarr-1".into(), target: movie("100"), exclusion_id: 7 },
            SyncAction::Schedule { card_id: "radarr-1".into(), target: movie("100"), collection_id: MOVIES, bytes: 5 * GIB },
        ]
    );
}

#[test]
fn an_operator_exclusion_is_an_operator_keep_and_is_never_touched() {
    let kept = film("radarr-1", "100", 5);
    let evicted = item("sonarr-2-s1", LibraryKind::Season, Some(season_ids("200", "201")), GIB);
    // The operator excluded the movie, and the whole show of the season.
    let rows = [row(3, "100", "100"), row(4, "200", "200")];
    let seen = observed(&[&kept, &evicted], &rows, &[]);

    let plan = plan_sync(&desired(&[&kept], &[&evicted]), &seen, &OwnedState::default(), &OPEN);

    assert!(plan.actions.is_empty(), "{:?}", plan.actions);
    assert_eq!(plan.operator_keeps, ["sonarr-2-s1", "radarr-1"]);
    let keeps = operator_keeps(&[kept, evicted], &seen, &OwnedState::default());
    assert_eq!(keeps.len(), 2, "the daemon sets the keep guard for both before planning");
}

#[test]
fn members_are_skipped_without_consuming_the_caps() {
    let member = film("radarr-1", "100", 40);
    let next = film("radarr-2", "110", 40);
    let later = film("radarr-3", "120", 1);
    let seen = observed(&[&member, &next, &later], &[], &[(MOVIES, "100")]);

    let plan = plan_sync(&desired(&[], &[&member, &next, &later]), &seen, &OwnedState::default(), &Caps::new(1, 50));

    let scheduled: Vec<&str> = plan.actions.iter().map(SyncAction::card_id).collect();
    assert_eq!(scheduled, ["radarr-2"]);
    assert_eq!(plan.already_scheduled, 1);
    assert_eq!(plan.deferred, ["radarr-3"], "the item cap is spent on the one new add");
}

#[test]
fn the_byte_cap_is_checked_before_each_add_and_a_smaller_later_item_still_fits() {
    let items = [film("radarr-1", "100", 30), film("radarr-2", "110", 30), film("radarr-3", "120", 10)];
    let refs: Vec<&SyncItem> = items.iter().collect();

    let plan = plan_sync(&desired(&[], &refs), &observed(&refs, &[], &[]), &OwnedState::default(), &Caps::new(10, 50));

    let scheduled: Vec<&str> = plan.actions.iter().map(SyncAction::card_id).collect();
    assert_eq!(scheduled, ["radarr-1", "radarr-3"]);
    assert_eq!(plan.deferred, ["radarr-2"], "30 + 30 GiB would overshoot the 50 GiB cap");
}

#[test]
fn an_item_larger_than_the_byte_cap_goes_alone_as_the_runs_first_add() {
    let items = [film("radarr-1", "100", 60), film("radarr-2", "110", 10)];
    let refs: Vec<&SyncItem> = items.iter().collect();

    let plan = plan_sync(&desired(&[], &refs), &observed(&refs, &[], &[]), &OwnedState::default(), &Caps::new(10, 50));

    let scheduled: Vec<&str> = plan.actions.iter().map(SyncAction::card_id).collect();
    assert_eq!(scheduled, ["radarr-1"], "deferred every run, it would hold its volume over the ceiling for good");
    assert_eq!(plan.deferred, ["radarr-2"], "the run's byte budget is spent");
}

#[test]
fn a_card_leaving_the_evict_list_is_unscheduled_before_it_is_protected() {
    let flipped = item("sonarr-2-s1", LibraryKind::Season, Some(season_ids("200", "201")), GIB);
    let mut owned = OwnedState::default();
    owned.scheduled.insert(
        "sonarr-2-s1".into(),
        ScheduledEntry { target: season("200", "201"), collection_id: SEASONS, added_at: 1 },
    );

    let plan = plan_sync(&desired(&[&flipped], &[]), &observed(&[&flipped], &[], &[(SEASONS, "201")]), &owned, &OPEN);

    assert_eq!(
        plan.actions,
        [
            SyncAction::Unschedule { card_id: "sonarr-2-s1".into(), target: season("200", "201"), collection_id: SEASONS },
            SyncAction::Protect { card_id: "sonarr-2-s1".into(), target: season("200", "201") },
        ]
    );
}

#[test]
fn a_card_without_plex_ids_is_unresolved_and_left_alone() {
    let no_ids = item("radarr-1", LibraryKind::Movie, None, GIB);
    let no_season_key = item("sonarr-2-s1", LibraryKind::Season, Some(movie_ids("200")), GIB);

    let plan = plan_sync(&desired(&[&no_ids], &[&no_season_key]), &observed(&[], &[], &[]), &OwnedState::default(), &OPEN);

    assert!(plan.actions.is_empty());
    assert_eq!(plan.unresolved, ["sonarr-2-s1", "radarr-1"]);
}

#[test]
fn a_misconfigured_kind_hands_over_nothing_of_that_kind() {
    let picture = film("radarr-1", "100", 1);
    let episode = item("sonarr-2-s1", LibraryKind::Season, Some(season_ids("200", "201")), GIB);
    let mut seen = observed(&[&picture, &episode], &[], &[]);
    seen.collections[1].arr_action = 3;

    let plan = plan_sync(&desired(&[], &[&picture, &episode]), &seen, &OwnedState::default(), &OPEN);

    let scheduled: Vec<&str> = plan.actions.iter().map(SyncAction::card_id).collect();
    assert_eq!(scheduled, ["radarr-1"]);
    assert_eq!(plan.blocked, [("sonarr-2-s1".to_string(), Blocked::CollectionMisconfigured)]);
    assert_eq!(plan.misconfigured.len(), 1);
}

#[test]
fn an_item_from_another_plex_section_is_blocked() {
    let mut elsewhere = film("radarr-1", "100", 1);
    elsewhere.plex = Some(crate::ids::PlexIds { section_id: Some(9), ..movie_ids("100") });

    let plan = plan_sync(&desired(&[], &[&elsewhere]), &observed(&[&elsewhere], &[], &[]), &OwnedState::default(), &OPEN);

    assert!(plan.actions.is_empty());
    assert_eq!(plan.blocked, [("radarr-1".to_string(), Blocked::NoCollectionForSection(Some(9)))]);
}

#[test]
fn an_old_maintainerr_still_gets_exclusions_but_no_handover() {
    let kept = film("radarr-1", "100", 1);
    let evicted = film("radarr-2", "110", 1);
    let mut seen = observed(&[&kept, &evicted], &[], &[]);
    seen.version = MaintainerrVersion::Release { major: 3, minor: 9, patch: 0 };

    let plan = plan_sync(&desired(&[&kept], &[&evicted]), &seen, &OwnedState::default(), &OPEN);

    assert_eq!(plan.actions, [SyncAction::Protect { card_id: "radarr-1".into(), target: movie("100") }]);
    assert!(matches!(plan.handover, Handover::Refused { .. }));
    assert_eq!(plan.blocked, [("radarr-2".to_string(), Blocked::HandoverRefused)]);
}

#[test]
fn keep_wins_when_a_card_is_in_both_lists() {
    let both = film("radarr-1", "100", 1);

    let plan = plan_sync(&desired(&[&both], &[&both]), &observed(&[&both], &[], &[]), &OwnedState::default(), &OPEN);

    assert_eq!(plan.actions, [SyncAction::Protect { card_id: "radarr-1".into(), target: movie("100") }]);
}

/// A film whose Plex item also holds `copies` (every copy's ratingKey).
fn copied(card: &str, rating_key: &str, copies: &[&str]) -> SyncItem {
    SyncItem { copies: copies.iter().map(|key| key.to_string()).collect(), ..film(card, rating_key, 1) }
}

#[rstest]
#[case::the_same_primary_copy(film("radarr-1", "100", 1), "100")]
#[case::another_copy_of_the_kept_item(copied("radarr-1", "90", &["90", "100"]), "100")]
fn an_eviction_sharing_a_kept_cards_rating_key_is_held(#[case] kept: SyncItem, #[case] shared: &str) {
    let evicted = film("radarr-2", shared, 1);

    let plan = plan_sync(&desired(&[&kept], &[&evicted]), &observed(&[&kept, &evicted], &[], &[]), &OwnedState::default(), &OPEN);

    assert!(!plan.actions.iter().any(|action| matches!(action, SyncAction::Schedule { .. })), "{:?}", plan.actions);
    assert_eq!(
        plan.blocked,
        [("radarr-2".to_string(), Blocked::SharesKeptItem { rating_key: shared.to_string(), kept: "radarr-1".to_string() })]
    );
}

#[test]
fn an_item_plex_holds_in_several_libraries_is_held_and_its_old_hand_over_taken_back() {
    let twice = copied("radarr-1", "100", &["100", "500"]);
    let mut owned = OwnedState::default();
    owned.scheduled.insert("radarr-1".into(), ScheduledEntry { target: movie("100"), collection_id: MOVIES, added_at: 1 });

    let plan = plan_sync(&desired(&[], &[&twice]), &observed(&[&twice], &[], &[(MOVIES, "100")]), &owned, &OPEN);

    assert_eq!(plan.actions, [SyncAction::Unschedule { card_id: "radarr-1".into(), target: movie("100"), collection_id: MOVIES }]);
    assert_eq!(plan.blocked, [("radarr-1".to_string(), Blocked::SeveralPlexCopies(vec!["100".into(), "500".into()]))]);
}

const LEAVING_MOVIES: i64 = 50;
const LEAVING_SEASONS: i64 = 51;

/// The same scene with a Leaving Soon collection per kind, both of which warn.
fn with_leaving(mut seen: Observed) -> Observed {
    seen.collections.extend([leaving(LEAVING_MOVIES, "movie", "1"), leaving(LEAVING_SEASONS, "season", "2")]);
    seen.members.entry(LEAVING_MOVIES).or_default();
    seen.members.entry(LEAVING_SEASONS).or_default();
    seen
}

/// Evictions with `announced` among them, and Leaving Soon named `leaving_title`.
fn announcing(evict: &[&SyncItem], announced: &[&str], leaving_title: &str) -> Desired {
    Desired {
        announced: announced.iter().map(|id| id.to_string()).collect(),
        collections: CollectionTitles { leaving: leaving_title.to_string(), ..titles() },
        ..desired(&[], evict)
    }
}

fn scheduled_into(plan: &super::super::SyncPlan) -> Vec<(&str, i64)> {
    plan.actions
        .iter()
        .filter_map(|action| match action {
            SyncAction::Schedule { card_id, collection_id, .. } => Some((card_id.as_str(), *collection_id)),
            _ => None,
        })
        .collect()
}

#[test]
fn an_unwatched_eviction_is_announced_while_a_watched_one_is_deleted() {
    let (stale, watched) = (film("radarr-1", "100", 5), film("radarr-2", "110", 5));
    let seen = with_leaving(observed(&[&stale, &watched], &[], &[]));

    let plan = plan_sync(&announcing(&[&stale, &watched], &["radarr-1"], "Leaving Soon"), &seen, &OwnedState::default(), &OPEN);

    assert_eq!(scheduled_into(&plan), [("radarr-1", LEAVING_MOVIES), ("radarr-2", MOVIES)]);
    assert_eq!(plan.leaving, BTreeSet::from([LEAVING_MOVIES, LEAVING_SEASONS]));
    assert!(plan.misconfigured.is_empty(), "{:?}", plan.misconfigured);
}

#[test]
fn without_a_leaving_soon_title_announced_items_go_to_the_delete_collection() {
    let stale = film("radarr-1", "100", 5);
    let seen = with_leaving(observed(&[&stale], &[], &[]));

    let plan = plan_sync(&announcing(&[&stale], &["radarr-1"], ""), &seen, &OwnedState::default(), &OPEN);

    assert_eq!(scheduled_into(&plan), [("radarr-1", MOVIES)]);
    assert!(plan.leaving.is_empty());
}

#[test]
fn a_broken_leaving_soon_collection_blocks_unwatched_items_instead_of_deleting_them() {
    let (stale, watched) = (film("radarr-1", "100", 5), film("radarr-2", "110", 5));
    // Named, but it does not exist yet.
    let seen = observed(&[&stale, &watched], &[], &[]);

    let plan = plan_sync(&announcing(&[&stale, &watched], &["radarr-1"], "Leaving Soon"), &seen, &OwnedState::default(), &OPEN);

    assert_eq!(scheduled_into(&plan), [("radarr-2", MOVIES)], "the watched item still goes");
    assert_eq!(plan.blocked, [("radarr-1".to_string(), Blocked::CollectionMisconfigured)]);
    assert!(plan.misconfigured.iter().any(|problem| problem.to_string().starts_with("Leaving Soon movie collection")));
}

#[test]
fn a_leaving_soon_item_that_got_played_is_pulled_back() {
    let stale = film("radarr-1", "100", 5);
    let seen = with_leaving(observed(&[&stale], &[], &[(LEAVING_MOVIES, "100")]));
    let mut owned = OwnedState::default();
    owned.scheduled.insert("radarr-1".into(), ScheduledEntry { target: movie("100"), collection_id: LEAVING_MOVIES, added_at: 1 });

    // Played: no longer evicted.
    let plan = plan_sync(&announcing(&[], &[], "Leaving Soon"), &seen, &owned, &OPEN);

    assert_eq!(
        plan.actions,
        [SyncAction::Unschedule { card_id: "radarr-1".into(), target: movie("100"), collection_id: LEAVING_MOVIES }]
    );
}

#[test]
fn an_item_whose_route_changes_leaves_its_old_collection_only_once_the_new_one_takes_it() {
    let finished = film("radarr-1", "100", 5);
    let seen = with_leaving(observed(&[&finished], &[], &[(LEAVING_MOVIES, "100")]));
    let mut owned = OwnedState::default();
    owned.scheduled.insert("radarr-1".into(), ScheduledEntry { target: movie("100"), collection_id: LEAVING_MOVIES, added_at: 1 });
    // Watched during its window: now a plain delete.
    let moved = announcing(&[&finished], &[], "Leaving Soon");

    let plan = plan_sync(&moved, &seen, &owned, &OPEN);
    assert_eq!(
        plan.actions,
        [
            SyncAction::Unschedule { card_id: "radarr-1".into(), target: movie("100"), collection_id: LEAVING_MOVIES },
            SyncAction::Schedule { card_id: "radarr-1".into(), target: movie("100"), collection_id: MOVIES, bytes: 5 * GIB },
        ]
    );

    // With no room for the new add this cycle, it stays where it is.
    let full = plan_sync(&moved, &seen, &owned, &Caps::new(0, 0));
    assert!(full.actions.is_empty(), "{:?}", full.actions);
    assert_eq!(full.deferred, ["radarr-1"]);
}

/// FLINCH's own exclusion for `card`, already in Maintainerr as row `id`.
fn owning(card: &str, target: super::super::MaintainerrTarget, id: i64) -> OwnedState {
    let mut owned = OwnedState::default();
    owned.protected.insert(card.into(), ProtectedEntry { target, exclusion_ids: vec![id] });
    owned
}

#[test]
fn an_exclusion_on_an_item_gone_from_plex_is_released_but_never_the_operators_row() {
    let owned = owning("sonarr-9-s1", season("900", "901"), 7);
    let mut seen = observed(&[], &[], &[]);
    // FLINCH's row for the season, and the operator's own row for another season of the show.
    seen.exclusions.insert("900".into(), vec![row(7, "901", "900"), row(3, "902", "900")]);
    let mut wanted = desired(&[], &[]);
    wanted.gone.insert("sonarr-9-s1".into());

    let plan = plan_sync(&wanted, &seen, &owned, &OPEN);

    assert_eq!(
        plan.actions,
        [SyncAction::RemoveExclusion { card_id: "sonarr-9-s1".into(), target: season("900", "901"), exclusion_id: 7 }]
    );
    assert_eq!(plan.gone, BTreeSet::from(["sonarr-9-s1".to_string()]));
}

#[test]
fn a_card_still_kept_is_never_released_as_gone() {
    let kept = film("radarr-1", "100", 5);
    let owned = owning("radarr-1", movie("100"), 7);
    let mut wanted = desired(&[&kept], &[]);
    wanted.gone.insert("radarr-1".into());

    let plan = plan_sync(&wanted, &observed(&[&kept], &[row(7, "100", "100")], &[]), &owned, &OPEN);

    assert!(plan.actions.is_empty(), "{:?}", plan.actions);
    assert_eq!((plan.already_protected, plan.gone.len()), (1, 0));
}

#[rstest]
#[case::seerr_and_no_force(true, false, 2)]
#[case::seerr_and_force(true, true, 0)]
#[case::no_seerr(false, false, 0)]
fn a_collection_leaving_seerr_requests_behind_is_a_warning_that_blocks_nothing(
    #[case] seerr_configured: bool,
    #[case] force_seerr: bool,
    #[case] warnings: usize,
) {
    let evicted = film("radarr-1", "100", 5);
    let mut seen = observed(&[&evicted], &[], &[]);
    for collection in &mut seen.collections {
        collection.force_seerr = force_seerr;
    }
    let wanted = Desired { seerr_configured, ..desired(&[], &[&evicted]) };

    let plan = plan_sync(&wanted, &seen, &OwnedState::default(), &OPEN);

    assert_eq!(plan.warnings.len(), warnings, "{:?}", plan.warnings);
    assert!(plan.warnings.iter().all(|warning| warning.contains("Force delete Seerr request")));
    assert_eq!(scheduled_into(&plan), [("radarr-1", MOVIES)], "a warning never holds an eviction back");
}
