use super::*;
use crate::golden::golden_movie;
use proptest::prelude::*;
use rstest::rstest;

fn score(p_safe: f32, hard_guard: Option<&'static str>) -> ReclaimScore {
    ReclaimScore { p_safe, forecast: p_safe, raw_logit: 0.0, signals: Vec::new(), hard_guard }
}

#[rstest]
#[case::low_value(0.90, Some(Tier::Compact))]
#[case::exactly_on_the_compact_floor(COMPACT_FLOOR, Some(Tier::Compact))]
#[case::household_comes_back(0.20, Some(Tier::Premium))]
#[case::undecided_middle(0.60, None)]
#[case::nan_score_says_nothing(f32::NAN, None)]
fn advice_follows_the_calibrated_score(#[case] p_safe: f32, #[case] tier: Option<Tier>) {
    let advice = advise(&golden_movie(), &score(p_safe, None));
    assert_eq!(advice.map(|a| a.tier), tier);
}

#[test]
fn a_guard_always_keeps_premium_even_at_a_certain_score() {
    let advice = advise(&golden_movie(), &score(0.99, Some("newest-season"))).expect("guarded advice");
    assert_eq!(advice.tier, Tier::Premium);
    assert!(advice.reason.contains("newest-season"));
}

#[rstest]
#[case::favorite(true, false)]
#[case::keep_collection(false, true)]
fn operator_keeps_are_premium_whatever_the_score(#[case] favorite: bool, #[case] keep_collection: bool) {
    let mut card = golden_movie();
    card.is_favorite = favorite;
    card.in_keep_collection = keep_collection;
    assert_eq!(advise(&card, &score(0.99, None)).map(|a| a.tier), Some(Tier::Premium));
}

#[rstest]
#[case::nothing_advised(vec![], None)]
#[case::only_undecided(vec![None, None], None)]
#[case::all_compact(vec![Some(Tier::Compact), Some(Tier::Compact)], Some(Tier::Compact))]
#[case::one_valued_season_keeps_the_show(vec![Some(Tier::Compact), Some(Tier::Premium)], Some(Tier::Premium))]
#[case::undecided_seasons_do_not_block_compact(vec![None, Some(Tier::Compact)], Some(Tier::Compact))]
fn a_show_rolls_up_premium_first(#[case] seasons: Vec<Option<Tier>>, #[case] expected: Option<Tier>) {
    assert_eq!(roll_up(seasons), expected);
}

#[test]
fn profiles_need_both_tiers_named() {
    let named = |premium: &str, compact: &str| InflowProfiles { premium: premium.to_string(), compact: compact.to_string() };
    assert!(named("WEB-2160p", "WEB-1080p").configured());
    assert!(!named("WEB-2160p", " ").configured());
    assert!(!named("", "WEB-1080p").configured());
}

fn tier() -> impl Strategy<Value = Option<Tier>> {
    prop_oneof![Just(None), Just(Some(Tier::Premium)), Just(Some(Tier::Compact))]
}

proptest! {
    #[test]
    fn a_guarded_item_is_never_advised_compact(p_safe in 0.0f32..=1.0) {
        let advice = advise(&golden_movie(), &score(p_safe, Some("favorite")));
        prop_assert_eq!(advice.map(|a| a.tier), Some(Tier::Premium));
    }

    #[test]
    fn roll_up_is_premium_exactly_when_any_season_is(seasons in prop::collection::vec(tier(), 0..8)) {
        let any_premium = seasons.contains(&Some(Tier::Premium));
        let any_compact = seasons.contains(&Some(Tier::Compact));
        let rolled = roll_up(seasons);
        prop_assert_eq!(rolled == Some(Tier::Premium), any_premium);
        prop_assert_eq!(rolled.is_none(), !any_premium && !any_compact);
    }
}
