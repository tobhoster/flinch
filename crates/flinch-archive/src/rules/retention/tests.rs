use super::super::*;
use crate::capacity::{CapacityForecast, VolumeForecast};
use crate::plan::knapsack::Sequence;
use crate::plan::{generate_eviction_plan, PlannerConfig};
use crate::regret::Regret;
use rstest::rstest;

const GIB: u64 = 1 << 30;

/// Season `index` of a five-season show on disk, newest first ranked.
fn season(index: u32, played: bool, p_watch: f64) -> (MediaCandidate, Facts) {
    let regret = Regret::new(p_watch, 1.0, 1.0);
    let candidate = MediaCandidate {
        id: format!("sonarr-7-s{index}"),
        title: format!("Andor S{index}"),
        size_bytes: 10 * GIB,
        volume: Some("tv".to_string()),
        regret,
        reason: String::new(),
        age_days: 400.0,
        exclusion: None,
        sequence: Some(Sequence { group: "sonarr-7".to_string(), index, played }),
        handed: false,
        announce: false,
        protect: false,
        quality: crate::quality::advise(&regret, &crate::quality::Item::default()),
        eviction_safety: 0.0,
        force: None,
    };
    let facts = Facts {
        kind: Some(Kind::Season),
        season: Some(index),
        newest_rank: Some(6 - index),
        first_season: Some(index == 1),
        continuing: Some(true),
        ..Facts::default()
    };
    (candidate, facts)
}

fn rule(effect: Effect) -> Rule {
    Rule { name: "rolling".to_string(), enabled: true, scope: Scope::default(), effect }
}

fn kept(rules: &[Rule], (candidate, facts): (MediaCandidate, Facts)) -> Option<Exclusion> {
    let mut candidates = [candidate];
    let facts = HashMap::from([(candidates[0].id.clone(), facts)]);
    apply(rules, &mut candidates, &facts, 0);
    let [candidate] = candidates;
    candidate.exclusion
}

const BY_RULE: Option<&str> = Some("rolling");

#[rstest]
#[case::newest(5, BY_RULE)]
#[case::second_newest(4, BY_RULE)]
#[case::older(3, None)]
fn the_newest_seasons_of_a_continuing_show_are_kept(#[case] index: u32, #[case] by: Option<&str>) {
    let rules = [rule(Effect::KeepLatestSeasons { seasons: 2 })];
    assert_eq!(kept(&rules, season(index, true, 0.1)), by.map(|name| Exclusion::Rule(name.to_string())));
}

#[rstest]
#[case::ended(|f: &mut Facts| f.continuing = Some(false), None)]
#[case::special(|f: &mut Facts| f.season = Some(0), None)]
#[case::movie(|f: &mut Facts| f.kind = Some(Kind::Movie), None)]
#[case::status_unknown(|f: &mut Facts| f.continuing = None, BY_RULE)]
#[case::rank_unknown(|f: &mut Facts| f.newest_rank = None, BY_RULE)]
fn an_ended_show_is_free_and_a_missing_fact_keeps(#[case] blank: fn(&mut Facts), #[case] by: Option<&str>) {
    let (candidate, mut facts) = season(5, true, 0.1);
    blank(&mut facts);
    let rules = [rule(Effect::KeepLatestSeasons { seasons: 2 })];
    assert_eq!(kept(&rules, (candidate, facts)), by.map(|name| Exclusion::Rule(name.to_string())));
}

#[test]
fn the_first_season_is_kept_and_no_other() {
    let rules = [rule(Effect::KeepFirstSeason)];
    assert_eq!(kept(&rules, season(1, false, 0.1)), Some(Exclusion::Rule("rolling".to_string())));
    assert_eq!(kept(&rules, season(2, false, 0.1)), None);
    let (candidate, facts) = season(1, false, 0.1);
    assert_eq!(kept(&rules, (candidate, Facts { first_season: None, ..facts })), Some(Exclusion::Rule("rolling".to_string())));
}

