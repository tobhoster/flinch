//! Maintainerr's own rules on the collections FLINCH fills are turned off,
//! whole and with a backup; nothing else is touched.

use super::super::own_rules::{read_rule_groups, rules_off_request};
use super::super::{competing, turn_off, RuleGroup, RulesOff};
use super::fake::{Fake, Fault, Op};
use super::{collection, current, valid_collections, titles, SEASONS};
use serde_json::{json, Value};
use std::cell::RefCell;

/// The live "Watched Seasons Cleanup" group (v3.29), rules on, trimmed.
fn group(id: i64, collection_id: i64, use_rules: bool, rules: usize) -> Value {
    json!({
        "id": id, "name": format!("group {id}"), "description": "Remove a season once watched", "libraryId": "2",
        "isActive": true, "collectionId": collection_id, "useRules": use_rules, "dataType": "season",
        "ruleHandlerCronSchedule": null, "notifications": [],
        "rules": (0..rules).map(|n| json!({"id": 80 + n, "ruleJson": "{\"action\":16}", "ruleGroupId": id, "section": 0, "isActive": true})).collect::<Vec<_>>(),
        "collection": {
            "id": collection_id, "mediaServerId": "1561", "mediaServerType": "plex", "libraryId": "2", "title": "Watched Seasons Cleanup",
            "isActive": true, "arrAction": 2, "visibleOnRecommended": true, "visibleOnHome": true, "deleteAfterDays": 0,
            "manualCollection": false, "manualCollectionName": "", "listExclusions": true, "forceSeerr": true,
            "keepLogsForMonths": 6, "addDate": "2026-03-14 06:38:18", "handledMediaAmount": 12, "lastDurationInSeconds": 1,
            "tautulliWatchedPercentOverride": null, "sonarrSettingsId": 1, "radarrSettingsId": null,
            "sortTitle": "001 Leaving Soon", "overlayEnabled": true, "overlayTemplateId": 3, "media": [{"mediaServerId": "946"}]
        }
    })
}

fn parsed(value: Value) -> RuleGroup {
    RuleGroup::from_value(value).expect("a rule group")
}

#[test]
fn a_rule_group_reads_from_the_live_list() {
    let body = Value::Array(vec![group(11, SEASONS, true, 4), json!({"name": "no id"})]).to_string();
    let groups = read_rule_groups(200, &body).expect("parsed");
    assert_eq!(groups.len(), 1, "a group without an id is skipped, not a failure");
    assert_eq!((groups[0].id, groups[0].collection_id, groups[0].use_rules, groups[0].rules), (11, Some(SEASONS), true, 4));
    assert!(read_rule_groups(500, "{}").is_err());
}

#[test]
fn the_update_keeps_every_collection_setting_and_turns_only_the_rules_off() {
    let update = rules_off_request(&parsed(group(11, SEASONS, true, 4))).expect("a body");
    assert_eq!(update["useRules"], json!(false));
    assert_eq!(update["rules"], json!([]));
    let collection = &update["collection"];
    for (field, kept) in [("sortTitle", json!("001 Leaving Soon")), ("overlayEnabled", json!(true)), ("overlayTemplateId", json!(3)), ("deleteAfterDays", json!(0)), ("visibleOnHome", json!(true))] {
        assert_eq!(collection[field], kept, "{field} goes back as it was: omitting it resets it");
    }
    for runtime in ["media", "id", "addDate", "handledMediaAmount", "mediaServerId"] {
        assert!(collection.get(runtime).is_none(), "{runtime} is state, not a setting");
    }
    assert_eq!((update["arrAction"].clone(), update["forceSeerr"].clone(), update["sonarrSettingsId"].clone()), (json!(2), json!(true), json!(1)), "the DTO's top-level settings");
    assert!(update.get("collectionId").is_none() && update.get("radarrSettingsId").is_none() && update.get("ruleHandlerCronSchedule").is_none(), "nulls and read-only fields stay out");
    assert_eq!((update["id"].clone(), update["libraryId"].clone(), update["dataType"].clone()), (json!(11), json!("2"), json!("season")));
}

