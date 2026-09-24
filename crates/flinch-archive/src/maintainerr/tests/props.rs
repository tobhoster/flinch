//! Invariants of the planner and executor over generated libraries: mixes of
//! movies and seasons, resolved or not, kept, evicted, neither or gone from
//! the library and Plex, with operator exclusions, FLINCH-owned exclusions or
//! memberships, and members added by operator rules.

use super::super::{
    execute, observe, plan_sync, Caps, Desired, Observed, Outcome, OwnedState, ProtectedEntry, ScheduledEntry,
    SyncAction, SyncItem, SyncPlan,
};
use super::fake::Fake;
use super::{current, item, movie, movie_ids, row, season, season_ids, titles, valid_collections, GIB, MOVIES, SEASONS};
use crate::card::LibraryKind;
use proptest::prelude::*;
use std::collections::BTreeSet;
use std::future::Future;

#[derive(Debug, Clone, Copy)]
enum Decision {
    Protect,
    Evict,
    Neither,
}

#[derive(Debug, Clone, Copy)]
enum Owned {
    Nothing,
    Exclusion,
    Membership,
}

#[derive(Debug, Clone)]
struct Card {
    season: bool,
    resolved: bool,
    decision: Decision,
    gib: u64,
    /// The operator excluded it. Maintainerr reuses a global row, so the
    /// operator and FLINCH never both hold one for the same key.
    operator_row: bool,
    owned: Owned,
    rule_member: bool,
    /// Left the library and Plex: not a card any more, only owned state.
    gone: bool,
}

fn card() -> impl Strategy<Value = Card> {
    let decision = prop_oneof![Just(Decision::Protect), Just(Decision::Evict), Just(Decision::Neither)];
    let owned = prop_oneof![Just(Owned::Nothing), Just(Owned::Exclusion), Just(Owned::Membership)];
    (
        any::<bool>(),
        prop::bool::weighted(0.8),
        decision,
        1u64..40,
        prop::bool::weighted(0.25),
        owned,
        prop::bool::weighted(0.2),
        prop::bool::weighted(0.3),
    )
        .prop_map(|(season, resolved, decision, gib, operator_row, owned, rule_member, gone)| Card {
            season,
            resolved,
            decision,
            gib,
            operator_row: operator_row && !matches!(owned, Owned::Exclusion),
            owned,
            rule_member,
            gone: gone && matches!(decision, Decision::Neither),
        })
}

struct World {
    fake: Fake,
    owned: OwnedState,
    all: Vec<SyncItem>,
    desired: Desired,
    operator_rows: BTreeSet<i64>,
}

fn world(cards: &[Card]) -> World {
    let mut fake = Fake::new(current(), valid_collections());
    let mut owned = OwnedState::default();
    let mut desired = Desired {
        protect: Vec::new(),
        evict: Vec::new(),
        announced: Default::default(),
        collections: titles(),
        gone: BTreeSet::new(),
        seerr_configured: false,
    };
    let (mut all, mut operator_rows) = (Vec::new(), BTreeSet::new());
    for (index, spec) in cards.iter().enumerate() {
        let id = format!("card-{index}");
        let key = 1000 + 10 * index as i64;
        let (show, own) = (key.to_string(), (key + 1).to_string());
        let (kind, target, plex, collection) = if spec.season {
            (LibraryKind::Season, season(&show, &own), season_ids(&show, &own), SEASONS)
        } else {
            (LibraryKind::Movie, movie(&show), movie_ids(&show), MOVIES)
        };
        if spec.operator_row {
            fake.rows.push(row(500 + index as i64, target.item_key(), target.media_id()));
            operator_rows.insert(500 + index as i64);
        }
        match spec.owned {
            Owned::Nothing => {}
            Owned::Exclusion => {
                fake.rows.push(row(700 + index as i64, target.item_key(), target.media_id()));
                let entry = ProtectedEntry { target: target.clone(), exclusion_ids: vec![700 + index as i64] };
                owned.protected.insert(id.clone(), entry);
            }
            Owned::Membership => {
                fake.members.entry(collection).or_default().insert(target.item_key().to_string());
                owned.scheduled.insert(id.clone(), ScheduledEntry { target: target.clone(), collection_id: collection, added_at: 0 });
            }
        }
        if spec.rule_member {
            fake.members.entry(collection).or_default().insert(target.item_key().to_string());
        }
        if spec.gone {
            desired.gone.insert(id);
            continue;
        }
        let card = item(&id, kind, spec.resolved.then_some(plex), spec.gib * GIB);
        match spec.decision {
            Decision::Protect => desired.protect.push(card.clone()),
            Decision::Evict => desired.evict.push(card.clone()),
            Decision::Neither => {}
        }
        all.push(card);
    }
    World { fake, owned, all, desired, operator_rows }
}

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().build().expect("a current-thread runtime").block_on(future)
}