fn forecast(target_gib: u64, emergency: bool) -> VolumeForecast {
    VolumeForecast {
        volume: "tv".to_string(),
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

/// What leaves a five-season show, 20 GiB needed, after `rules`; `before`
/// may exclude seasons first.
fn plan_show(played: bool, rules: &[Rule], before: fn(&mut [MediaCandidate]), emergency: bool) -> Vec<String> {
    let (mut candidates, facts): (Vec<MediaCandidate>, HashMap<String, Facts>) = (1..=5)
        .map(|index| season(index, played, 0.1 * f64::from(index)))
        .map(|(candidate, facts)| {
            let id = candidate.id.clone();
            (candidate, (id, facts))
        })
        .unzip();
    before(&mut candidates);
    apply(rules, &mut candidates, &facts, 0);
    let plan = generate_eviction_plan(&candidates, &[forecast(20, emergency)], &PlannerConfig::default()).expect("valid inputs");
    let mut ids: Vec<String> = plan.items.into_iter().map(|item| item.id).collect();
    ids.sort();
    ids
}

fn nothing(_: &mut [MediaCandidate]) {}

#[rstest]
#[case::milp(false)]
#[case::greedy(true)]
fn rolling_retention_lets_older_seasons_go_where_a_plain_keep_would_hold_the_show(#[case] emergency: bool) {
    // Unplayed seasons go from the end, so S4 and S5 kept any other way
    // anchor the chain: nothing of the show may go.
    let pin_newest = |candidates: &mut [MediaCandidate]| {
        for candidate in &mut candidates[3..] {
            candidate.exclusion = Some(Exclusion::Rule("plain".to_string()));
        }
    };
    assert!(plan_show(false, &[], pin_newest, emergency).is_empty());
    // Rolling retention keeps S4 and S5 and lets S3 then S2 go (from the end).
    let rolling = [rule(Effect::KeepLatestSeasons { seasons: 2 })];
    assert_eq!(plan_show(false, &rolling, nothing, emergency), vec!["sonarr-7-s2", "sonarr-7-s3"]);
}

#[rstest]
#[case::milp(false)]
#[case::greedy(true)]
fn a_kept_first_season_does_not_hold_the_watched_seasons_after_it(#[case] emergency: bool) {
    // Played seasons go from the start: S1 kept any other way blocks S2…S5.
    let pin_first = |candidates: &mut [MediaCandidate]| candidates[0].exclusion = Some(Exclusion::Rule("plain".to_string()));
    assert!(plan_show(true, &[], pin_first, emergency).is_empty());
    let first = [rule(Effect::KeepFirstSeason)];
    assert_eq!(plan_show(true, &first, nothing, emergency), vec!["sonarr-7-s2", "sonarr-7-s3"]);
}

#[test]
fn a_force_set_before_the_rules_stands_unless_a_rule_decides_the_item() {
    let spared = |(candidate, facts): (MediaCandidate, Facts)| (MediaCandidate { force: Some(Force::Spare), ..candidate }, facts);
    let after = |rules: &[Rule], item: (MediaCandidate, Facts)| {
        let facts = HashMap::from([(item.0.id.clone(), item.1)]);
        let mut candidates = [item.0];
        apply(rules, &mut candidates, &facts, 0);
        let [candidate] = candidates;
        (candidate.force, candidate.exclusion.is_some())
    };
    let silent = [rule(Effect::KeepLatestSeasons { seasons: 1 })];
    assert_eq!(after(&silent, spared(season(2, true, 0.1))), (Some(Force::Spare), false));
    assert_eq!(after(&silent, spared(season(5, true, 0.1))), (None, true), "a keep decides the item");
    let seasons = Scope { kind: Some(Kind::Season), ..Scope::default() };
    let must = [Rule { name: "go".to_string(), enabled: true, scope: seasons, effect: Effect::MustEvict }];
    assert_eq!(after(&must, spared(season(2, true, 0.1))), (Some(Force::Must), false), "an operator's rule outranks a soft keep");
}
