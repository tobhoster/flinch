//! The executor against the in-memory Maintainerr: verified read-back,
//! failure retention, ordering between a card's steps, and dry-run.

use super::super::{
    execute, observe, plan_sync, Caps, CollectionTitles, Desired, Outcome, OwnedState, ProtectedEntry, ScheduledEntry, SyncItem,
    SyncReport, SyncSummary,
};
use super::fake::{Fake, Fault, Op};
use super::{
    current, item, leaving, movie, movie_ids, row, season, season_ids, titles, valid_collections, GIB, LEAVING_MOVIES, MOVIES, SEASONS,
};
use crate::card::LibraryKind;
use rstest::rstest;
use std::collections::BTreeSet;

fn fake() -> Fake {
    Fake::new(current(), valid_collections())
}

fn desired(protect: &[SyncItem], evict: &[SyncItem]) -> Desired {
    Desired {
        protect: protect.to_vec(),
        evict: evict.to_vec(),
        announced: Default::default(),
        collections: titles(),
        gone: Default::default(),
        seerr_configured: false,
    }
}

/// One daemon cycle: observe, plan, execute.
async fn cycle(fake: &mut Fake, desired: &Desired, owned: &mut OwnedState) -> SyncReport {
    let items: Vec<SyncItem> = desired.protect.iter().chain(&desired.evict).cloned().collect();
    let observed = observe(fake, &items, &desired.collections, owned).await.expect("the fake always answers");
    let plan = plan_sync(desired, &observed, owned, &Caps::new(100, 1000));
    execute(fake, plan, &observed, owned, 42).await
}

fn outcomes(report: &SyncReport) -> Vec<(String, Outcome)> {
    report.results().map(|(action, outcome)| (action.to_string(), outcome.clone())).collect()
}

fn a_season() -> SyncItem {
    item("sonarr-2-s1", LibraryKind::Season, Some(season_ids("200", "201")), 2 * GIB)
}

fn a_movie(card: &str, rating_key: &str) -> SyncItem {
    item(card, LibraryKind::Movie, Some(movie_ids(rating_key)), 3 * GIB)
}

#[tokio::test]
async fn a_protection_owns_exactly_the_rows_its_post_created() {
    let mut maintainerr = fake();
    // The operator excluded another season of the same show.
    maintainerr.rows.push(row(50, "202", "200"));
    let mut owned = OwnedState::default();
    let want = desired(&[a_season()], &[]);

    let first = cycle(&mut maintainerr, &want, &mut owned).await;

    assert!(first.outcomes.iter().all(|o| *o == Outcome::Done), "{:?}", outcomes(&first));
    let created: Vec<i64> = maintainerr.rows.iter().filter(|r| r.id != 50).map(|r| r.id).collect();
    assert_eq!(created.len(), 2, "the season's row and its episode's row");
    assert_eq!(owned.protected["sonarr-2-s1"], ProtectedEntry { target: season("200", "201"), exclusion_ids: created });
    let second = cycle(&mut maintainerr, &want, &mut owned).await;
    assert!(second.plan.actions.is_empty(), "an owned exclusion is not re-sent");
    assert_eq!(second.plan.already_protected, 1);
}

#[tokio::test]
async fn an_item_gone_from_plex_loses_flinchs_exclusion_and_the_operators_stays() {
    let mut maintainerr = fake();
    // The operator excluded another season of the same show.
    maintainerr.rows.push(row(50, "202", "200"));
    let mut owned = OwnedState::default();
    cycle(&mut maintainerr, &desired(&[a_season()], &[]), &mut owned).await;
    assert!(owned.is_protected("sonarr-2-s1"));

    // The season left the library and Plex.
    let gone = Desired { gone: ["sonarr-2-s1".to_string()].into(), ..desired(&[], &[]) };
    let report = cycle(&mut maintainerr, &gone, &mut owned).await;

    assert!(report.outcomes.iter().all(|o| *o == Outcome::Done), "{:?}", outcomes(&report));
    assert!(!owned.is_protected("sonarr-2-s1"), "nothing of FLINCH's is left for it");
    assert_eq!(maintainerr.rows.iter().map(|r| r.id).collect::<Vec<_>>(), [50]);
    let summary = SyncSummary::new(&report, false);
    assert_eq!((summary.released_gone, summary.exclusions_removed), (1, 2), "one season: its row and its episode's row");
}

