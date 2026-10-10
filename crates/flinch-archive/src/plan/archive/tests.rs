use super::super::{generate_eviction_plan, generate_plan, Exclusion, Kept, Pin, PlannerConfig};
use super::*;
use crate::capacity::{CapacityForecast, VolumeForecast};
use crate::plan::knapsack::{Force, Sequence};
use crate::regret::Regret;
use rstest::rstest;

const GIB: u64 = 1 << 30;

fn candidate(id: &str, title: &str, volume: &str, gib: u64, regret: f64) -> MediaCandidate {
    MediaCandidate {
        id: id.to_string(),
        title: title.to_string(),
        size_bytes: gib * GIB,
        volume: Some(volume.to_string()),
        regret: Regret::new(regret, 1.0, 1.0),
        reason: String::new(),
        age_days: 400.0,
        exclusion: None,
        sequence: None,
        handed: false,
        announce: false,
        protect: false,
        quality: crate::quality::advise(&Regret::new(regret, 1.0, 1.0), &crate::quality::Item::default()),
        eviction_safety: 0.0,
        force: None,
    }
}

fn season(series: u64, index: u32, played: bool) -> MediaCandidate {
    let id = format!("sonarr-{series}-s{index}");
    let sequence = Some(Sequence { group: format!("sonarr-{series}"), index, played });
    MediaCandidate { sequence, ..candidate(&id, &format!("Andor S{index}"), "/tv", 10, 1.0) }
}

fn needs(volume: &str, gib: u64) -> VolumeForecast {
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
            target_reclaim_bytes: gib * GIB,
            is_emergency: false,
        },
    }
}

fn to(app: App, root: &str, room_gib: u64) -> ArchiveDestination {
    ArchiveDestination {
        app,
        instance: String::new(),
        root: root.to_string(),
        volume: "/archive".to_string(),
        headroom_bytes: room_gib * GIB,
    }
}

fn plan(candidates: &[MediaCandidate], forecast: VolumeForecast, archive: &[ArchiveDestination]) -> EvictionPlan {
    generate_plan(candidates, &[forecast], &PlannerConfig::default(), archive).expect("valid inputs")
}

#[test]
fn a_movie_moves_to_the_archive_instead_of_leaving() {
    let library = [candidate("radarr-1", "Heat", "/movies", 10, 2.0), candidate("radarr-2", "Ronin", "/movies", 30, 0.5)];
    let result = plan(&library, needs("/movies", 10), &[to(App::Radarr, "/archive/movies", 100)]);

    let expected = PlanMove {
        id: "radarr-1".into(),
        app: App::Radarr,
        instance: String::new(),
        arr_id: 1,
        title: "Heat".into(),
        cards: vec!["radarr-1".into()],
        size_bytes: 10 * GIB,
        volume: "/movies".into(),
        archive_volume: "/archive".into(),
        root: "/archive/movies".into(),
        regret_avoided: 2.0,
    };
    assert_eq!(result.moves, [expected], "the smaller copy covers the target");
    assert!(result.items.is_empty(), "nothing is deleted");
    assert_eq!(result.kept["radarr-1"], Kept::Archived);
    assert_eq!(result.kept["radarr-2"], Kept::NotNeeded);
    assert_eq!((result.volumes[0].planned_bytes, result.moved_bytes(), result.total_reclaimed_bytes), (10 * GIB, 10 * GIB, 0));
    assert!(result.covered());
}

#[test]
fn a_series_moves_as_one_with_every_season() {
    let result = plan(&[season(7, 1, true), season(7, 2, true)], needs("/tv", 10), &[to(App::Sonarr, "/archive/tv", 100)]);
    assert_eq!(result.moves.len(), 1);
    let moved = &result.moves[0];
    assert_eq!((moved.id.as_str(), moved.title.as_str(), moved.size_bytes), ("sonarr-7", "Andor", 20 * GIB));
    assert_eq!(moved.cards, ["sonarr-7-s1", "sonarr-7-s2"]);
    assert!(result.items.is_empty());
}

#[test]
fn a_pinned_season_keeps_its_series_where_it_is() {
    let pinned = MediaCandidate { exclusion: Some(Exclusion::Pinned(Pin::Favorite)), ..season(7, 2, true) };
    let result = plan(&[season(7, 1, true), pinned], needs("/tv", 10), &[to(App::Sonarr, "/archive/tv", 100)]);
    assert!(result.moves.is_empty(), "moving the series would move the pinned season");
    assert_eq!(result.items.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(), ["sonarr-7-s1"]);
}

#[rstest]
#[case::protected(MediaCandidate { protect: true, ..candidate("radarr-1", "Heat", "/movies", 10, 2.0) })]
#[case::handed_over(MediaCandidate { handed: true, ..candidate("radarr-1", "Heat", "/movies", 10, 2.0) })]
#[case::ruled(MediaCandidate { force: Some(Force::Prefer), ..candidate("radarr-1", "Heat", "/movies", 10, 2.0) })]
fn a_partway_handed_or_ruled_item_never_moves(#[case] item: MediaCandidate) {
    let result = plan(&[item], needs("/movies", 10), &[to(App::Radarr, "/archive/movies", 100)]);
    assert!(result.moves.is_empty());
    assert_eq!(result.items.len(), 1);
}

#[test]
fn without_a_destination_for_its_app_nothing_moves() {
    let library = [candidate("radarr-1", "Heat", "/movies", 10, 2.0)];
    assert!(plan(&library, needs("/movies", 10), &[to(App::Sonarr, "/archive/tv", 100)]).moves.is_empty());
    let evictions_only = generate_eviction_plan(&library, &[needs("/movies", 10)], &PlannerConfig::default()).expect("valid inputs");
    assert_eq!((evictions_only.moves.len(), evictions_only.items.len()), (0, 1));
}

#[test]
fn each_instance_archives_only_to_its_own_destination() {
    let library = [candidate("radarr@4k-1", "Heat", "/movies", 10, 2.0)];
    assert!(
        plan(&library, needs("/movies", 10), &[to(App::Radarr, "/archive/movies", 100)]).moves.is_empty(),
        "the default's root is not 4k's"
    );
    let uhd = ArchiveDestination { instance: "4k".into(), ..to(App::Radarr, "/archive/4k", 100) };
    let moved = plan(&library, needs("/movies", 10), &[uhd]).moves;
    assert_eq!(
        moved.iter().map(|planned| (planned.id.as_str(), planned.instance.as_str(), planned.arr_id)).collect::<Vec<_>>(),
        [("radarr@4k-1", "4k", 1)]
    );
}

#[rstest]
#[case::movie("radarr-12", Some((App::Radarr, "", 12)))]
#[case::season("sonarr-7-s3", Some((App::Sonarr, "", 7)))]
#[case::season_zero("sonarr-7-s0", Some((App::Sonarr, "", 7)))]
#[case::named_movie("radarr@4k-12", Some((App::Radarr, "4k", 12)))]
#[case::named_season("sonarr@anime-7-s3", Some((App::Sonarr, "anime", 7)))]
#[case::unknown("plex-4", None)]
#[case::malformed("radarr-x", None)]
fn a_card_names_its_arr_item(#[case] card: &str, #[case] item: Option<(App, &str, u64)>) {
    assert_eq!(arr_item(card), item);
}
