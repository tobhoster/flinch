use super::*;
use crate::inflow::Rule;
use rstest::rstest;
use serde_json::json;

const SONARR_LIST: ImportListRef = ImportListRef { app: App::Sonarr, id: 3 };
const RADARR_LIST: ImportListRef = ImportListRef { app: App::Radarr, id: 4 };

fn suggestion(subject: &str) -> Suggestion {
    Suggestion {
        title: format!("title of {subject}"),
        subject: subject.to_string(),
        rule: Rule::ColdUnstarted,
        why: String::new(),
        gib_per_season: Some(10.0),
        action: "unmonitor future seasons in Sonarr".to_string(),
        taste: Some(-1.0),
        requester: None,
    }
}

fn config(approved: &[&str]) -> InflowActionsConfig {
    InflowActionsConfig {
        enabled: true,
        approved: approved.iter().map(|subject| subject.to_string()).collect(),
        import_lists: vec![SONARR_LIST, RADARR_LIST],
    }
}

fn unmonitor(id: u32) -> Action {
    Action::Unmonitor { series_id: id, subject: format!("sonarr-{id}"), title: format!("title of sonarr-{id}") }
}

#[test]
fn over_target_only_approved_suggested_shows_are_unmonitored_and_chosen_lists_go_off() {
    let suggestions = [suggestion("sonarr-7"), suggestion("sonarr-8"), suggestion("radarr-9")];
    // sonarr-5 is approved but no longer suggested; radarr-9 is not a show.
    let actions = plan(&config(&["sonarr-7", "sonarr-5", "radarr-9"]), &suggestions, true, &InflowLedger::default());
    assert_eq!(actions, vec![Action::ListOff(SONARR_LIST), Action::ListOff(RADARR_LIST), unmonitor(7)]);
}

#[test]
fn under_target_nothing_is_written_and_lists_flinch_switched_off_come_back() {
    let ledger = InflowLedger { lists_off: vec![(SONARR_LIST, 1)], ..InflowLedger::default() };
    let actions = plan(&config(&["sonarr-7"]), &[suggestion("sonarr-7")], false, &ledger);
    assert_eq!(actions, vec![Action::ListOn(SONARR_LIST)]);
}

#[rstest]
#[case::feature_off(false, vec![SONARR_LIST])]
#[case::list_dropped_from_settings(true, vec![])]
fn a_list_flinch_holds_off_comes_back_once_it_is_no_longer_wanted_off(#[case] enabled: bool, #[case] lists: Vec<ImportListRef>) {
    let config = InflowActionsConfig { enabled, import_lists: lists, ..config(&[]) };
    let ledger = InflowLedger { lists_off: vec![(SONARR_LIST, 1)], ..InflowLedger::default() };
    assert_eq!(plan(&config, &[], true, &ledger), vec![Action::ListOn(SONARR_LIST)]);
}

#[test]
fn each_thing_is_changed_once_and_a_dropped_approval_is_forgotten() {
    let mut ledger = InflowLedger {
        unmonitored: BTreeMap::from([("sonarr-7".to_string(), 1), ("sonarr-8".to_string(), 1)]),
        lists_off: vec![(SONARR_LIST, 1), (RADARR_LIST, 1)],
    };
    let config = config(&["sonarr-7"]);
    ledger.prune(&config);
    assert_eq!(ledger.unmonitored.keys().collect::<Vec<_>>(), vec!["sonarr-7"]);
    assert!(plan(&config, &[suggestion("sonarr-7")], true, &ledger).is_empty());
}

#[test]
fn future_seasons_are_unmonitored_and_seasons_on_disk_keep_theirs() {
    let mut series = json!({
        "id": 7, "monitored": true, "monitorNewItems": "all",
        "seasons": [
            {"seasonNumber": 0, "monitored": true, "statistics": {"episodeFileCount": 0}},
            {"seasonNumber": 1, "monitored": true, "statistics": {"episodeFileCount": 8}},
            {"seasonNumber": 2, "monitored": true, "statistics": {"episodeFileCount": 0}},
            {"seasonNumber": 3, "monitored": true}
        ]
    });
    assert!(!future_unmonitored(&series));
    assert!(unmonitor_future(&mut series));
    let monitored: Vec<bool> =
        series["seasons"].as_array().into_iter().flatten().map(|season| season["monitored"] == json!(true)).collect();
    assert_eq!(monitored, vec![true, true, false, true], "specials, a season on disk and one without statistics stay");
    assert_eq!(series["monitorNewItems"], json!("none"));
    assert!(future_unmonitored(&series));
    assert!(!unmonitor_future(&mut series), "a second pass changes nothing");
}

#[rstest]
#[case::sonarr(App::Sonarr, json!({"id": 3, "name": "Trakt", "enableAutomaticAdd": true}))]
#[case::radarr(App::Radarr, json!({"id": 4, "name": "IMDb", "enabled": true, "enableAuto": true}))]
fn each_app_switches_its_own_automatic_add_field(#[case] app: App, #[case] mut list: Value) {
    assert_eq!(set_auto_add(&mut list, app, false), Some(true));
    assert_eq!(auto_add(&list, app), Some(false));
    assert_eq!(set_auto_add(&mut list, app, false), Some(false), "already off: nothing to restore later");
    assert!(list.get("enabled").and_then(Value::as_bool).unwrap_or(true), "only automatic add moves");
}

#[test]
fn a_list_without_the_field_is_not_written() {
    assert_eq!(set_auto_add(&mut json!({"id": 3}), App::Sonarr, false), None);
    let known = known_lists(App::Sonarr, &json!([{"id": 3, "name": "Trakt", "enableAutomaticAdd": false}, {"name": "no id"}]));
    assert_eq!(known, vec![KnownList { app: App::Sonarr, id: 3, name: "Trakt".to_string(), auto_add: Some(false) }]);
}

#[rstest]
#[case::not_a_show(InflowActionsConfig { approved: vec!["radarr-1".into()], ..InflowActionsConfig::default() })]
#[case::no_id(InflowActionsConfig { approved: vec!["sonarr-x".into()], ..InflowActionsConfig::default() })]
#[case::list_twice(InflowActionsConfig { import_lists: vec![SONARR_LIST, SONARR_LIST], ..InflowActionsConfig::default() })]
#[case::list_without_id(InflowActionsConfig { import_lists: vec![ImportListRef { app: App::Radarr, id: 0 }], ..InflowActionsConfig::default() })]
fn settings_the_page_would_refuse_are_invalid(#[case] config: InflowActionsConfig) {
    assert!(config.validate().is_err());
}
