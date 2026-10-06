use super::*;
use rstest::rstest;

const GIB: u64 = 1 << 30;

fn action(p_watch: f64, friction: f64, household: f64, partway: bool, gib: u64) -> QualityAction {
    advise(&Regret::new(p_watch, friction, household), partway, gib * GIB).action
}

#[rstest]
#[case::partway_beats_everything(0.05, 1.0, 1.0, true, 40, QualityAction::KeepOriginal { reason: KeepReason::ActiveProgress })]
#[case::likely_watched(0.60, 1.0, 1.0, false, 40, QualityAction::KeepOriginal { reason: KeepReason::HighWatchLikelihood })]
#[case::costly_to_lose(0.30, 2.0, 2.5, false, 40, QualityAction::KeepOriginal { reason: KeepReason::HouseholdPriority })]
#[case::moderate_and_large(0.30, 1.0, 1.0, false, 40, QualityAction::DowngradeQuality { estimated_reclaim_bytes: 32 * GIB })]
#[case::moderate_but_small(0.30, 1.0, 1.0, false, 4, QualityAction::EligibleForEviction)]
#[case::cold_but_hard_to_replace(0.05, 6.0, 1.0, false, 4, QualityAction::KeepOriginal { reason: KeepReason::HighReacquisitionFriction })]
#[case::cold_and_easy(0.05, 1.2, 1.0, false, 40, QualityAction::EligibleForEviction)]
fn advice_follows_demand_regret_and_friction(
    #[case] p_watch: f64,
    #[case] friction: f64,
    #[case] household: f64,
    #[case] partway: bool,
    #[case] gib: u64,
    #[case] expected: QualityAction,
) {
    assert_eq!(action(p_watch, friction, household, partway, gib), expected);
}

#[test]
fn safety_never_exceeds_the_chance_nobody_watches() {
    for p_watch in [0.0, 0.38, 0.9, 1.0] {
        for friction in [0.1, 1.0, 3.0, 8.0] {
            let safety = eviction_safety(&Regret::new(p_watch, friction, 1.0));
            assert!((0.0..=1.0 - p_watch + 1e-12).contains(&safety), "p {p_watch} friction {friction}: {safety}");
        }
    }
    assert!(eviction_safety(&Regret::new(0.38, 1.0, 1.0)) <= 0.62);
    assert!(eviction_safety(&Regret::new(0.1, 5.0, 1.0)) < eviction_safety(&Regret::new(0.1, 1.0, 1.0)), "harder to replace, less safe");
}
