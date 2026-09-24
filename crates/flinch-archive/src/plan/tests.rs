use super::*;
use crate::golden::golden_season;
use proptest::prelude::*;
use rstest::rstest;

const GB: u64 = 1_000_000_000;

fn watched_cold_season(id: &str) -> ArchiveCard {
    let mut c = golden_season();
    c.id = id.to_string();
    c.title = id.to_string();
    c.season_state = Some(crate::card::SeasonState::Completed);
    c.last_watched_days = Some(400.0);
    c.is_newest_season = Some(false);
    c
}

fn sized(id: &str, gb: u64) -> ArchiveCard {
    let mut c = watched_cold_season(id);
    c.size_bytes = gb * GB;
    c
}

fn verdict(p_safe: f32) -> ScoreVerdict {
    ScoreVerdict { p_safe, hard_guard: false, sibling_played: false }
}

fn baseline_plan(cards: &[ArchiveCard], verdicts: &HashMap<String, ScoreVerdict>, goal: &ReclaimGoal) -> Plan {
    let policy = ArchivePolicy::default();
    build_plan(cards, &Baseline::new(policy), &policy, 0.95, verdicts, goal)
}

fn ids(plan: &Plan) -> Vec<&str> {
    plan.entries.iter().map(|e| e.id.as_str()).collect()
}

#[test]
fn the_gate_keeps_items_below_the_floor_even_when_the_policy_says_delete() {
    // A future head at 0.6 must not delete; the deterministic policy alone
    // would.
    struct Shy;
    impl ArchiveModel for Shy {
        fn delete_probability(&self, _: &ArchiveCard, _: &Reason) -> Probability {
            Probability::new(0.6).expect("in-range")
        }
    }
    let cards = vec![watched_cold_season("shy")];
    let plan = build_plan(&cards, &Shy, &ArchivePolicy::default(), 0.95, &HashMap::new(), &ReclaimGoal::AllSafe);
    assert!(plan.entries.is_empty(), "0.6 < 0.95 must keep the item");
    assert_eq!(plan.eligible_bytes, 0, "a gated item is not reserve either");
}

#[test]
fn a_confident_model_can_delete_what_the_policy_allows() {
    let cards = vec![watched_cold_season("ok"), watched_cold_season("ok2")];
    let plan = baseline_plan(&cards, &HashMap::new(), &ReclaimGoal::AllSafe);
    assert_eq!(plan.entries.len(), 2);
    assert!(plan.goal_met, "everything safe was taken");
    assert_eq!(plan.goal_bytes, None);
}

#[test]
fn armed_never_played_reclaim_actually_reaches_the_plan() {
    // The preview counted these items; the plan must be able to act on them.
    let mut card = golden_season();
    card.id = "never-played".to_string();
    card.season_state = Some(crate::card::SeasonState::Empty);
    card.is_newest_season = Some(false);
    card.last_watched_days = None;
    card.added_days_ago = 300.0;
    let mut armed = ArchivePolicy::default();
    armed.unwatched_reclaim.enabled = true;
    let verdicts: HashMap<String, ScoreVerdict> = [(card.id.clone(), verdict(0.9))].into_iter().collect();
    let plan = build_plan(&[card], &Baseline::new(armed), &armed, 0.95, &verdicts, &ReclaimGoal::AllSafe);
    assert_eq!(plan.entries.len(), 1, "a permitted never-played reclaim must be planned");
}

#[test]
fn protections_never_reach_the_plan() {
    let mut fav = watched_cold_season("fav");
    fav.is_favorite = true;
    let plan = baseline_plan(&[fav], &HashMap::new(), &ReclaimGoal::AllSafe);
    assert!(plan.entries.is_empty());
}

#[test]
fn a_byte_goal_stops_once_covered_taking_larger_items_first_at_equal_regret() {
    let cards: Vec<ArchiveCard> = (0..6).map(|i| sized(&format!("s{i}"), 2 * (i + 1))).collect();
    let plan = baseline_plan(&cards, &HashMap::new(), &ReclaimGoal::Bytes(15 * GB));
    // Baseline P = 1.0 everywhere: zero regret, so size decides — fewest deletes.
    assert_eq!(ids(&plan), ["s5", "s4"], "12 + 10 GB covers 15 GB in two deletes");
    assert!(plan.goal_met);
    assert_eq!(plan.goal_bytes, Some(15 * GB));
}

#[test]
fn a_zero_byte_goal_plans_nothing_but_still_reports_the_reserve() {
    let cards = vec![sized("a", 10), sized("b", 20)];
    let plan = baseline_plan(&cards, &HashMap::new(), &ReclaimGoal::Bytes(0));
    assert!(plan.entries.is_empty(), "zero bytes means nothing — never 'everything'");
    assert!(plan.goal_met);
    assert_eq!(plan.eligible_bytes, 30 * GB, "the reserve is still visible");
}

