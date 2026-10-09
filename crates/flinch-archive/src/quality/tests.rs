use super::*;
use rstest::rstest;

const GIB: u64 = 1 << 30;

fn action(p_watch: f64, friction: f64, household: f64, partway: bool, gib: u64) -> QualityAction {
    let item = Item { size_bytes: gib * GIB, partway, ..Item::default() };
    advise(&Regret::new(p_watch, friction, household), &item).action
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

const DOWNGRADE_40: QualityAction = QualityAction::DowngradeQuality { estimated_reclaim_bytes: 32 * GIB };

/// An item in a seldom-played theme: P(watch), friction, household weight,
/// (partway, pinned, 2160p), GiB.
#[rstest]
#[case::partway_still_keeps(0.05, 6.0, 1.0, (true, false, false), 40, QualityAction::KeepOriginal { reason: KeepReason::ActiveProgress })]
#[case::likely_watched_still_keeps(0.60, 6.0, 1.0, (false, false, false), 40, QualityAction::KeepOriginal { reason: KeepReason::HighWatchLikelihood })]
#[case::costly_still_keeps(0.30, 6.0, 2.5, (false, false, false), 40, QualityAction::KeepOriginal { reason: KeepReason::HouseholdPriority })]
#[case::hard_to_replace_is_downgraded(0.05, 6.0, 1.0, (false, false, false), 40, DOWNGRADE_40)]
#[case::uhd_under_the_size_bar(0.30, 1.0, 1.0, (false, false, true), 10, QualityAction::DowngradeQuality { estimated_reclaim_bytes: 8 * GIB })]
#[case::pinned_is_left_alone(0.05, 6.0, 1.0, (false, true, false), 40, QualityAction::KeepOriginal { reason: KeepReason::HighReacquisitionFriction })]
#[case::not_large_enough(0.05, 6.0, 1.0, (false, false, false), 20, QualityAction::KeepOriginal { reason: KeepReason::HighReacquisitionFriction })]
#[case::eviction_advice_stands(0.05, 1.2, 1.0, (false, false, true), 40, QualityAction::EligibleForEviction)]
fn a_cold_theme_suggests_a_downgrade_after_every_keep_rule(
    #[case] p_watch: f64,
    #[case] friction: f64,
    #[case] household: f64,
    #[case] (partway, pinned, uhd): (bool, bool, bool),
    #[case] gib: u64,
    #[case] expected: QualityAction,
) {
    let theme = ColdTheme { name: "Documentary · History".to_string(), played_share: 0.04 };
    let item = Item { size_bytes: gib * GIB, partway, pinned, uhd, cold_theme: Some(&theme) };
    assert_eq!(advise(&Regret::new(p_watch, friction, household), &item).action, expected);
}

#[test]
fn a_cold_theme_downgrade_names_the_theme_and_never_moves_regret() {
    let theme = ColdTheme { name: "Documentary · History".to_string(), played_share: 0.04 };
    let regret = Regret::new(0.05, 6.0, 1.0);
    let plain = advise(&regret, &Item { size_bytes: 40 * GIB, ..Item::default() });
    let themed = advise(&regret, &Item { size_bytes: 40 * GIB, cold_theme: Some(&theme), ..Item::default() });
    assert!(themed.explanation.contains("\"Documentary · History\" is seldom played (4%"), "{}", themed.explanation);
    assert_eq!(themed.marginal_regret_per_gb, plain.marginal_regret_per_gb);
}

#[test]
fn moderate_demand_keeps_its_own_reason_in_a_cold_theme() {
    let theme = ColdTheme { name: "Western".to_string(), played_share: 0.0 };
    let advice = advise(&Regret::new(0.30, 1.0, 1.0), &Item { size_bytes: 40 * GIB, cold_theme: Some(&theme), ..Item::default() });
    assert!(advice.explanation.starts_with("Moderate demand"), "{}", advice.explanation);
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
