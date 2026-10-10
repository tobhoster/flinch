use super::preview::{preview, PlanInputs};
use super::*;
use crate::capacity::{CapacityForecast, VolumeForecast};
use crate::plan::{generate_eviction_plan, EvictionPlan, Kept, Pin, PlannerConfig};
use crate::regret::Regret;
use rstest::rstest;

const GIB: u64 = 1 << 30;
const DAY: u64 = 86_400;
const NOW: u64 = 1_800_000_000;

fn candidate(id: &str, gib: u64, p_watch: f64) -> MediaCandidate {
    let regret = Regret::new(p_watch, 1.0, 1.0);
    MediaCandidate {
        id: id.to_string(),
        title: id.to_string(),
        size_bytes: gib * GIB,
        volume: Some("movies".to_string()),
        regret,
        reason: String::new(),
        age_days: 400.0,
        exclusion: None,
        sequence: None,
        handed: false,
        announce: false,
        protect: false,
        quality: crate::quality::advise(&regret, &crate::quality::Item::default()),
        eviction_safety: 0.0,
        force: None,
    }
}

/// A 20 GiB 4K horror film Ann asked for ten days ago, last played 100 days ago.
fn alien() -> (MediaCandidate, Facts) {
    let facts = Facts {
        kind: Some(Kind::Movie),
        path: Some("/data/movies/Alien (1979)".to_string()),
        tags: Some(vec!["kids".to_string()]),
        plex_section: Some(1),
        requests: Some(vec![RequestFact { requester: "Ann".to_string(), at: Some(NOW - 10 * DAY) }]),
        theme: Some("Space Horror".to_string()),
        genres: Some(vec!["Horror".to_string(), "Science Fiction".to_string()]),
        quality: Some("Bluray-2160p".to_string()),
        played: Some(true),
        last_played_days: Some(100.0),
        ..Facts::default()
    };
    (candidate("alien", 20, 0.05), facts)
}

fn rule(name: &str, scope: Scope, effect: Effect) -> Rule {
    Rule { name: name.to_string(), enabled: true, scope, effect }
}

fn scoped(set: fn(&mut Scope)) -> Scope {
    let mut scope = Scope::default();
    set(&mut scope);
    scope
}

fn tagged(tag: &str) -> Scope {
    Scope { tags: vec![tag.to_string()], ..Scope::default() }
}

/// The candidate after `rules`, and what they reported.
fn decide(rules: &[Rule], (candidate, facts): (MediaCandidate, Facts)) -> (MediaCandidate, Outcome) {
    let mut candidates = [candidate];
    let facts = HashMap::from([(candidates[0].id.clone(), facts)]);
    let outcome = apply(rules, &mut candidates, &facts, NOW);
    let [candidate] = candidates;
    (candidate, outcome)
}

fn kept_by(name: &str) -> Option<Exclusion> {
    Some(Exclusion::Rule(name.to_string()))
}

type Blank = fn(&mut MediaCandidate, &mut Facts);

