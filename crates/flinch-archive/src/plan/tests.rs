use super::*;
use crate::capacity::CapacityForecast;
use proptest::prelude::*;

const GIB: u64 = 1 << 30;

fn candidate(id: &str, volume: &str, gib: u64, regret: f64) -> MediaCandidate {
    MediaCandidate {
        id: id.to_string(),
        title: id.to_string(),
        size_bytes: gib * GIB,
        volume: Some(volume.to_string()),
        regret: Regret::new(regret, 1.0, 1.0),
        reason: format!("{id} reason"),
        age_days: 400.0,
        exclusion: None,
        sequence: None,
        handed: false,
        announce: false,
        protect: false,
        quality: crate::quality::advise(&Regret::new(regret, 1.0, 1.0), false, 0),
        eviction_safety: 0.0,
    }
}

fn season(show: &str, index: u32, played: bool, regret: f64) -> MediaCandidate {
    MediaCandidate {
        sequence: Some(Sequence { group: show.to_string(), index, played }),
        ..candidate(&format!("{show}-s{index}"), "tv", 10, regret)
    }
}

fn forecast(volume: &str, target_gib: u64, emergency: bool) -> VolumeForecast {
    VolumeForecast {
        volume: volume.to_string(),
        forecast: CapacityForecast {
            current_used_bytes: 0,
            max_capacity_bytes: 1,
            current_utilization: 0.0,
            daily_ingest_rate_bytes: 0,
            queue_bytes: 0,
            in_flight_bytes: 0,
            projected_used_bytes: 0,
            target_reclaim_bytes: target_gib * GIB,
            is_emergency: emergency,
        },
    }
}

fn ids(plan: &EvictionPlan) -> Vec<&str> {
    plan.items.iter().map(|item| item.id.as_str()).collect()
}

fn plan(candidates: &[MediaCandidate], forecasts: &[VolumeForecast]) -> EvictionPlan {
    generate_eviction_plan(candidates, forecasts, &PlannerConfig::default()).expect("valid inputs")
}

#[test]
fn a_healthy_forecast_plans_nothing_and_never_runs_the_solver() {
    let library = [candidate("a", "movies", 10, 0.0)];
    let healthy = plan(&library, &[forecast("movies", 0, false)]);
    assert_eq!(healthy.method, None);
    assert!(healthy.items.is_empty());
    assert_eq!(healthy.kept["a"], Kept::Healthy);
    assert_eq!(healthy.eligible_bytes, 10 * GIB, "the reserve is still reported");
}

#[test]
fn the_plan_covers_the_target_at_least_regret_and_says_why_the_rest_stays() {
    let library = [candidate("cheap", "movies", 10, 0.1), candidate("dear", "movies", 10, 5.0)];
    let result = plan(&library, &[forecast("movies", 10, false)]);
    assert_eq!(result.method, Some(Method::Milp));
    assert_eq!(ids(&result), ["cheap"]);
    assert_eq!(result.kept["dear"], Kept::NotNeeded);
    assert!(result.covered());
    assert_eq!((result.total_reclaimed_bytes, result.total_regret), (10 * GIB, 0.1));
}

#[test]
fn every_exclusion_keeps_its_item_whatever_its_regret() {
    let young = MediaCandidate { age_days: 3.0, ..candidate("young", "movies", 10, 0.0) };
    let homeless = MediaCandidate { volume: None, ..candidate("homeless", "movies", 10, 0.0) };
    let pinned = MediaCandidate { exclusion: Some(Exclusion::Pinned(Pin::Favorite)), ..candidate("pinned", "movies", 10, 0.0) };
    let unknown = MediaCandidate { exclusion: Some(Exclusion::NoWatchEvidence), ..candidate("unknown", "movies", 10, 0.0) };
    let fallback = candidate("fallback", "movies", 10, 9.0);
    let result = plan(&[young, homeless, pinned, unknown, fallback], &[forecast("movies", 100, false)]);
    assert_eq!(ids(&result), ["fallback"], "only the selectable item, even at the highest regret");
    assert!(!result.covered(), "a target beyond the eligible set is reported short, not forced");
    assert_eq!(result.kept["young"], Kept::Excluded(Exclusion::Grace { days: 30 }));
    assert_eq!(result.kept["homeless"], Kept::Excluded(Exclusion::NoGovernedDisk));
    assert_eq!(result.kept["pinned"].to_string(), "Pinned: favorite");
}

#[test]
fn an_emergency_forecast_takes_the_greedy_path() {
    let library = [candidate("a", "movies", 10, 1.0)];
    assert_eq!(plan(&library, &[forecast("movies", 5, true)]).method, Some(Method::Emergency));
}

#[test]
fn an_unplayed_show_is_never_left_without_its_beginning() {
    // S1 is the cheapest by far, but taking it alone would orphan S2 and S3.
    let show = [season("andor", 1, false, 0.01), season("andor", 2, false, 3.0), season("andor", 3, false, 3.0)];
    let result = plan(&show, &[forecast("tv", 25, false)]);
    assert_eq!(ids(&result), ["andor-s3", "andor-s2", "andor-s1"], "from the end, S3 first");
    assert_eq!(result.items[1].after.as_deref(), Some("andor-s3"));
}

#[test]
fn a_regret_that_is_not_a_number_is_refused_by_id() {
    let library = [candidate("nan", "movies", 1, f64::NAN)];
    assert_eq!(
        generate_eviction_plan(&library, &[forecast("movies", 1, false)], &PlannerConfig::default()),
        Err(PlanError::InvalidRegret { id: "nan".to_string() })
    );
}

#[test]
fn user_weights_match_names_case_insensitively_and_default_to_one() {
    let config = PlannerConfig { user_weights: BTreeMap::from([("Ann".to_string(), 0.5)]), ..PlannerConfig::default() };
    assert_eq!((config.weight("ann"), config.weight("bo")), (0.5, 1.0));
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// The per-run caps take a prefix of the items: no prefix may hand over a
    /// season before the one it must follow.
    #[test]
    fn every_prefix_of_the_order_respects_precedence(
        seasons in prop::collection::vec((0u32..3, 1u32..6, any::<bool>(), 0u32..500), 1..30),
        target in 1u64..300,
        emergency in any::<bool>(),
    ) {
        let library: Vec<MediaCandidate> = seasons
            .iter()
            .enumerate()
            .map(|(n, (show, index, played, regret))| MediaCandidate {
                id: format!("{n}"),
                ..season(&format!("show{show}"), *index, *played, f64::from(*regret) / 100.0)
            })
            .collect();
        let result = plan(&library, &[forecast("tv", target, emergency)]);
        let mut seen = std::collections::HashSet::new();
        for item in &result.items {
            if let Some(after) = &item.after {
                prop_assert!(seen.contains(after.as_str()), "{} before {}", item.id, after);
            }
            seen.insert(item.id.as_str());
        }
    }
}
