//! End to end over the checked-in `fixtures/arr/cards.json`: the engine the
//! offline CLI and the daemon share, from cards to a plan.

use flinch_archive::capacity::{CapacityConfig, SlidingWindowCapacityForecaster, VolumeForecast, VolumeLoad};
use flinch_archive::card::ArchiveCard;
use flinch_archive::plan::{candidates, generate_eviction_plan, EvictionPlan, Exclusion, PlannerConfig};

const CARDS: &str = include_str!("../../../fixtures/arr/cards.json");
const GIB: u64 = 1 << 30;
const NOW: u64 = 2_000_000_000;

fn plan_for(used_gib: u64, never_played: bool) -> (Vec<ArchiveCard>, EvictionPlan) {
    let cards: Vec<ArchiveCard> = serde_json::from_str(CARDS).expect("fixture parses");
    let forecaster = SlidingWindowCapacityForecaster::new(CapacityConfig { headroom_buffer_bytes: 0, ..CapacityConfig::default() })
        .expect("default config");
    let load = VolumeLoad { total_bytes: 100 * GIB, used_bytes: used_gib * GIB, daily_ingest: &[], queue_bytes: 0, in_flight_bytes: 0 };
    let forecasts = [VolumeForecast { volume: "disk".to_string(), forecast: forecaster.forecast(&load).expect("measured") }];
    let config = PlannerConfig::default();
    let candidates = candidates::offline(&cards, "disk", (!never_played).then_some(Exclusion::NeverPlayedOff), NOW, &config);
    (cards, generate_eviction_plan(&candidates, &forecasts, &config).expect("plans"))
}

#[test]
fn a_disk_under_its_target_plans_nothing() {
    let (_, plan) = plan_for(60, true);
    assert_eq!(plan.method, None);
    assert!(plan.items.is_empty());
}

#[test]
fn a_full_disk_frees_its_target_without_touching_a_pinned_or_young_item() {
    let (cards, plan) = plan_for(90, true);
    assert!(plan.covered(), "10 GiB over an 80% target; the fixture holds more than that eligible");
    assert!(plan.total_reclaimed_bytes >= 10 * GIB);
    for item in &plan.items {
        let card = cards.iter().find(|card| card.id == item.id).expect("planned item is a card");
        assert!(!card.is_favorite && !card.in_keep_collection, "{} is pinned", card.id);
        assert!(card.added_days_ago >= 30.0, "{} is in its grace period", card.id);
    }
}

#[test]
fn never_played_items_stay_unless_the_operator_enables_them() {
    let (cards, plan) = plan_for(99, false);
    let never_played =
        |id: &str| cards.iter().any(|card| card.id == id && card.last_watched_days.is_none() && card.is_watched != Some(true));
    assert!(plan.items.iter().all(|item| !never_played(&item.id)));
}