#[rstest]
#[case::kind(|s: &mut Scope| s.kind = Some(Kind::Movie), |s: &mut Scope| s.kind = Some(Kind::Season), |_: &mut MediaCandidate, f: &mut Facts| f.kind = None)]
#[case::root_folder(|s: &mut Scope| s.root_folders = vec!["/data/movies/".into()], |s: &mut Scope| s.root_folders = vec!["/data/mov".into()], |_: &mut MediaCandidate, f: &mut Facts| f.path = None)]
#[case::disk(|s: &mut Scope| s.disks = vec!["movies".into()], |s: &mut Scope| s.disks = vec!["tv".into()], |c: &mut MediaCandidate, _: &mut Facts| c.volume = None)]
#[case::tag(|s: &mut Scope| s.tags = vec!["KIDS".into()], |s: &mut Scope| s.tags = vec!["adults".into()], |_: &mut MediaCandidate, f: &mut Facts| f.tags = None)]
#[case::plex_section(|s: &mut Scope| s.plex_sections = vec![1], |s: &mut Scope| s.plex_sections = vec![2], |_: &mut MediaCandidate, f: &mut Facts| f.plex_section = None)]
#[case::requester(|s: &mut Scope| s.requesters = vec!["ann".into()], |s: &mut Scope| s.requesters = vec!["Bo".into()], |_: &mut MediaCandidate, f: &mut Facts| f.requests = None)]
#[case::theme(|s: &mut Scope| s.themes = vec!["space horror".into()], |s: &mut Scope| s.themes = vec!["Westerns".into()], |_: &mut MediaCandidate, f: &mut Facts| f.theme = None)]
#[case::genre(|s: &mut Scope| s.genres = vec!["horror".into()], |s: &mut Scope| s.genres = vec!["Comedy".into()], |_: &mut MediaCandidate, f: &mut Facts| f.genres = Some(Vec::new()))]
#[case::quality(|s: &mut Scope| s.qualities = vec!["2160p".into()], |s: &mut Scope| s.qualities = vec!["1080p".into()], |_: &mut MediaCandidate, f: &mut Facts| f.quality = None)]
#[case::played(|s: &mut Scope| s.played = Some(true), |s: &mut Scope| s.played = Some(false), |_: &mut MediaCandidate, f: &mut Facts| f.played = None)]
#[case::last_played(|s: &mut Scope| s.last_played_days.min = Some(90.0), |s: &mut Scope| s.last_played_days.max = Some(30.0), |_: &mut MediaCandidate, f: &mut Facts| f.last_played_days = None)]
fn each_scope_condition_matches_misses_and_only_keeps_when_its_fact_is_missing(
    #[case] hit: fn(&mut Scope),
    #[case] miss: fn(&mut Scope),
    #[case] blank: Blank,
) {
    let keep = |set| [rule("r", scoped(set), Effect::Keep)];
    let evict = |set| [rule("r", scoped(set), Effect::MustEvict)];
    assert_eq!(decide(&keep(hit), alien()).0.exclusion, kept_by("r"));
    assert_eq!(decide(&keep(miss), alien()).0.exclusion, None);
    assert_eq!(decide(&evict(hit), alien()).0.force, Some(Force::Must));
    assert_eq!(decide(&evict(miss), alien()).0.force, None);

    let unknown = || {
        let (mut candidate, mut facts) = alien();
        blank(&mut candidate, &mut facts);
        (candidate, facts)
    };
    let (kept, outcome) = decide(&keep(hit), unknown());
    assert_eq!((kept.exclusion, outcome.uncertain), (kept_by("r"), 1), "a missing fact keeps, and says so");
    assert_eq!(decide(&evict(hit), unknown()).0.force, None, "a missing fact never evicts");
}

#[rstest]
#[case::size(|s: &mut Scope| s.size_gib.min = Some(10.0), |s: &mut Scope| s.size_gib.max = Some(10.0))]
#[case::age(|s: &mut Scope| s.age_days.min = Some(365.0), |s: &mut Scope| s.age_days.max = Some(30.0))]
#[case::p_watch(|s: &mut Scope| s.p_watch.max = Some(0.1), |s: &mut Scope| s.p_watch.min = Some(0.5))]
fn the_candidates_own_numbers_always_answer(#[case] hit: fn(&mut Scope), #[case] miss: fn(&mut Scope)) {
    assert_eq!(decide(&[rule("r", scoped(hit), Effect::MustEvict)], alien()).0.force, Some(Force::Must));
    assert_eq!(decide(&[rule("r", scoped(miss), Effect::MustEvict)], alien()).0.force, None);
}

#[test]
fn a_never_played_item_is_outside_every_last_played_range() {
    let (candidate, facts) = alien();
    let never = Facts { played: Some(false), last_played_days: None, ..facts };
    let scope = Scope { last_played_days: Range { min: Some(0.0), max: None }, ..Scope::default() };
    assert_eq!(decide(&[rule("r", scope, Effect::Keep)], (candidate, never)).0.exclusion, None);
}

#[rstest]
#[case::added_within(Event::Added, 500, Scope::default(), true)]
#[case::added_past(Event::Added, 300, Scope::default(), false)]
#[case::played_within(Event::LastPlayed, 120, Scope::default(), true)]
#[case::played_past(Event::LastPlayed, 60, Scope::default(), false)]
#[case::requested_by_ann_within(Event::Requested, 60, Scope { requesters: vec!["ann".into()], ..Scope::default() }, true)]
#[case::requested_by_ann_past(Event::Requested, 5, Scope { requesters: vec!["ann".into()], ..Scope::default() }, false)]
#[case::requested_by_bo(Event::Requested, 60, Scope { requesters: vec!["Bo".into()], ..Scope::default() }, false)]
fn keep_until_holds_for_its_days_after_the_event(#[case] after: Event, #[case] days: u32, #[case] scope: Scope, #[case] kept: bool) {
    let rules = [rule("hold", scope, Effect::KeepUntil { days, after })];
    assert_eq!(decide(&rules, alien()).0.exclusion, kept.then(|| Exclusion::Rule("hold".into())));
}

