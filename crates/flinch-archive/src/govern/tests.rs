use super::*;
use crate::arr::{SeasonStats, SeriesSeason};
use crate::capacity::{AppDisks, CapacityAction, Credit, RecycleBin, RootFolder, Volume};
use std::collections::BTreeMap;
use rstest::rstest;

const GB: u64 = 1_000_000_000;

fn movie(id: u32, path: Option<&str>) -> ArrMovie {
    ArrMovie {
        id,
        title: format!("Movie {id}"),
        title_slug: None,
        year: None,
        size_on_disk: 10 * GB,
        has_file: true,
        added: None,
        images: Vec::new(),
        movie_file: None,
        path: path.map(str::to_string),
        ..Default::default()
    }
}

fn show(id: u32, path: Option<&str>, seasons: &[u32]) -> ArrSeries {
    ArrSeries {
        id,
        title: format!("Show {id}"),
        title_slug: None,
        year: None,
        series_type: "standard".to_string(),
        seasons: seasons
            .iter()
            .map(|n| SeriesSeason {
                season_number: *n,
                statistics: SeasonStats { episode_file_count: 1, episode_count: 1, total_episode_count: 1, size_on_disk: GB },
                ..Default::default()
            })
            .collect(),
        added: None,
        images: Vec::new(),
        path: path.map(str::to_string),
        status: None,
        previous_airing: None,
        ..Default::default()
    }
}

/// Radarr and Sonarr both see one 100 GB share at /media, `used_gb` full.
fn library(used_gb: u64) -> LibraryVolumes {
    let share = Volume { path: "/media".to_string(), total_bytes: 100 * GB, free_bytes: (100 - used_gb) * GB };
    let root = |path: &str| vec![RootFolder { path: path.to_string(), free_bytes: None }];
    LibraryVolumes::build(&[
        AppDisks { app: App::Radarr, diskspace: vec![share.clone()], root_folders: root("/media/movies"), recycle: RecycleBin::Disabled },
        AppDisks { app: App::Sonarr, diskspace: vec![share], root_folders: root("/media/tv"), recycle: RecycleBin::Disabled },
    ])
}

fn settings(ceiling: f32, release: f32) -> RuntimeSettings {
    RuntimeSettings { capacity_ceiling: ceiling, capacity_release: release, ..RuntimeSettings::default() }
}

fn governed(used_gb: u64, ceiling: f32, release: f32) -> (Governance, ArchivePolicy) {
    let library = library(used_gb);
    let volume_of = volume_map(
        &library,
        &[movie(1, Some("/media/movies/One (2001)")), movie(2, None)],
        &[show(7, Some("/media/tv/Seven"), &[1, 2])],
    );
    let mut policy = ArchivePolicy::default();
    let governance = govern(library, volume_of, |_| true, &settings(ceiling, release), &Latch::default(), OnDisk::default(), &mut policy);
    (governance, policy)
}

#[test]
fn items_are_attributed_through_their_own_app_and_path() {
    let library = library(50);
    let map = volume_map(
        &library,
        &[movie(1, Some("/media/movies/One")), movie(2, None), movie(3, Some("/downloads/Three"))],
        &[show(7, Some("/media/tv/Seven"), &[1, 2]), show(8, None, &[1])],
    );
    assert_eq!(map.get("radarr-1").map(String::as_str), Some("/media"));
    assert_eq!(map.get("sonarr-7-s1").map(String::as_str), Some("/media"));
    assert_eq!(map.get("sonarr-7-s2").map(String::as_str), Some("/media"));
    for absent in ["radarr-2", "radarr-3", "sonarr-8-s1"] {
        assert!(!map.contains_key(absent), "{absent} has no governed path");
    }
}

#[rstest]
#[case::release_above_ceiling(0.70, 0.75)]
#[case::ceiling_above_one(1.20, 0.75)]
#[case::nan(f32::NAN, 0.75)]
fn malformed_watermarks_govern_nothing_and_say_so(#[case] ceiling: f32, #[case] release: f32) {
    let (governance, policy) = governed(95, ceiling, release);
    assert!(governance.invalid_watermarks);
    assert_eq!(governance.decision.action, CapacityAction::Unmeasured);
    assert!(matches!(&governance.goal, ReclaimGoal::PerVolume(goals) if goals.goals.is_empty()), "no goal, no eviction");
    assert!(!policy.unwatched_reclaim.enabled, "no pressure without valid governance");
    assert_eq!(governance.held_reason("radarr-1"), "Eligible — held while disk usage is unmeasured");
    assert!(governance.status(&[], []).is_none());
}