#[test]
fn eviction_takes_the_least_expected_regret_per_byte_first() {
    // c: certain (0 regret) · a: 1% over 10 GB · b: 10% over 50 GB (2x a's density).
    let cards = vec![sized("a", 10), sized("b", 50), sized("c", 5)];
    let verdicts: HashMap<String, ScoreVerdict> =
        [("a", 0.99), ("b", 0.90), ("c", 1.0)].into_iter().map(|(id, p)| (id.to_string(), verdict(p))).collect();
    let plan = baseline_plan(&cards, &verdicts, &ReclaimGoal::AllSafe);
    assert_eq!(ids(&plan), ["c", "a", "b"]);
}

#[rstest]
#[case::clears_the_floor(Some(0.90), true)]
#[case::below_the_floor(Some(0.70), false)]
#[case::exactly_on_the_floor(Some(0.80), true)]
#[case::no_verdict_is_judged_by_the_rules_alone(None, true)]
#[case::a_nan_score_never_clears(Some(f32::NAN), false)]
fn the_score_floor_gates_what_the_policy_permits(#[case] p_safe: Option<f32>, #[case] planned: bool) {
    let card = sized("x", 10);
    let verdicts: HashMap<String, ScoreVerdict> =
        p_safe.map(|p| (card.id.clone(), verdict(p))).into_iter().collect();
    let policy = ArchivePolicy { score_floor: 0.80, ..ArchivePolicy::default() };
    let plan = build_plan(&[card], &Baseline::new(policy), &policy, 0.95, &verdicts, &ReclaimGoal::AllSafe);
    assert_eq!(!plan.entries.is_empty(), planned);
}

fn on_volumes(pairs: &[(&str, &str)], goals: &[(&str, u64)]) -> ReclaimGoal {
    ReclaimGoal::PerVolume(VolumeGoals {
        goals: goals.iter().map(|(v, gb)| (v.to_string(), gb * GB)).collect(),
        volume_of: pairs.iter().map(|(id, v)| (id.to_string(), v.to_string())).collect(),
        handed: HashSet::new(),
    })
}

#[test]
fn per_volume_goals_evict_only_on_the_volume_that_needs_space() {
    let cards = vec![sized("m1", 20), sized("m2", 10), sized("t1", 30)];
    let goal = on_volumes(&[("m1", "/movies"), ("m2", "/movies"), ("t1", "/tv")], &[("/movies", 15)]);
    let plan = baseline_plan(&cards, &HashMap::new(), &goal);
    assert_eq!(ids(&plan), ["m1"], "the 30 GB show would free the wrong disk");
    assert!(plan.goal_met);
    let tv = plan.volumes.iter().find(|v| v.volume == "/tv").expect("reserve row for /tv");
    assert_eq!((tv.goal_bytes, tv.reclaimed_bytes, tv.eligible_bytes), (0, 0, 30 * GB));
}

#[test]
fn an_item_on_no_known_volume_is_never_evicted_and_is_not_reserve() {
    let cards = vec![sized("known", 10), sized("orphan", 90)];
    let goal = on_volumes(&[("known", "/media")], &[("/media", 50)]);
    let plan = baseline_plan(&cards, &HashMap::new(), &goal);
    assert_eq!(ids(&plan), ["known"]);
    assert!(!plan.goal_met, "10 GB cannot cover 50 GB");
    assert_eq!(plan.eligible_bytes, 10 * GB, "the orphan can never relieve /media");
}

#[test]
fn an_idle_per_volume_goal_evicts_nothing_and_reports_every_reserve() {
    let cards = vec![sized("m1", 20), sized("t1", 30)];
    let goal = on_volumes(&[("m1", "/movies"), ("t1", "/tv")], &[]);
    let plan = baseline_plan(&cards, &HashMap::new(), &goal);
    assert!(plan.entries.is_empty());
    assert!(plan.goal_met, "no volume asked for anything");
    assert_eq!(plan.volumes.len(), 2);
}

#[test]
fn an_item_already_handed_over_is_taken_before_a_cheaper_newcomer() {
    // Both cover the goal alone; the newcomer is the cheaper eviction.
    let cards = vec![sized("announced", 10), sized("newcomer", 12)];
    let verdicts = HashMap::from([("announced".to_string(), verdict(0.90)), ("newcomer".to_string(), verdict(0.99))]);
    let mut goal = on_volumes(&[("announced", "/media"), ("newcomer", "/media")], &[("/media", 10)]);
    assert_eq!(ids(&baseline_plan(&cards, &verdicts, &goal)), ["newcomer"]);

    // Once handed over it stays chosen: re-planning would restart its window.
    if let ReclaimGoal::PerVolume(goals) = &mut goal {
        goals.handed.insert("announced".to_string());
    }
    assert_eq!(ids(&baseline_plan(&cards, &verdicts, &goal)), ["announced"]);
}