#[rstest]
#[case::undated_request(|f: &mut Facts| f.requests = Some(vec![RequestFact { requester: "Ann".into(), at: None }]))]
#[case::seerr_unread(|f: &mut Facts| f.requests = None)]
fn keep_until_a_request_keeps_when_the_request_date_is_unknown(#[case] blank: fn(&mut Facts)) {
    let (candidate, mut facts) = alien();
    blank(&mut facts);
    let rules = [rule("hold", Scope::default(), Effect::KeepUntil { days: 60, after: Event::Requested })];
    let (kept, outcome) = decide(&rules, (candidate, facts));
    assert_eq!((kept.exclusion, outcome.uncertain), (kept_by("hold"), 1));
}

#[test]
fn keep_beats_evict_and_the_conflict_is_reported() {
    let rules = [rule("purge 4K", tagged("kids"), Effect::MustEvict), rule("kids", tagged("kids"), Effect::Keep)];
    let (kept, outcome) = decide(&rules, alien());
    assert_eq!((kept.exclusion, kept.force), (kept_by("kids"), None));
    assert_eq!(
        outcome.conflicts,
        vec![Conflict { id: "alien".into(), title: "alien".into(), keep: "kids".into(), evict: "purge 4K".into() }]
    );
    assert_eq!(outcome.rules.iter().map(|count| count.items).collect::<Vec<_>>(), vec![1, 1], "both rules matched");
}

#[test]
fn must_beats_prefer_and_a_disabled_rule_is_silent() {
    let off = Rule { enabled: false, ..rule("off", tagged("kids"), Effect::Keep) };
    let rules = [off, rule("prefer", tagged("kids"), Effect::PreferEvict), rule("must", tagged("kids"), Effect::MustEvict)];
    let (forced, outcome) = decide(&rules, alien());
    assert_eq!((forced.exclusion, forced.force, outcome.forced), (None, Some(Force::Must), 1));
    assert_eq!(outcome.rules.len(), 2, "only enabled rules are counted");
}

#[test]
fn a_certain_keep_names_the_item_over_an_uncertain_one() {
    let (candidate, facts) = alien();
    let facts = Facts { theme: None, ..facts };
    let rules =
        [rule("themed", Scope { themes: vec!["x".into()], ..Scope::default() }, Effect::Keep), rule("kids", tagged("kids"), Effect::Keep)];
    let (kept, outcome) = decide(&rules, (candidate, facts));
    assert_eq!((kept.exclusion, outcome.uncertain), (kept_by("kids"), 0));
}

#[test]
fn rules_never_force_a_pinned_or_partway_item_and_never_rename_a_pin() {
    let (pinned, facts) = alien();
    let pinned = MediaCandidate { exclusion: Some(Exclusion::Pinned(Pin::Favorite)), protect: true, ..pinned };
    assert_eq!(decide(&[rule("kids", tagged("kids"), Effect::Keep)], (pinned.clone(), facts.clone())).0.exclusion, pinned.exclusion);
    assert_eq!(decide(&[rule("go", tagged("kids"), Effect::MustEvict)], (pinned, facts.clone())).0.force, None);
    let partway = MediaCandidate { protect: true, ..alien().0 };
    assert_eq!(decide(&[rule("go", tagged("kids"), Effect::MustEvict)], (partway, facts)).0.force, None);
}

