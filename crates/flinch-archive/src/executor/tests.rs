//! The native lifecycle's transitions, and who acts, as the daemon asks them.

use super::lifecycle::{plan, Action, Held, Inputs, Item, Route, Withdrawn};
use super::{acting, Acting, Executor, Leaving, NativeState};
use crate::card::LibraryKind;
use crate::maintainerr::Caps;
use rstest::rstest;
use std::collections::{HashMap, HashSet};

const DAY: u64 = 86_400;
const NOW: u64 = 100 * DAY;
const WINDOW: u64 = 14 * DAY;
const GIB: u64 = 1 << 30;

/// One cycle's world: every named card is on disk with evidence `last`, and
/// selected unless dropped.
struct World {
    on_disk: Vec<&'static str>,
    evidence: HashMap<&'static str, Option<u64>>,
    selected: Vec<&'static str>,
    pinned: Vec<&'static str>,
    complete: bool,
    shelf: Option<&'static str>,
    server: super::ShelfServer,
    caps: Caps,
    max_deletes: usize,
}

impl World {
    fn new(cards: &[(&'static str, Option<u64>)]) -> Self {
        Self {
            on_disk: cards.iter().map(|(id, _)| *id).collect(),
            evidence: cards.iter().copied().collect(),
            selected: cards.iter().map(|(id, _)| *id).collect(),
            pinned: Vec::new(),
            complete: true,
            shelf: Some("Leaving Soon"),
            server: super::ShelfServer::Plex,
            caps: Caps::new(10, 50),
            max_deletes: 10,
        }
    }

    fn plan(&self, state: &NativeState, eligible: &[Item]) -> super::lifecycle::Plan {
        let selected: HashSet<&str> = self.selected.iter().copied().collect();
        let pinned: HashSet<&str> = self.pinned.iter().copied().collect();
        let in_library: HashSet<&str> = self.on_disk.iter().copied().collect();
        let evidence: HashMap<&str, Option<u64>> = self.evidence.iter().map(|(id, last)| (*id, *last)).collect();
        plan(
            state,
            &Inputs {
                eligible,
                selected: &selected,
                pinned: &pinned,
                in_library: &in_library,
                evidence: &evidence,
                complete: self.complete,
                shelf: self.shelf,
                server: self.server,
                caps: self.caps,
                max_deletes: self.max_deletes,
                window_secs: WINDOW,
                now: NOW,
            },
        )
    }
}

fn item(id: &'static str, announce: bool) -> Item<'static> {
    Item { id, bytes: GIB, announce, after: None }
}

/// A shelf holding `id`, announced `days_ago` under the "Leaving Soon" title.
fn shelf(id: &str, days_ago: u64) -> NativeState {
    let announced_at = NOW - days_ago * DAY;
    let mut state = NativeState::default();
    state.leaving.insert(
        id.to_string(),
        Leaving {
            title: id.to_string(),
            kind: LibraryKind::Movie,
            bytes: GIB,
            announced_at,
            until: announced_at + WINDOW,
            shelf: "Leaving Soon".into(),
            collection: "77".into(),
            rating_key: "10".into(),
            server: super::ShelfServer::Plex,
        },
    );
    state
}

fn delete(id: &str, route: Route) -> Action {
    Action::Delete { id: id.into(), route }
}

fn withdraw(id: &str, why: Withdrawn) -> Action {
    Action::Withdraw { id: id.into(), why }
}

#[test]
fn a_finished_item_past_its_grace_runs_is_deleted_at_once() {
    let world = World::new(&[("radarr-1", Some(NOW - 30 * DAY))]);
    let plan = world.plan(&NativeState::default(), &[item("radarr-1", false)]);
    assert_eq!(plan.actions, [delete("radarr-1", Route::Finished)]);
}

#[test]
fn an_unwatched_item_is_announced_then_deleted_only_after_its_window() {
    let world = World::new(&[("radarr-2", None)]);
    let announced = world.plan(&NativeState::default(), &[item("radarr-2", true)]);
    assert_eq!(announced.actions, [Action::Announce { id: "radarr-2".into(), until: NOW + WINDOW }]);

    let waiting = world.plan(&shelf("radarr-2", 13), &[item("radarr-2", true)]);
    assert!(waiting.actions.is_empty(), "a day before its window ends nothing happens: {waiting:?}");

    let due = world.plan(&shelf("radarr-2", 14), &[item("radarr-2", true)]);
    assert_eq!(due.actions, [delete("radarr-2", Route::LeavingSoon)]);
}

#[rstest]
#[case::played_in_the_window(&[("radarr-3", Some(NOW - DAY))], vec![], vec![], Withdrawn::Played)]
#[case::dropped_from_the_plan(&[("radarr-3", None)], vec!["radarr-3"], vec![], Withdrawn::NotSelected)]
#[case::pinned_since(&[("radarr-3", None)], vec![], vec!["radarr-3"], Withdrawn::Pinned)]
fn an_item_on_the_shelf_is_taken_back(
    #[case] cards: &[(&'static str, Option<u64>)],
    #[case] unselect: Vec<&'static str>,
    #[case] pin: Vec<&'static str>,
    #[case] why: Withdrawn,
) {
    let mut world = World::new(cards);
    world.selected.retain(|id| !unselect.contains(id));
    world.pinned = pin;
    // Even past its window: a take-back wins over the delete.
    let plan = world.plan(&shelf("radarr-3", 20), &[]);
    assert_eq!(plan.actions, [withdraw("radarr-3", why)]);
}

#[test]
fn switching_the_shelf_server_takes_items_back_to_restart_their_window() {
    let mut world = World::new(&[("radarr-3", None)]);
    world.server = super::ShelfServer::Jellyfin;
    let plan = world.plan(&shelf("radarr-3", 20), &[]);
    assert_eq!(plan.actions, [withdraw("radarr-3", Withdrawn::ShelfChanged)], "never deleted off a shelf nobody can see");
}

#[test]
fn a_play_from_before_the_announcement_does_not_take_it_back() {
    let world = World::new(&[("radarr-3", Some(NOW - 40 * DAY))]);
    let plan = world.plan(&shelf("radarr-3", 20), &[]);
    assert_eq!(plan.actions, [delete("radarr-3", Route::LeavingSoon)]);
}

#[test]
fn an_item_whose_evidence_is_lost_is_held_neither_deleted_nor_taken_back() {
    let mut world = World::new(&[("radarr-4", None)]);
    world.evidence.clear();
    world.selected.clear();
    let plan = world.plan(&shelf("radarr-4", 20), &[item("radarr-5", false)]);
    assert!(plan.actions.is_empty(), "{plan:?}");
    assert_eq!(plan.held, [("radarr-4".to_string(), Held::NoEvidence), ("radarr-5".to_string(), Held::NoEvidence)]);
}

#[test]
fn an_expired_window_waits_while_the_watch_history_is_incomplete() {
    let mut world = World::new(&[("radarr-4", None)]);
    world.complete = false;
    let plan = world.plan(&shelf("radarr-4", 20), &[]);
    assert!(plan.actions.is_empty());
    assert_eq!(plan.held, [("radarr-4".to_string(), Held::EvidenceIncomplete)]);
}

#[test]
fn without_a_shelf_unwatched_items_are_held_and_finished_ones_still_go() {
    let mut world = World::new(&[("radarr-6", None), ("radarr-7", Some(NOW - DAY))]);
    world.shelf = None;
    let plan = world.plan(&NativeState::default(), &[item("radarr-6", true), item("radarr-7", false)]);
    assert_eq!(plan.actions, [delete("radarr-7", Route::Finished)]);
    assert_eq!(plan.held, [("radarr-6".to_string(), Held::NoShelf)]);
}

#[test]
fn a_cleared_shelf_title_takes_what_waits_back() {
    let mut world = World::new(&[("radarr-6", None)]);
    world.shelf = None;
    let plan = world.plan(&shelf("radarr-6", 3), &[]);
    assert_eq!(plan.actions, [withdraw("radarr-6", Withdrawn::ShelfChanged)]);
}

#[test]
fn an_item_gone_from_the_library_leaves_the_shelf() {
    let mut world = World::new(&[("radarr-8", None)]);
    world.on_disk.clear();
    assert_eq!(world.plan(&shelf("radarr-8", 3), &[]).actions, [withdraw("radarr-8", Withdrawn::Gone)]);
}

#[test]
fn the_caps_take_a_prefix_of_the_plan_and_defer_the_rest() {
    let mut world = World::new(&[("radarr-1", Some(1)), ("radarr-2", Some(1)), ("radarr-3", None)]);
    world.caps = Caps::new(1, 50);
    let plan = world.plan(&NativeState::default(), &[item("radarr-1", false), item("radarr-2", false), item("radarr-3", true)]);
    assert_eq!(plan.actions, [delete("radarr-1", Route::Finished)]);
    assert_eq!(plan.deferred, ["radarr-2", "radarr-3"]);
}

#[test]
fn deletes_per_run_bound_expired_windows_and_new_deletes_alike() {
    let mut world = World::new(&[("radarr-1", None), ("radarr-2", Some(1)), ("radarr-3", None)]);
    world.max_deletes = 1;
    let plan = world.plan(&shelf("radarr-1", 20), &[item("radarr-2", false), item("radarr-3", true)]);
    assert_eq!(plan.actions, [delete("radarr-1", Route::LeavingSoon)]);
    assert_eq!(plan.deferred, ["radarr-2", "radarr-3"], "the first that does not fit defers the rest");
}

#[test]
fn a_season_waits_while_the_one_it_must_follow_waits() {
    let mut world = World::new(&[("sonarr-1-s1", Some(1)), ("sonarr-1-s2", Some(1))]);
    world.evidence.remove("sonarr-1-s1");
    let second = Item { id: "sonarr-1-s2", bytes: GIB, announce: false, after: Some("sonarr-1-s1") };
    let plan = world.plan(&NativeState::default(), &[item("sonarr-1-s1", false), second]);
    assert!(plan.actions.is_empty(), "{plan:?}");
    assert_eq!(plan.deferred, ["sonarr-1-s2"]);
}

#[test]
fn a_pinned_item_is_never_touched() {
    let mut world = World::new(&[("radarr-9", Some(1))]);
    world.pinned = vec!["radarr-9"];
    let plan = world.plan(&NativeState::default(), &[item("radarr-9", false)]);
    assert_eq!(plan, Default::default());
}

#[rstest]
#[case::maintainerr_readable(Executor::Maintainerr, true, Acting::Maintainerr)]
#[case::maintainerr_unreadable(Executor::Maintainerr, false, Acting::Nobody)]
#[case::native_needs_no_maintainerr(Executor::Native, false, Acting::Native)]
#[case::native_with_maintainerr_still_acts_alone(Executor::Native, true, Acting::Native)]
fn the_executor_setting_chooses_who_acts(#[case] executor: Executor, #[case] readable: bool, #[case] expected: Acting) {
    assert_eq!(acting(executor, readable), expected);
}
