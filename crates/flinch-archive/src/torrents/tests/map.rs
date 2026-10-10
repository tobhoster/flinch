//! Cards to torrents, hardlinks on a real filesystem, and the holds the
//! planner applies.

use super::super::map::{self, LibraryLinks, MatchedBy};
use super::super::{ClientKind, Holding, SeedHold, Torrent, TorrentError, TorrentsConfig};
use super::torrent;
use crate::arr::history::HistoryRecord;
use rstest::rstest;
use std::collections::{BTreeMap, HashMap, HashSet};

fn hash(c: char) -> String {
    c.to_string().repeat(40)
}

#[test]
fn each_file_counts_its_newest_import_only() {
    let rows = format!(
        r#"[
        {{"id":1,"movieId":10,"date":"2026-01-01T00:00:00Z","eventType":"downloadFolderImported","downloadId":"{a}"}},
        {{"id":2,"movieId":10,"date":"2026-06-01T00:00:00Z","eventType":"downloadFolderImported","downloadId":"{b}"}},
        {{"id":3,"seriesId":4,"episodeId":41,"episode":{{"seasonNumber":1}},"date":"2026-02-01T00:00:00Z","eventType":"downloadFolderImported","downloadId":"{c}"}},
        {{"id":4,"seriesId":4,"episodeId":42,"episode":{{"seasonNumber":1}},"date":"2026-02-01T00:00:00Z","eventType":"downloadFolderImported","downloadId":"SABnzbd_nzo_x"}},
        {{"id":5,"movieId":11,"date":"2026-02-01T00:00:00Z","eventType":"grabbed","downloadId":"{a}"}}
    ]"#,
        a = hash('A'),
        b = hash('B'),
        c = hash('C')
    );
    let records: Vec<HistoryRecord> = serde_json::from_str(&rows).expect("records parse");
    let downloads = map::downloads(&records, "");
    assert_eq!(map::downloads(&records, "4k")["radarr@4k-10"], [hash('b')], "a named instance's cards are its own");
    assert_eq!(downloads["radarr-10"], [hash('b')], "the upgrade's torrent, lower-cased; the replaced file's is gone");
    assert_eq!(downloads["sonarr-4-s1"], [hash('c'), "sabnzbd_nzo_x".to_string()], "every episode's own download");
    assert!(!downloads.contains_key("radarr-11"), "a grab moved no file");
}

fn listed(torrents: Vec<Torrent>) -> Vec<Result<Vec<Torrent>, TorrentError>> {
    vec![Ok(torrents)]
}

#[test]
fn history_ties_first_and_a_folder_match_covers_cards_history_does_not() {
    let in_library = Torrent { hash: hash('d'), content_path: "/media/movies/Kept (2020)".into(), ..torrent(1.0, 1) };
    let listings = listed(vec![Torrent { hash: hash('a'), ..torrent(0.2, 1) }, in_library]);
    let downloads =
        BTreeMap::from([("radarr-1".to_string(), vec![hash('a'), "sabnzbd_nzo_x".into()]), ("radarr-2".to_string(), vec![hash('e')])]);
    let folders = HashMap::from([
        ("radarr-1".to_string(), "/media/movies/One (2001)".to_string()),
        ("radarr-2".to_string(), "/media/movies/Gone (2002)".to_string()),
        ("radarr-3".to_string(), "/media/movies/Kept (2020)/".to_string()),
    ]);
    let matches = map::match_cards(&downloads, &folders, &listings, &TorrentsConfig::default());
    let found = |card: &str| matches.cards.get(card).map(|list| list.iter().map(|(_, t, how)| (t.hash.clone(), *how)).collect::<Vec<_>>());
    assert_eq!(found("radarr-1"), Some(vec![(hash('a'), MatchedBy::History)]));
    assert_eq!(found("radarr-2"), None, "its torrent is gone from a client that answered");
    assert_eq!(found("radarr-3"), Some(vec![(hash('d'), MatchedBy::Path)]));
    assert!(matches.unreadable.is_empty());
}

#[test]
fn a_torrent_missing_while_a_client_is_unreadable_may_still_be_seeding() {
    let listings = vec![Ok(Vec::new()), Err(TorrentError::Login { client: ClientKind::Transmission })];
    let downloads = BTreeMap::from([("radarr-1".to_string(), vec![hash('a')]), ("radarr-2".to_string(), vec!["sabnzbd_nzo_x".into()])]);
    let matches = map::match_cards(&downloads, &HashMap::new(), &listings, &TorrentsConfig::default());
    assert_eq!(matches.unreadable, HashSet::from(["radarr-1".to_string()]), "a usenet id is no torrent's");
}