fn forecast(target_gib: u64, emergency: bool) -> VolumeForecast {
    VolumeForecast {
        volume: "movies".to_string(),
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

/// Cheap and dear (both 10 GiB), and a dear one in its grace period; the
/// dear ones carry the `doomed` tag.
fn library() -> PlanInputs {
    let doomed = || Facts { tags: Some(vec!["doomed".to_string()]), ..Facts::default() };
    PlanInputs {
        computed_at: NOW,
        forecasts: vec![forecast(10, false)],
        candidates: vec![
            candidate("cheap", 10, 0.1),
            candidate("dear", 10, 0.9),
            MediaCandidate { age_days: 3.0, ..candidate("fresh", 10, 0.9) },
        ],
        facts: HashMap::from([("dear".to_string(), doomed()), ("fresh".to_string(), doomed())]),
    }
}

#[rstest]
#[case::milp(false)]
#[case::greedy(true)]
fn the_planner_takes_a_must_evict_item_over_a_cheaper_one_but_never_one_in_grace(#[case] emergency: bool) {
    let mut inputs = library();
    let rules = [rule("doomed", tagged("doomed"), Effect::MustEvict)];
    apply(&rules, &mut inputs.candidates, &inputs.facts, NOW);
    let plan = generate_eviction_plan(&inputs.candidates, &[forecast(10, emergency)], &PlannerConfig::default()).expect("valid inputs");
    assert_eq!(ids(&plan), vec!["dear"]);
    assert_eq!(plan.kept["fresh"], Kept::Excluded(Exclusion::Grace { days: 30 }));
}

#[test]
fn the_planner_ignores_a_force_on_a_protected_item_whoever_set_it() {
    let partway = MediaCandidate { protect: true, force: Some(Force::Must), ..candidate("partway", 10, 0.9) };
    let plan = generate_eviction_plan(&[candidate("cheap", 10, 0.1), partway], &[forecast(10, false)], &PlannerConfig::default())
        .expect("valid inputs");
    assert_eq!(ids(&plan), vec!["cheap"]);
}

#[test]
fn the_preview_diffs_the_draft_against_the_saved_rules_on_the_same_inputs() {
    let inputs = library();
    let saved = [rule(
        "keep cheap",
        Scope { size_gib: Range { min: Some(9.0), max: Some(11.0) }, p_watch: Range { max: Some(0.2), min: None }, ..Scope::default() },
        Effect::Keep,
    )];
    let draft = [rule("doomed", tagged("doomed"), Effect::MustEvict)];
    let diff = preview(&inputs, &PlannerConfig::default(), &saved, &draft).expect("valid inputs");
    assert_eq!((diff.saved.items, diff.draft.items), (1, 1));
    // Saved: cheap is kept, so dear goes. Draft: dear is forced, cheap is free
    // but not needed.
    assert!(diff.added.is_empty() && diff.removed.is_empty(), "{diff:?}");

    let diff = preview(&inputs, &PlannerConfig::default(), &[], &saved).expect("valid inputs");
    assert_eq!(diff.added.iter().map(|change| change.id.as_str()).collect::<Vec<_>>(), vec!["dear"]);
    assert_eq!(diff.removed.len(), 1);
    assert_eq!(diff.removed[0].kept_because, "Kept by rule \u{201c}keep cheap\u{201d}");
    assert_eq!(diff.added[0].kept_because, "Not needed this run");
    assert!(diff.draft.regret > diff.saved.regret, "keeping the cheap one costs regret");
}

#[rstest]
#[case::evict_everything(vec![rule("all", Scope::default(), Effect::MustEvict)])]
#[case::same_name(vec![rule("Kids", tagged("a"), Effect::Keep), rule("kids", tagged("b"), Effect::Keep)])]
#[case::blank_name(vec![rule("  ", tagged("a"), Effect::Keep)])]
#[case::no_days(vec![rule("r", tagged("a"), Effect::KeepUntil { days: 0, after: Event::Added })])]
#[case::no_seasons(vec![rule("r", Scope::default(), Effect::KeepLatestSeasons { seasons: 0 })])]
#[case::inverted_range(vec![rule("r", Scope { age_days: Range { min: Some(9.0), max: Some(1.0) }, ..Scope::default() }, Effect::Keep)])]
#[case::probability_above_one(vec![rule("r", Scope { p_watch: Range { min: None, max: Some(1.5) }, ..Scope::default() }, Effect::Keep)])]
#[case::blank_tag(vec![rule("r", tagged(" "), Effect::Keep)])]
fn a_rule_the_editor_would_refuse_is_invalid(#[case] rules: Vec<Rule>) {
    assert!(validate(&rules).is_err());
}

#[test]
fn a_keep_for_everything_is_allowed_and_a_settings_rule_reads_as_written() {
    assert!(validate(&[rule("hold all", Scope::default(), Effect::Keep)]).is_ok());
    let text = r#"{"name": "Ann's requests", "scope": {"requesters": ["Ann"]},
                   "effect": {"type": "keep_until", "days": 60, "after": "requested"}}"#;
    let parsed: Rule = serde_json::from_str(text).expect("a valid rule");
    let expected = rule(
        "Ann's requests",
        Scope { requesters: vec!["Ann".into()], ..Scope::default() },
        Effect::KeepUntil { days: 60, after: Event::Requested },
    );
    assert_eq!(parsed, expected);
    let again: Rule = serde_json::from_value(serde_json::to_value(&parsed).expect("serializable")).expect("round trip");
    assert_eq!(again, expected);
}

#[rstest]
#[case::misspelt_scope_key(r#"{"name": "r", "scope": {"tag": ["a"]}, "effect": {"type": "must_evict"}}"#)]
#[case::unknown_effect(r#"{"name": "r", "scope": {"tags": ["a"]}, "effect": {"type": "delete_now"}}"#)]
fn a_misspelt_rule_is_refused_rather_than_widened(#[case] text: &str) {
    assert!(serde_json::from_str::<Rule>(text).is_err());
}