/// (size in GB, calibrated P(safe), is favorite)
fn specs() -> impl Strategy<Value = Vec<(u64, f32, bool)>> {
    prop::collection::vec((1u64..=100, 0.0f32..=1.0, any::<bool>()), 0..24)
}

fn library(specs: &[(u64, f32, bool)]) -> (Vec<ArchiveCard>, HashMap<String, ScoreVerdict>) {
    let mut cards = Vec::new();
    let mut verdicts = HashMap::new();
    for (i, (gb, p, favorite)) in specs.iter().enumerate() {
        let mut card = sized(&format!("c{i:02}"), *gb);
        card.is_favorite = *favorite;
        verdicts.insert(card.id.clone(), verdict(*p));
        cards.push(card);
    }
    (cards, verdicts)
}

/// The eviction order recomputed independently of the implementation.
fn oracle_order(specs: &[(u64, f32, bool)]) -> Vec<String> {
    let mut rows: Vec<(f64, u64, String)> = specs
        .iter()
        .enumerate()
        .filter(|(_, (_, _, favorite))| !favorite)
        .map(|(i, (gb, p, _))| (f64::from(1.0 - p) / (gb * GB) as f64, gb * GB, format!("c{i:02}")))
        .collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    rows.into_iter().map(|(_, _, id)| id).collect()
}

proptest! {
    #[test]
    fn a_byte_goal_evicts_a_minimal_prefix_of_the_regret_order(specs in specs(), goal_gb in 0u64..=600) {
        let (cards, verdicts) = library(&specs);
        let goal = goal_gb * GB;
        let plan = baseline_plan(&cards, &verdicts, &ReclaimGoal::Bytes(goal));
        let order = oracle_order(&specs);

        prop_assert_eq!(plan.reclaimed_bytes, plan.entries.iter().map(|e| e.size_bytes).sum::<u64>());
        prop_assert_eq!(ids(&plan), order.iter().take(plan.entries.len()).map(String::as_str).collect::<Vec<_>>());
        prop_assert_eq!(plan.goal_met, plan.reclaimed_bytes >= goal);
        if let Some(last) = plan.entries.last() {
            prop_assert!(plan.reclaimed_bytes - last.size_bytes < goal, "every eviction was needed");
        }
        if !plan.goal_met {
            prop_assert_eq!(plan.reclaimed_bytes, plan.eligible_bytes, "a missed goal took the whole reserve");
        }
    }

    #[test]
    fn everything_safe_takes_the_whole_reserve_and_no_protection(specs in specs()) {
        let (cards, verdicts) = library(&specs);
        let plan = baseline_plan(&cards, &verdicts, &ReclaimGoal::AllSafe);
        let order = oracle_order(&specs);
        prop_assert_eq!(ids(&plan), order.iter().map(String::as_str).collect::<Vec<_>>());
        prop_assert_eq!(plan.reclaimed_bytes, plan.eligible_bytes);
    }

    #[test]
    fn per_volume_goals_never_cross_volumes_and_never_overshoot(
        specs in specs(),
        placement in prop::collection::vec(0usize..3, 24),
        goals_gb in prop::collection::vec(0u64..=200, 3),
    ) {
        let (cards, verdicts) = library(&specs);
        let volume = |i: usize| format!("/v{}", placement[i]);
        let goal = ReclaimGoal::PerVolume(VolumeGoals {
            goals: goals_gb.iter().enumerate().map(|(v, gb)| (format!("/v{v}"), gb * GB)).collect(),
            volume_of: cards.iter().enumerate().map(|(i, c)| (c.id.clone(), volume(i))).collect(),
            handed: HashSet::new(),
        });
        let plan = baseline_plan(&cards, &verdicts, &goal);

        for row in &plan.volumes {
            let taken: Vec<&PlanEntry> = plan.entries.iter()
                .filter(|e| cards.iter().position(|c| c.id == e.id).map(volume).as_deref() == Some(row.volume.as_str()))
                .collect();
            let reclaimed: u64 = taken.iter().map(|e| e.size_bytes).sum();
            prop_assert_eq!(reclaimed, row.reclaimed_bytes);
            if let Some(last) = taken.last() {
                prop_assert!(reclaimed - last.size_bytes < row.goal_bytes, "{} overshot", row.volume);
            }
            if row.reclaimed_bytes < row.goal_bytes {
                prop_assert_eq!(row.reclaimed_bytes, row.eligible_bytes, "{} left reserve unused", row.volume);
            }
        }
        prop_assert_eq!(plan.goal_met, plan.volumes.iter().all(|v| v.reclaimed_bytes >= v.goal_bytes));
    }
}