fn plan(world: &mut World, caps: &Caps) -> (Observed, SyncPlan) {
    let observed = block_on(observe(&mut world.fake, &world.all, &world.desired.collections, &world.owned))
        .expect("the fake always answers");
    let plan = plan_sync(&world.desired, &observed, &world.owned, caps);
    (observed, plan)
}

fn cards_with(plan: &SyncPlan, pick: fn(&SyncAction) -> bool) -> BTreeSet<&str> {
    plan.actions.iter().filter(|action| pick(action)).map(SyncAction::card_id).collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn a_plan_respects_ownership_caps_and_resolution(
        cards in prop::collection::vec(card(), 0..10),
        max_items in 0usize..5,
        max_gib in 0u64..80,
    ) {
        let mut world = world(&cards);
        let caps = Caps::new(max_items, max_gib);
        let (_, plan) = plan(&mut world, &caps);

        for action in &plan.actions {
            if let SyncAction::RemoveExclusion { card_id, exclusion_id, .. } = action {
                let owned = world.owned.protected.get(card_id).map(|entry| &entry.exclusion_ids);
                prop_assert!(owned.is_some_and(|ids| ids.contains(exclusion_id)), "removes a row FLINCH does not own: {action}");
                prop_assert!(!world.operator_rows.contains(exclusion_id));
            }
        }

        let adds: Vec<u64> = plan.actions.iter().filter_map(|a| match a {
            SyncAction::Schedule { bytes, .. } => Some(*bytes),
            _ => None,
        }).collect();
        prop_assert!(adds.len() <= max_items);
        // Over the byte cap only as one oversized first add; a cap of 0 hands nothing.
        prop_assert!(adds.iter().sum::<u64>() <= caps.max_bytes || (adds.len() == 1 && caps.max_bytes > 0));
        // Liveness: open caps never defer everything.
        let open = caps.max_items > 0 && caps.max_bytes > 0;
        prop_assert!(!(open && adds.is_empty() && !plan.deferred.is_empty()), "open caps deferred everything: {:?}", plan.deferred);

        let protected = cards_with(&plan, |a| matches!(a, SyncAction::Protect { .. }));
        let scheduled = cards_with(&plan, |a| matches!(a, SyncAction::Schedule { .. }));
        prop_assert!(protected.is_disjoint(&scheduled));

        let unresolved: BTreeSet<&str> = world.all.iter().filter(|i| i.plex.is_none()).map(|i| i.card_id.as_str()).collect();
        let acting = cards_with(&plan, |a| !matches!(a, SyncAction::Unschedule { .. }));
        prop_assert!(acting.is_disjoint(&unresolved), "an unresolved card is protected or scheduled");
    }

    #[test]
    fn executing_a_plan_reaches_a_fixed_point(
        cards in prop::collection::vec(card(), 0..10),
        max_items in 0usize..5,
        max_gib in 0u64..80,
    ) {
        let mut world = world(&cards);
        let caps = Caps::new(max_items, max_gib);
        let (observed, first) = plan(&mut world, &caps);
        let deferred: BTreeSet<String> = first.deferred.iter().cloned().collect();
        let report = block_on(execute(&mut world.fake, first, &observed, &mut world.owned, 1));
        prop_assert!(report.outcomes.iter().all(|o| *o == Outcome::Done), "{:?}", report.outcomes);

        let rows: BTreeSet<i64> = world.fake.rows.iter().map(|r| r.id).collect();
        prop_assert!(world.operator_rows.is_subset(&rows), "an operator row was removed");
        let both = world.owned.protected.keys().filter(|id| world.owned.scheduled.contains_key(*id)).count();
        prop_assert_eq!(both, 0, "a card is both protected and scheduled");

        let (_, second) = plan(&mut world, &caps);
        let acted: BTreeSet<String> = second.actions.iter().map(|a| a.card_id().to_string()).collect();
        prop_assert!(acted.is_subset(&deferred), "re-planning acts beyond the cap-deferred cards: {:?}", second.actions);
    }
}