#[rstest]
#[case::locked(Fault::Status(409))]
#[case::code_0(Fault::Refused)]
#[case::accepted_but_absent(Fault::Lie)]
#[tokio::test]
async fn a_failed_protection_is_not_recorded_and_is_retried(#[case] fault: Fault) {
    let mut maintainerr = fake();
    maintainerr.fail(Op::AddExclusion, fault);
    let mut owned = OwnedState::default();
    let want = desired(&[a_movie("radarr-1", "100")], &[]);

    let first = cycle(&mut maintainerr, &want, &mut owned).await;
    assert!(matches!(first.outcomes[..], [Outcome::Failed(_) | Outcome::Unverified(_)]), "{:?}", outcomes(&first));
    assert!(owned.protected.is_empty());
    assert_eq!(SyncSummary::new(&first, false).failures, 1);

    let retry = cycle(&mut maintainerr, &want, &mut owned).await;
    assert_eq!(retry.outcomes, [Outcome::Done]);
    assert!(owned.is_protected("radarr-1"));
}

#[tokio::test]
async fn an_unverified_unschedule_blocks_the_keep_until_it_lands() {
    let mut maintainerr = fake();
    maintainerr.members.entry(SEASONS).or_default().insert("201".into());
    maintainerr.fail(Op::RemoveFromCollection, Fault::Lie);
    let mut owned = OwnedState::default();
    owned.scheduled.insert("sonarr-2-s1".into(), ScheduledEntry { target: season("200", "201"), collection_id: SEASONS, added_at: 1 });
    let want = desired(&[a_season()], &[]);

    let first = cycle(&mut maintainerr, &want, &mut owned).await;
    assert!(matches!(first.outcomes[..], [Outcome::Unverified(_), Outcome::Skipped]), "{:?}", outcomes(&first));
    assert!(maintainerr.rows.is_empty(), "no exclusion before the membership is gone");
    assert!(owned.scheduled.contains_key("sonarr-2-s1"), "still FLINCH's to remove");

    let retry = cycle(&mut maintainerr, &want, &mut owned).await;
    assert_eq!(retry.outcomes, [Outcome::Done, Outcome::Done]);
    assert!(owned.scheduled.is_empty() && owned.is_protected("sonarr-2-s1"));
}

#[tokio::test]
async fn a_failed_release_keeps_the_item_out_of_the_collection() {
    let mut maintainerr = fake();
    maintainerr.rows.push(row(7, "100", "100"));
    maintainerr.fail(Op::RemoveExclusion, Fault::Status(500));
    let mut owned = OwnedState::default();
    owned.protected.insert("radarr-1".into(), ProtectedEntry { target: movie("100"), exclusion_ids: vec![7] });

    let report = cycle(&mut maintainerr, &desired(&[], &[a_movie("radarr-1", "100")]), &mut owned).await;

    assert!(matches!(report.outcomes[..], [Outcome::Failed(_), Outcome::Skipped]), "{:?}", outcomes(&report));
    assert!(maintainerr.members[&MOVIES].is_empty());
    assert!(owned.is_protected("radarr-1") && owned.scheduled.is_empty());
}

#[tokio::test]
async fn the_report_lists_only_verified_new_adds() {
    let mut maintainerr = fake();
    maintainerr.members.entry(MOVIES).or_default().insert("100".into());
    maintainerr.fail(Op::AddToCollection, Fault::Lie);
    let mut owned = OwnedState::default();
    // radarr-1 is already a member; radarr-2's add is swallowed; radarr-3 lands.
    let evict = [a_movie("radarr-1", "100"), a_movie("radarr-2", "110"), a_movie("radarr-3", "120")];

    let report = cycle(&mut maintainerr, &desired(&[], &evict), &mut owned).await;

    assert_eq!(report.scheduled().collect::<Vec<_>>(), [("radarr-3", 3 * GIB)]);
    let summary = SyncSummary::new(&report, false);
    assert_eq!((summary.scheduled, summary.already_scheduled, summary.failures), (1, 1, 1));
    assert_eq!(owned.scheduled.keys().collect::<Vec<_>>(), ["radarr-3"], "a member FLINCH did not add is not FLINCH's");
}