/// A scratch directory: `downloads/` and `library/` side by side.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("flinch-torrents-{}-{name}", std::process::id()));
        std::fs::create_dir_all(root.join("downloads")).expect("downloads");
        std::fs::create_dir_all(root.join("library/Movie (2020)")).expect("library");
        Self(root)
    }

    fn path(&self, relative: &str) -> String {
        self.0.join(relative).to_string_lossy().into_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
fn a_shared_inode_with_the_library_file_is_a_hardlink_and_a_copy_is_not() {
    let scratch = Scratch::new("links");
    let (linked, copied) = (scratch.path("downloads/linked.mkv"), scratch.path("downloads/copied.mkv"));
    std::fs::write(&linked, b"video").expect("torrent file");
    std::fs::hard_link(&linked, scratch.path("library/Movie (2020)/Movie.mkv")).expect("import by hardlink");
    std::fs::write(&copied, b"video").expect("torrent file");
    std::fs::copy(&copied, scratch.path("library/Movie (2020)/Copy.mkv")).expect("import by copy");
    let folder = scratch.path("library/Movie (2020)");
    let config = TorrentsConfig::default();
    let mut links = LibraryLinks::default();

    let files = [linked.clone(), copied.clone()];
    assert_eq!(map::hardlinks(Some(&files), &folder, &config, &mut links), (vec![linked], true));
    let missing = [scratch.path("downloads/not-mounted.mkv")];
    assert_eq!(map::hardlinks(Some(&missing), &folder, &config, &mut links), (Vec::new(), false), "unreadable: unverified");
    assert_eq!(map::hardlinks(None, &folder, &config, &mut links), (Vec::new(), false), "files unlisted: unverified");
}

#[test]
fn client_paths_are_read_through_the_path_map() {
    let scratch = Scratch::new("mapped");
    let file = scratch.path("downloads/m.mkv");
    std::fs::write(&file, b"video").expect("torrent file");
    std::fs::hard_link(&file, scratch.path("library/Movie (2020)/m.mkv")).expect("hardlink");
    let config = TorrentsConfig {
        path_map: vec![
            super::super::PathMap { from: "/data/torrents".into(), to: scratch.path("downloads") },
            super::super::PathMap { from: "/movies".into(), to: scratch.path("library") },
        ],
        ..TorrentsConfig::default()
    };
    let client_files = ["/data/torrents/m.mkv".to_string()];
    let (linked, verified) = map::hardlinks(Some(&client_files), "/movies/Movie (2020)", &config, &mut LibraryLinks::default());
    assert_eq!((linked, verified), (vec!["/data/torrents/m.mkv".to_string()], true));
}

fn held(meets_goal: bool, linked: bool, verified: bool, cards: usize) -> Holding {
    Holding {
        hash: hash('a'),
        client: ClientKind::Qbittorrent,
        client_index: 0,
        ratio: 0.42,
        seeding_secs: 3 * 86_400 + 5,
        meets_goal,
        hardlinked_paths: if linked { vec!["/dl/m.mkv".into()] } else { Vec::new() },
        links_verified: verified,
        cards: (0..cards).map(|n| format!("sonarr-1-s{n}")).collect(),
    }
}

const BELOW: SeedHold = SeedHold::BelowGoal { ratio_centi: 42, seeding_days: 3 };

#[rstest]
#[case::below_its_goal(held(false, true, true, 1), true, true, Some(BELOW))]
#[case::below_but_goals_not_respected_and_not_linked(held(false, false, true, 1), false, false, None)]
#[case::linked_torrent_that_stays(held(true, true, true, 1), true, false, Some(SeedHold::HeldByTorrent))]
#[case::linked_torrent_that_goes_with_it(held(true, true, true, 1), true, true, None)]
#[case::linked_pack_holding_other_seasons(held(true, true, true, 2), true, true, Some(SeedHold::HeldByTorrent))]
#[case::a_copy_frees_its_bytes(held(true, false, true, 1), true, false, None)]
#[case::links_unread(held(true, false, false, 1), true, false, Some(SeedHold::LinksUnverified))]
#[case::links_unread_but_the_torrent_goes(held(true, false, false, 1), true, true, None)]
fn a_card_is_held_while_its_torrent_seeds_toward_a_goal_or_keeps_its_bytes(
    #[case] holding: Holding,
    #[case] respect: bool,
    #[case] torrent_goes: bool,
    #[case] expected: Option<SeedHold>,
) {
    let config = TorrentsConfig { respect_seed_goals: respect, ..TorrentsConfig::default() };
    let holdings = HashMap::from([("card".to_string(), vec![holding])]);
    let holds = map::holds(&holdings, &HashSet::new(), &config, torrent_goes);
    assert_eq!(holds.get("card").copied(), expected);
}

#[test]
fn an_unreadable_client_holds_and_the_status_counts_every_hold_once() {
    let holdings = HashMap::from([("seeding".to_string(), vec![held(false, false, true, 1)])]);
    let unreadable = HashSet::from(["unknown".to_string()]);
    let holds = map::holds(&holdings, &unreadable, &TorrentsConfig::default(), false);
    assert_eq!(holds.get("unknown"), Some(&SeedHold::ClientUnreadable));
    let mut status = map::TorrentStatus::default();
    status.count(&holds, |card| if card == "seeding" { 10 } else { 5 });
    assert_eq!((status.below_goal.items, status.below_goal.bytes), (1, 10));
    assert_eq!((status.client_unreadable.items, status.client_unreadable.bytes), (1, 5));
}

#[rstest]
#[case::off(0.0, false)]
#[case::below_the_desired_ratio(2.0, true)]
#[case::past_the_desired_ratio(0.4, false)]
fn a_torrent_past_its_goal_but_below_the_desired_ratio_spares_its_card(#[case] desired: f64, #[case] spared: bool) {
    let config = TorrentsConfig { prefer_after_ratio: desired, ..TorrentsConfig::default() };
    // Ratio 0.42: "met" holds a goal-met copy; "kept" is below its goal.
    let holdings =
        HashMap::from([("met".to_string(), vec![held(true, false, true, 1)]), ("kept".to_string(), vec![held(false, false, true, 1)])]);
    let holds = map::holds(&holdings, &HashSet::new(), &config, false);
    let found = map::spared(&holdings, &holds, &config);
    assert_eq!(found.contains("met"), spared);
    assert!(!found.contains("kept"), "a card a hold keeps is kept, not merely spared");
}