#[test]
fn no_library_volume_is_unmeasured() {
    let mut policy = ArchivePolicy::default();
    let governance = govern(LibraryVolumes::default(), HashMap::new(), |_| true, &RuntimeSettings::default(), &Latch::default(), OnDisk::default(), &mut policy);
    assert_eq!(governance.decision.action, CapacityAction::Unmeasured);
    assert!(!governance.invalid_watermarks);
}

#[test]
fn under_the_ceiling_nothing_is_goaled_and_items_say_why_they_stay() {
    let (governance, policy) = governed(50, 0.80, 0.75);
    assert_eq!(governance.decision.action, CapacityAction::Idle);
    assert!(governance.decision.goals.is_empty());
    assert!(!policy.unwatched_reclaim.enabled);
    assert_eq!(governance.held_reason("sonarr-7-s1"), "Eligible — held while /media is under the 80% ceiling");
}

#[test]
fn over_the_ceiling_the_volume_evicts_to_the_release_mark() {
    let (governance, policy) = governed(90, 0.80, 0.75);
    assert!(matches!(governance.decision.action, CapacityAction::Evict { goal_bytes, .. } if goal_bytes == 15 * GB));
    assert!(policy.unwatched_reclaim.enabled, "default settings arm never-played while evicting");
    assert_eq!(governance.held_reason("radarr-1"), "Eligible — not needed yet to bring /media back to 75%");
    let status = governance.status(&[], []).expect("measured");
    assert_eq!((status.covered, status.goal_met), (Some(false), Some(false)), "nothing planned or handed over against a 15 GB goal");
}

#[test]
fn an_item_on_no_governed_disk_says_it_is_never_evicted() {
    let (governance, _) = governed(90, 0.80, 0.75);
    assert_eq!(governance.volume_for("radarr-2"), None);
    assert_eq!(governance.held_reason("radarr-2"), "Eligible, but no governed disk holds it — never evicted");
}

#[rstest]
#[case::recycle_bin(Credit { pending: 40 * GB, held: 0 }, "Eligible — held while /media's recycle bin releases space already evicted")]
#[case::never_released(
    Credit { pending: 0, held: 40 * GB },
    "Eligible — held while /media waits for space handed over earlier that the disk has not released"
)]
fn a_volume_waiting_on_credited_space_explains_the_hold(#[case] credit: Credit, #[case] reason: &str) {
    let library = library(90);
    let volume_of = volume_map(&library, &[movie(1, Some("/media/movies/One"))], &[]);
    let on_disk = OnDisk { credit: BTreeMap::from([("/media".to_string(), credit)]), ..OnDisk::default() };
    let mut policy = ArchivePolicy::default();
    let governance = govern(library, volume_of, |_| true, &settings(0.80, 0.75), &Latch::default(), on_disk, &mut policy);
    assert_eq!(governance.held_reason("radarr-1"), reason);
    let status = governance.status(&[], []).expect("measured");
    assert_eq!((status.goal_bytes, status.pending_bytes, status.held_bytes), (0, credit.pending, credit.held));
    assert_eq!(status.goal_met, Some(true), "the credited bytes already cover the gap: nothing more is evicted for it");
}

#[test]
fn an_item_flinch_cannot_hand_over_never_counts_toward_a_goal() {
    // No Plex identity means Maintainerr cannot act on it: counting it made a
    // disk stuck at 85% report its goal as met, cycle after cycle.
    let library = library(90);
    let located = volume_map(&library, &[movie(1, Some("/media/movies/One")), movie(2, Some("/media/movies/Two"))], &[]);
    let mut policy = ArchivePolicy::default();
    let governance =
        govern(library, located, |id| id != "radarr-2", &settings(0.80, 0.75), &Latch::default(), OnDisk::default(), &mut policy);
    assert_eq!(governance.volume_for("radarr-2").as_deref(), Some("/media"), "still shown on its disk");
    assert_eq!(
        governance.held_reason("radarr-2"),
        "Eligible, but not matched in Plex by id — FLINCH cannot hand it to Maintainerr, so it is never evicted"
    );
    assert!(matches!(
        &governance.goal,
        ReclaimGoal::PerVolume(goals) if goals.volume_of.contains_key("radarr-1") && !goals.volume_of.contains_key("radarr-2")
    ));
}