#[tokio::test]
async fn a_dry_run_prints_every_write_and_records_nothing() {
    let mut maintainerr = fake();
    maintainerr.simulated = true;
    let mut owned = OwnedState::default();
    let want = desired(&[a_movie("radarr-1", "100")], &[a_season()]);

    let report = cycle(&mut maintainerr, &want, &mut owned).await;

    assert_eq!(report.outcomes, [Outcome::DryRun, Outcome::DryRun]);
    assert_eq!(maintainerr.writes.len(), 2);
    assert!(maintainerr.rows.is_empty() && maintainerr.members.values().all(|m| m.is_empty()));
    assert_eq!(owned, OwnedState::default());
    assert_eq!(SyncSummary::new(&report, true).simulated, 2);
}

#[tokio::test]
async fn a_membership_maintainerr_already_deleted_is_forgotten() {
    let mut maintainerr = fake();
    let mut owned = OwnedState::default();
    owned.scheduled.insert("radarr-1".into(), ScheduledEntry { target: movie("100"), collection_id: MOVIES, added_at: 1 });

    let report = cycle(&mut maintainerr, &desired(&[], &[]), &mut owned).await;

    assert!(report.plan.actions.is_empty(), "nothing to remove");
    assert!(owned.scheduled.is_empty());
}

#[tokio::test]
async fn without_a_leaving_soon_title_an_unwatched_item_waits_and_the_status_says_why() {
    let mut maintainerr = fake();
    let mut owned = OwnedState::default();
    let evict = [a_movie("radarr-1", "100"), a_movie("radarr-2", "110")];
    let want = Desired { announced: BTreeSet::from(["radarr-1".to_string()]), ..desired(&[], &evict) };

    let report = cycle(&mut maintainerr, &want, &mut owned).await;

    assert_eq!(maintainerr.members[&MOVIES], BTreeSet::from(["110".to_string()]), "only the watched item is handed over");
    assert_eq!(owned.scheduled.keys().collect::<Vec<_>>(), ["radarr-2"]);
    let problems = SyncSummary::new(&report, false).problems;
    assert!(problems.iter().any(|problem| problem.starts_with("Leaving Soon movie collection has no title")), "{problems:?}");
    assert!(problems.contains(&"radarr-1: collection misconfigured".to_string()), "{problems:?}");
}

#[tokio::test]
async fn an_unwatched_item_in_a_delete_collection_flinch_lost_track_of_is_held_until_it_is_out() {
    let mut maintainerr = fake();
    maintainerr.collections.push(leaving(LEAVING_MOVIES, "movie", "1"));
    maintainerr.members.entry(LEAVING_MOVIES).or_default();
    let mut want = desired(&[], &[a_movie("radarr-1", "100")]);
    want.collections.leaving = "Leaving Soon".into();
    // Handed over while it counted as watched, then the record of it was lost:
    // a stop before scheduled.json was written.
    cycle(&mut maintainerr, &want, &mut OwnedState::default()).await;
    assert_eq!(maintainerr.members[&MOVIES], BTreeSet::from(["100".to_string()]));
    let mut owned = OwnedState::default();

    // Now nobody finished it.
    want.announced.insert("radarr-1".into());
    let held = cycle(&mut maintainerr, &want, &mut owned).await;

    assert_eq!(outcomes(&held), [("protect radarr-1 (ratingKey 100)".to_string(), Outcome::Done)]);
    assert!(maintainerr.members[&MOVIES].contains("100"), "a membership FLINCH does not own is never removed");
    assert!(maintainerr.members[&LEAVING_MOVIES].is_empty(), "not announced while it can leave unwarned");
    let problems = SyncSummary::new(&held, false).problems;
    assert!(problems.iter().any(|problem| problem.starts_with("radarr-1: held:") && problem.contains("collection 10")), "{problems:?}");

    // The operator takes it out of the delete collection: it is announced.
    maintainerr.members.entry(MOVIES).or_default().clear();
    let announced = cycle(&mut maintainerr, &want, &mut owned).await;

    assert!(announced.outcomes.iter().all(|o| *o == Outcome::Done), "{:?}", outcomes(&announced));
    assert_eq!(maintainerr.members[&LEAVING_MOVIES], BTreeSet::from(["100".to_string()]));
    assert!(maintainerr.rows.is_empty(), "its exclusion is released");
    assert_eq!(owned.scheduled["radarr-1"].collection_id, LEAVING_MOVIES);
}

