//! The GiB estimate beside each change, and the opt-in deletion of profiles
//! nothing uses, against the fake Radarr (one operator profile, "Any", id 1,
//! upgrading until WEB 1080p whose qualities are capped at 100 MB/min).

use super::radarr::{self, State, KEY};
use super::sync::{compact, guide};
use crate::capacity::App;
use crate::trash::config::InstanceConfig;
use crate::trash::diff::{Action, Kind, Owned};
use crate::trash::impact::Sizes;
use crate::trash::{presets, sync_app, AppOutcome, AppState, AppSync, ArrClient, DiskOutlook, Selection, Usage};
use rstest::rstest;
use std::collections::{BTreeMap, BTreeSet};

const GB: u64 = 1_000_000_000;

struct Options<'a> {
    delete_unused_profiles: bool,
    usage: Option<&'a Usage>,
    outlook: Option<DiskOutlook>,
    keep_profile: Option<&'a str>,
}

async fn run(
    server: &super::fake::Server,
    config: &InstanceConfig,
    options: Options<'_>,
    apply: Option<(Selection<'_>, bool)>,
) -> (AppState, Option<AppOutcome>) {
    let http = reqwest::Client::new();
    let client = ArrClient::new(&http, App::Radarr, &server.base, KEY);
    let guide = guide();
    let sync = AppSync {
        client: &client,
        config,
        guide: &guide,
        delete_unmanaged: false,
        delete_unused_profiles: options.delete_unused_profiles,
        owned: Owned::default(),
        usage: options.usage,
        outlook: options.outlook,
        keep_profile: options.keep_profile,
    };
    sync_app(sync, apply).await
}

fn usage(on_any: Vec<Sizes>) -> Usage {
    Usage { profiles: BTreeMap::from([(1, on_any)]), complete: true }
}

fn sized(smallest: u64, largest: u64) -> Sizes {
    Sizes { smallest: Some(smallest), largest: Some(largest) }
}

/// The preset's original-quality profile adopted as the operator's "Any".
fn raise_any_to_web_2160p() -> InstanceConfig {
    let mut config = presets::radarr();
    config.quality_profiles.truncate(1);
    config.quality_profiles[0].name = Some("Any".to_string());
    config.quality_definition = None;
    config
}

#[tokio::test]
async fn raising_a_cutoff_forecasts_more_gib_from_the_items_on_the_profile_and_their_cached_releases() {
    let server = radarr::serve(State::fixtures());
    // Spreads 10 GB and 2 GB; the third item has no search yet and counts at the mean.
    let held = usage(vec![sized(GB, 11 * GB), sized(2 * GB, 4 * GB), Sizes::default()]);
    let outlook = DiskOutlook { projected_used_bytes: 50 * GB, capacity_bytes: 100 * GB };
    let options = Options { delete_unused_profiles: false, usage: Some(&held), outlook: Some(outlook), keep_profile: None };

    let (state, _) = run(&server, &raise_any_to_web_2160p(), options, None).await;

    let change = state.changes.iter().find(|c| c.kind == Kind::QualityProfile).expect("the adopted profile changes");
    assert_eq!((change.action, change.arr_id), (Action::Update, Some(1)));
    let impact = &state.impacts[&change.id];
    assert_eq!((impact.items, impact.sampled), (3, 2));
    // WEB 1080p reaches 100 of 2000 MB/min (0.05), WEB 2160p is unlimited (1.0):
    // mean spread 6 GB × 3 items × 0.95.
    assert_eq!(impact.delta_bytes, 17_100_000_000);
    assert_eq!(impact.utilization_before, Some(0.5));
    assert!(impact.utilization_after.is_some_and(|after| (after - 0.671).abs() < 1e-9), "{:?}", impact.utilization_after);
}

#[tokio::test]
async fn a_size_table_that_raises_maxima_warns_and_is_sized_over_every_profile() {
    let server = radarr::serve(State::fixtures());
    let held = usage(vec![sized(GB, 11 * GB)]);
    let options = Options { delete_unused_profiles: false, usage: Some(&held), outlook: None, keep_profile: None };

    let (state, _) = run(&server, &compact(Vec::new()), options, None).await;

    let impact = &state.impacts["radarr:sizes:movie"];
    assert!(impact.warnings.iter().any(|w| w.starts_with("WEBDL-1080p: max size rises from 100 to 2000")), "{:?}", impact.warnings);
    assert_eq!(impact.items, 1);
    assert!(impact.delta_bytes > 0, "Any upgrades until WEB 1080p, whose cap rises: {}", impact.delta_bytes);
    assert_eq!((impact.utilization_before, impact.utilization_after), (None, None), "no forecast yet");
}

#[tokio::test]
async fn without_a_library_read_there_is_no_estimate() {
    let server = radarr::serve(State::fixtures());
    let options = Options { delete_unused_profiles: true, usage: None, outlook: None, keep_profile: None };
    let (state, _) = run(&server, &compact(Vec::new()), options, None).await;
    assert!(state.impacts.is_empty());
    assert!(
        !state.changes.iter().any(|c| c.kind == Kind::QualityProfile && c.action == Action::Delete),
        "use unknown: no profile is offered"
    );
}

#[rstest]
#[case::unused_and_opted_in(true, vec![], true, None, true)]
#[case::not_opted_in(false, vec![], true, None, false)]
#[case::in_use(true, vec![Sizes::default()], true, None, false)]
#[case::some_item_unread(true, vec![], false, None, false)]
#[case::the_quality_actions_fallback(true, vec![], true, Some("any"), false)]
#[tokio::test]
async fn only_an_unused_unmanaged_profile_is_offered_for_deletion(
    #[case] opted_in: bool,
    #[case] on_any: Vec<Sizes>,
    #[case] complete: bool,
    #[case] keep: Option<&'static str>,
    #[case] offered: bool,
) {
    let server = radarr::serve(State::fixtures());
    let held = Usage { complete, ..usage(on_any) };
    let options = Options { delete_unused_profiles: opted_in, usage: Some(&held), outlook: None, keep_profile: keep };

    let (state, _) = run(&server, &compact(Vec::new()), options, None).await;

    let deletes: Vec<&str> =
        state.changes.iter().filter(|c| c.kind == Kind::QualityProfile && c.action == Action::Delete).map(|c| c.id.as_str()).collect();
    assert_eq!(deletes, offered.then_some("radarr:profile-delete:1").into_iter().collect::<Vec<_>>());
}

#[tokio::test]
async fn a_profile_is_deleted_only_when_the_operator_selects_it_never_by_apply_everything() {
    let server = radarr::serve(State::fixtures());
    let held = usage(Vec::new());
    let options = || Options { delete_unused_profiles: true, usage: Some(&held), outlook: None, keep_profile: None };

    let (everything, _) = run(&server, &compact(Vec::new()), options(), Some((Selection::All, false))).await;
    assert!(!server.requests().iter().any(|r| r.method == "DELETE"), "apply everything leaves profiles alone");
    assert_eq!(everything.changes.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["radarr:profile-delete:1"], "still offered");

    let selected = BTreeSet::from(["radarr:profile-delete:1".to_string()]);
    let (state, outcome) = run(&server, &compact(Vec::new()), options(), Some((Selection::Only(&selected), false))).await;
    assert_eq!(outcome.expect("an outcome").applied, ["radarr:profile-delete:1"]);
    assert!(server.requests().iter().any(|r| r.method == "DELETE" && r.path == "/api/v3/qualityprofile/1"));
    assert!(state.changes.is_empty(), "{:?}", state.changes);
}
