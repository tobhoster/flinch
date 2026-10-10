//! Seed goals, path mapping and settings bounds; the clients and the card
//! map have their own files.

use super::{ClientConfig, ClientKind, PathMap, Torrent, TorrentsConfig};
use rstest::rstest;

mod fake;
mod map;
mod qbittorrent;
mod transmission;

const DAY: u64 = 86_400;

fn torrent(ratio: f64, days: u64) -> Torrent {
    Torrent {
        hash: "a".repeat(40),
        name: "Movie".into(),
        ratio,
        seeding_secs: days * DAY,
        complete: true,
        limit_reached: false,
        content_path: "/downloads/Movie".into(),
    }
}

#[rstest]
#[case::below_both_floors(torrent(0.5, 3), true, false)]
#[case::ratio_met(torrent(1.0, 0), true, true)]
#[case::days_met(torrent(0.1, 14), true, true)]
#[case::the_clients_own_limit_met(Torrent { limit_reached: true, ..torrent(0.2, 1) }, true, true)]
#[case::still_downloading(Torrent { complete: false, limit_reached: true, ..torrent(5.0, 30) }, true, false)]
#[case::no_floor_of_flinchs_own(torrent(0.0, 0), false, true)]
fn a_torrent_meets_its_goal_by_the_clients_limit_or_flinchs_floor(#[case] torrent: Torrent, #[case] floor: bool, #[case] meets: bool) {
    let config =
        if floor { TorrentsConfig::default() } else { TorrentsConfig { min_ratio: 0.0, min_seed_days: 0, ..TorrentsConfig::default() } };
    assert_eq!(config.meets_goal(&torrent), meets);
}

#[rstest]
#[case::longest_prefix_wins("/data/torrents/tv/Show/e1.mkv", "/mnt/tv/Show/e1.mkv")]
#[case::shorter_prefix("/data/torrents/movies/M.mkv", "/mnt/dl/movies/M.mkv")]
#[case::only_at_a_path_component("/data/torrentsextra/x", "/data/torrentsextra/x")]
#[case::the_prefix_itself("/data/torrents", "/mnt/dl")]
#[case::unmapped("/elsewhere/x", "/elsewhere/x")]
fn client_paths_map_through_the_longest_matching_prefix(#[case] path: &str, #[case] local: &str) {
    let config = TorrentsConfig {
        path_map: vec![
            PathMap { from: "/data/torrents/".into(), to: "/mnt/dl".into() },
            PathMap { from: "/data/torrents/tv".into(), to: "/mnt/tv/".into() },
        ],
        ..TorrentsConfig::default()
    };
    assert_eq!(config.local(path), std::path::PathBuf::from(local));
}

fn client(url: &str, password_env: Option<&str>) -> ClientConfig {
    ClientConfig { kind: ClientKind::Qbittorrent, url: url.into(), username: "admin".into(), password_env: password_env.map(Into::into) }
}

#[rstest]
#[case::negative_ratio(TorrentsConfig { min_ratio: -1.0, ..TorrentsConfig::default() })]
#[case::not_a_ratio(TorrentsConfig { min_ratio: f64::NAN, ..TorrentsConfig::default() })]
#[case::days_beyond_ten_years(TorrentsConfig { min_seed_days: 3_651, ..TorrentsConfig::default() })]
#[case::desired_ratio_not_a_number(TorrentsConfig { prefer_after_ratio: f64::INFINITY, ..TorrentsConfig::default() })]
#[case::desired_ratio_negative(TorrentsConfig { prefer_after_ratio: -0.5, ..TorrentsConfig::default() })]
#[case::not_a_url(TorrentsConfig { clients: vec![client("qbittorrent:8080", None)], ..TorrentsConfig::default() })]
#[case::credentials_in_the_url(TorrentsConfig { clients: vec![client("http://admin:pw@qbit:8080", None)], ..TorrentsConfig::default() })]
#[case::a_password_not_a_variable_name(TorrentsConfig { clients: vec![client("http://qbit:8080", Some("hunter2!"))], ..TorrentsConfig::default() })]
#[case::relative_path_map(TorrentsConfig { path_map: vec![PathMap { from: "data".into(), to: "/mnt".into() }], ..TorrentsConfig::default() })]
fn settings_the_page_would_refuse_are_invalid(#[case] config: TorrentsConfig) {
    assert!(config.validate().is_err());
}

#[test]
fn a_configured_client_with_its_password_in_a_variable_is_valid() {
    let config = TorrentsConfig { clients: vec![client("http://qbittorrent:8080", Some("QBIT_PASSWORD"))], ..TorrentsConfig::default() };
    assert_eq!(config.validate(), Ok(()));
}