#[test]
fn only_rules_that_still_pick_items_for_a_collection_flinch_fills_compete() {
    let mut collections = valid_collections();
    collections.push(collection(99, "Someone else's", "season", "2", true, 0));
    let groups = [
        parsed(group(1, SEASONS, true, 4)),
        parsed(group(2, 99, true, 3)),
        parsed(group(3, SEASONS, false, 4)),
        parsed(group(4, SEASONS, true, 0)),
    ];
    let ids: Vec<i64> = competing(&groups, &collections, &titles()).iter().map(|group| group.id).collect();
    assert_eq!(ids, [1], "a collection FLINCH does not use, rules already off, or no rule at all: left alone");
}

fn fake_with(groups: Vec<Value>) -> Fake {
    let mut fake = Fake::new(current(), valid_collections());
    fake.groups = groups.into_iter().map(parsed).collect();
    fake
}

#[tokio::test]
async fn competing_rules_are_backed_up_then_turned_off_and_read_back() {
    let mut fake = fake_with(vec![group(11, SEASONS, true, 4)]);
    let backed_up = RefCell::new(Vec::new());
    let outcomes = turn_off(&mut fake, &valid_collections(), &titles(), |group: &RuleGroup| {
        assert!(fake_writes_none(&backed_up), "the backup comes before anything is sent");
        backed_up.borrow_mut().push(group.raw.clone());
        Ok(format!("backup/{}.json", group.id))
    })
    .await
    .expect("read");
    assert_eq!(outcomes, [RulesOff::Done { name: "group 11".into(), backup: "backup/11.json".into() }]);
    assert_eq!(backed_up.borrow()[0]["rules"].as_array().map(Vec::len), Some(4), "the backup holds the rules being turned off");
    assert_eq!(fake.writes, [(Op::TurnOffRules, "11".to_string())]);
    assert!(!fake.groups[0].picks());

    let again = turn_off(&mut fake, &valid_collections(), &titles(), |_: &RuleGroup| Ok(String::new())).await.expect("read");
    assert!(again.is_empty(), "once off, nothing more is sent");
}

fn fake_writes_none(backed_up: &RefCell<Vec<Value>>) -> bool {
    backed_up.borrow().is_empty()
}

#[tokio::test]
async fn without_a_backup_nothing_is_sent() {
    let mut fake = fake_with(vec![group(11, SEASONS, true, 4)]);
    let outcomes = turn_off(&mut fake, &valid_collections(), &titles(), |_: &RuleGroup| Err(std::io::Error::other("read-only state"))).await.expect("read");
    assert!(matches!(&outcomes[0], RulesOff::Failed { error, .. } if error.contains("backup not written")), "{outcomes:?}");
    assert!(fake.writes.is_empty());
}

#[tokio::test]
async fn an_update_maintainerr_accepts_but_does_not_apply_is_a_failure() {
    let mut fake = fake_with(vec![group(11, SEASONS, true, 4)]);
    fake.fail(Op::TurnOffRules, Fault::Lie);
    let outcomes = turn_off(&mut fake, &valid_collections(), &titles(), |_: &RuleGroup| Ok("b".into())).await.expect("read");
    assert!(matches!(&outcomes[0], RulesOff::Failed { error, .. } if error.contains("still on")), "{outcomes:?}");
}

#[tokio::test]
async fn a_dry_run_prints_and_writes_no_backup() {
    let mut fake = fake_with(vec![group(11, SEASONS, true, 4)]);
    fake.simulated = true;
    let outcomes = turn_off(&mut fake, &valid_collections(), &titles(), |_: &RuleGroup| -> std::io::Result<String> { panic!("no backup in a dry run") })
        .await
        .expect("read");
    assert_eq!(outcomes, [RulesOff::DryRun { name: "group 11".into() }]);
    assert!(fake.groups[0].picks(), "nothing changed");
}