#[tokio::test]
async fn a_deferred_move_into_leaving_soon_is_a_take_back() {
    let mut maintainerr = fake();
    maintainerr.collections.push(leaving(LEAVING_MOVIES, "movie", "1"));
    maintainerr.members.entry(MOVIES).or_default().insert("100".into());
    let mut owned = OwnedState::default();
    owned.scheduled.insert("radarr-1".into(), ScheduledEntry { target: movie("100"), collection_id: MOVIES, added_at: 1 });
    // Handed over while it counted as watched; now nobody finished it.
    let evict = [a_movie("radarr-1", "100")];
    let want = Desired {
        announced: BTreeSet::from(["radarr-1".to_string()]),
        collections: CollectionTitles { leaving: "Leaving Soon".to_string(), ..titles() },
        ..desired(&[], &evict)
    };

    let observed = observe(&mut maintainerr, &evict, &want.collections, &owned).await.expect("the fake always answers");
    let plan = plan_sync(&want, &observed, &owned, &Caps::new(0, 0));
    let report = execute(&mut maintainerr, plan, &observed, &mut owned, 42).await;

    assert_eq!(report.outcomes, [Outcome::Done]);
    assert!(maintainerr.members.values().all(BTreeSet::is_empty), "in no collection: {:?}", maintainerr.members);
    assert!(owned.scheduled.is_empty(), "nothing of FLINCH's is on its way out");
    assert_eq!(report.taken_back().collect::<Vec<_>>(), ["radarr-1"], "the ledger forgets it");
    let summary = SyncSummary::new(&report, false);
    assert_eq!((summary.unscheduled, summary.scheduled, summary.deferred), (1, 0, 1));
}

/// radarr-1 was announced into Leaving Soon and is still evicted, but no
/// longer announced: its route is now the delete collection.
async fn reroute(maintainerr: &mut Fake, owned: &mut OwnedState) -> SyncReport {
    maintainerr.collections.push(leaving(LEAVING_MOVIES, "movie", "1"));
    maintainerr.members.entry(LEAVING_MOVIES).or_default().insert("100".into());
    owned.scheduled.insert("radarr-1".into(), ScheduledEntry { target: movie("100"), collection_id: LEAVING_MOVIES, added_at: 1 });
    let mut want = desired(&[], &[a_movie("radarr-1", "100")]);
    want.collections.leaving = "Leaving Soon".into();
    cycle(maintainerr, &want, owned).await
}

#[tokio::test]
async fn a_route_change_is_a_move_not_a_take_back() {
    let (mut maintainerr, mut owned) = (fake(), OwnedState::default());

    let report = reroute(&mut maintainerr, &mut owned).await;

    assert_eq!(report.outcomes, [Outcome::Done, Outcome::Done], "{:?}", outcomes(&report));
    assert_eq!(report.scheduled().collect::<Vec<_>>(), [("radarr-1", 3 * GIB)]);
    assert_eq!(report.taken_back().count(), 0, "still on its way out: the ledger keeps its hand-over");
    let entry = &owned.scheduled["radarr-1"];
    assert_eq!((entry.collection_id, entry.added_at), (MOVIES, 42), "its window restarts at the move");
}

#[tokio::test]
async fn a_move_whose_add_fails_is_a_take_back() {
    let (mut maintainerr, mut owned) = (fake(), OwnedState::default());
    maintainerr.fail(Op::AddToCollection, Fault::Status(409));

    let report = reroute(&mut maintainerr, &mut owned).await;

    assert!(matches!(report.outcomes[..], [Outcome::Done, Outcome::Failed(_)]), "{:?}", outcomes(&report));
    assert_eq!(report.taken_back().collect::<Vec<_>>(), ["radarr-1"], "in no collection: nothing is on its way out");
    assert!(owned.scheduled.is_empty());
}
