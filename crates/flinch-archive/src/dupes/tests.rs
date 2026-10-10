use super::decisions::{settle, Decisions};
use super::group::{group, resolution, MovieIn, PlexVersion};
use super::pick::Advice;
use super::remove::{plan, Clients, Removal};
use super::{ArrFile, Group, KeepPreference};
use crate::card::LibraryKind;
use crate::ids::PlexIds;
use crate::maintainerr::SyncItem;
use rstest::rstest;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;

const GIB: u64 = 1 << 30;

fn version(rating_key: &str, section: u32, media_id: u64, plex_resolution: &str, file: &str, gib: u64, plays: u32) -> PlexVersion {
    PlexVersion {
        rating_key: rating_key.into(),
        section_id: Some(section),
        media_id,
        // As Plex sends it (`4k`, `1080`), read as PlexVersion::of reads it.
        resolution: resolution(plex_resolution),
        files: vec![(file.into(), gib * GIB)],
        plays,
    }
}

fn radarr_file(instance: &str, movie_id: u32, file_id: u32, path: &str, gib: u64) -> ArrFile {
    ArrFile { instance: instance.into(), movie_id, file_id, path: path.into(), bytes: gib * GIB, quality: Some("Bluray-1080p".into()) }
}

fn heat(plex: Vec<PlexVersion>, arr: Vec<ArrFile>) -> MovieIn {
    MovieIn { card_id: Some("radarr-7".into()), title: "Heat".into(), year: Some(1995), tmdb: Some(949), plex, arr }
}

fn only(groups: Vec<Group>) -> Group {
    assert_eq!(groups.len(), 1, "one group: {groups:?}");
    groups.into_iter().next().unwrap_or_else(|| unreachable!())
}

/// A 4K copy in "4K Movies" (section 2) and Radarr's 1080p file in "Movies".
fn two_sections(plays_4k: u32, plays_hd: u32) -> MovieIn {
    heat(
        vec![
            version("100", 1, 11, "1080", "/data/movies/Heat (1995)/Heat.mkv", 12, plays_hd),
            version("500", 2, 51, "4k", "/data/4k/Heat (1995)/Heat.2160p.mkv", 60, plays_4k),
        ],
        // Radarr mounts the store elsewhere: matched by name and size.
        vec![radarr_file("radarr", 7, 70, "/movies/Heat (1995)/Heat.mkv", 12)],
    )
}

#[rstest]
#[case::arr_file_wins_over_picture(two_sections(0, 0), KeepPreference::Highest, Advice::None, "plex:100:11")]
#[case::arr_file_wins_over_plays(two_sections(3, 0), KeepPreference::Highest, Advice::None, "plex:100:11")]
fn the_copy_radarr_tracks_is_kept(#[case] movie: MovieIn, #[case] prefer: KeepPreference, #[case] advice: Advice, #[case] keep: &str) {
    let advice = HashMap::from([("radarr-7".to_string(), advice)]);
    let found = only(group(vec![movie], prefer, &advice));
    assert_eq!(found.recommended, keep);
    assert_eq!(found.redundant_bytes, 60 * GIB);
    assert!(found.reasons[0].contains("radarr tracks this copy"), "{:?}", found.reasons);
}

/// Two versions Plex holds outside any *arr, in one item.
fn versions(plays_4k: u32) -> MovieIn {
    MovieIn {
        card_id: None,
        title: "Ronin".into(),
        year: Some(1998),
        tmdb: Some(8195),
        plex: vec![
            version("300", 1, 31, "1080", "/data/movies/Ronin/Ronin.1080p.mkv", 10, 0),
            version("300", 1, 32, "4k", "/data/movies/Ronin/Ronin.2160p.mkv", 50, plays_4k),
        ],
        arr: Vec::new(),
    }
}

#[rstest]
#[case::highest_picture(versions(0), KeepPreference::Highest, "plex:300:32")]
#[case::hd_intent(versions(0), KeepPreference::Hd, "plex:300:31")]
fn intent_decides_between_versions(#[case] movie: MovieIn, #[case] prefer: KeepPreference, #[case] keep: &str) {
    assert_eq!(only(group(vec![movie], prefer, &HashMap::new())).recommended, keep);
}

#[test]
fn the_played_copy_is_kept_over_the_better_picture() {
    let movie = MovieIn {
        plex: vec![version("300", 1, 31, "1080", "/m/Ronin.1080p.mkv", 10, 4), version("301", 2, 41, "4k", "/k/Ronin.2160p.mkv", 50, 0)],
        ..versions(0)
    };
    let found = only(group(vec![movie], KeepPreference::Highest, &HashMap::new()));
    assert_eq!(found.recommended, "plex:300:31");
    assert!(found.reasons[0].contains("played"), "{:?}", found.reasons);
}

#[test]
fn downgrade_advice_prefers_hd() {
    let movie = MovieIn { card_id: Some("radarr-9".into()), ..versions(0) };
    let advice = HashMap::from([("radarr-9".to_string(), Advice::Downgrade)]);
    assert_eq!(only(group(vec![movie], KeepPreference::Highest, &advice)).recommended, "plex:300:31");
}

#[test]
fn copies_sharing_a_file_are_one_copy() {
    // One folder in two libraries: removing either Media would remove both.
    let movie = MovieIn {
        plex: vec![version("300", 1, 31, "1080", "/m/Ronin.mkv", 10, 0), version("301", 2, 41, "1080", "/m/Ronin.mkv", 10, 0)],
        ..versions(0)
    };
    assert!(group(vec![movie], KeepPreference::Highest, &HashMap::new()).is_empty());
}

#[test]
fn instances_merge_by_tmdb_id() {
    let hd = MovieIn { plex: Vec::new(), arr: vec![radarr_file("radarr", 7, 70, "/movies/Heat.mkv", 12)], ..heat(Vec::new(), Vec::new()) };
    let uhd =
        MovieIn { card_id: None, plex: Vec::new(), arr: vec![radarr_file("radarr@4k", 3, 30, "/4k/Heat.2160p.mkv", 60)], ..hd.clone() };
    let found = only(group(vec![hd, uhd], KeepPreference::Highest, &HashMap::new()));
    assert_eq!(found.id, "tmdb:949");
    assert_eq!(found.copies.iter().map(|copy| copy.id.as_str()).collect::<Vec<_>>(), ["radarr:7:70", "radarr@4k:3:30"]);
}

#[test]
fn an_arr_file_matching_no_plex_copy_holds_the_group() {
    let movie = heat(
        vec![version("100", 1, 11, "1080", "/a/Heat.mkv", 12, 0), version("500", 2, 51, "4k", "/b/Heat.2160p.mkv", 60, 0)],
        vec![radarr_file("radarr", 7, 70, "/movies/Heat.renamed.mkv", 13)],
    );
    let found = only(group(vec![movie], KeepPreference::Highest, &HashMap::new()));
    assert!(found.held.is_some());
}

/// `group` with a confirmed choice of `keep`.
fn confirmed(mut group: Group, keep: &str) -> Group {
    let mut decisions = Decisions::default();
    decisions.choose(&group, Some(keep), false, 1).unwrap_or_else(|error| panic!("{error}"));
    decisions.choose(&group, Some(keep), true, 2).unwrap_or_else(|error| panic!("{error}"));
    decisions.attach(std::slice::from_mut(&mut group));
    group
}

#[test]
fn confirming_needs_a_choice_first() {
    let group = only(group(vec![versions(0)], KeepPreference::Highest, &HashMap::new()));
    let mut decisions = Decisions::default();
    assert!(decisions.choose(&group, Some("plex:300:31"), true, 1).is_err());
    assert!(decisions.choose(&group, Some("plex:999:1"), false, 1).is_err());
}

#[rstest]
#[case::plex_only_copy_goes_through_plex("plex:100:11", false, Ok(vec![Removal::PlexMedia { rating_key: "500".into(), media_id: 51 }]))]
#[case::radarrs_own_file_is_never_removed("plex:500:51", false, Err("radarr tracks"))]
#[case::a_pinned_item_keeps_its_copies("plex:100:11", true, Err("pinned"))]
fn removals_follow_the_owner(#[case] keep: &str, #[case] protected: bool, #[case] expected: Result<Vec<Removal>, &str>) {
    let group = confirmed(only(group(vec![two_sections(0, 0)], KeepPreference::Highest, &HashMap::new())), keep);
    match (plan(&group, protected), expected) {
        (Ok(removals), Ok(expected)) => assert_eq!(removals.into_iter().map(|(_, removal)| removal).collect::<Vec<_>>(), expected),
        (Err(held), Err(expected)) => assert!(held.contains(expected), "{held}"),
        (got, expected) => panic!("{got:?} vs {expected:?}"),
    }
}

#[test]
fn a_choice_made_before_a_copy_appeared_is_not_acted_on() {
    let mut found = confirmed(only(group(vec![versions(0)], KeepPreference::Highest, &HashMap::new())), "plex:300:31");
    found.copies.push(super::Copy { id: "plex:302:1".into(), ..found.copies[0].clone() });
    assert!(found.confirmed().is_none());
    assert_eq!(plan(&found, false), Ok(Vec::new()));
}

fn sync_item() -> SyncItem {
    SyncItem {
        card_id: "radarr-7".into(),
        kind: LibraryKind::Movie,
        plex: Some(PlexIds { rating_key: "100".into(), season_rating_key: None, section_id: Some(1) }),
        copies: vec!["100".into(), "500".into()],
        bytes: 12 * GIB,
    }
}

#[test]
fn a_confirmed_choice_settles_the_several_copies_hold() {
    let group = confirmed(only(group(vec![two_sections(0, 0)], KeepPreference::Highest, &HashMap::new())), "plex:500:51");
    let mut items = [sync_item()];
    assert_eq!(settle(&mut items, std::slice::from_ref(&group)), 1);
    assert_eq!(items[0].copies, ["500"]);
    assert_eq!(items[0].plex.as_ref().map(|ids| (ids.rating_key.as_str(), ids.section_id)), Some(("500", Some(2))));

    let unconfirmed = only(super::group::group(vec![two_sections(0, 0)], KeepPreference::Highest, &HashMap::new()));
    let mut untouched = [sync_item()];
    assert_eq!(settle(&mut untouched, &[unconfirmed]), 0);
    assert_eq!(untouched[0].copies, ["100", "500"]);
}

/// A fake Plex answering each request in turn; returns the request lines.
fn fake_plex(answers: Vec<(u16, String)>) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|error| panic!("{error}"));
    let base = format!("http://{}", listener.local_addr().unwrap_or_else(|error| panic!("{error}")));
    let handle = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for (status, body) in answers {
            let Ok((mut stream, _)) = listener.accept() else { break };
            let mut buffer = [0u8; 4096];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]).to_string();
            seen.push(request.lines().next().unwrap_or_default().to_string());
            let answer = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(answer.as_bytes()).ok();
        }
        seen
    });
    (base, handle)
}

fn media(ids: &[u64]) -> String {
    let media: Vec<String> = ids.iter().map(|id| format!(r#"{{"id":{id}}}"#)).collect();
    format!(r#"{{"MediaContainer":{{"Metadata":[{{"ratingKey":"300","Media":[{}]}}]}}}}"#, media.join(","))
}

#[rstest]
#[case::removed_and_the_kept_copy_remains(vec![(200, media(&[31, 32])), (200, media(&[31, 32])), (200, String::new()), (200, media(&[31])), (200, media(&[31]))], Ok(true))]
#[case::plex_refuses_media_deletion(vec![(200, media(&[31, 32])), (200, media(&[31, 32])), (401, String::new())], Err("Allow media deletion"))]
#[case::the_kept_copy_is_missing(vec![(200, media(&[32]))], Err("not there"))]
#[tokio::test]
async fn plex_removal_reads_back_the_kept_copy(#[case] answers: Vec<(u16, String)>, #[case] expected: Result<bool, &str>) {
    let (base, server) = fake_plex(answers);
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap_or_else(|error| panic!("{error}"));
    let found = only(group(vec![versions(0)], KeepPreference::Hd, &HashMap::new()));
    let keep = found.copy("plex:300:31").cloned().unwrap_or_else(|| unreachable!());
    let clients = Clients { http: &http, plex: Some((&base, "token")), arrs: &[], dry_run: false };
    let got = clients.remove(&keep, &Removal::PlexMedia { rating_key: "300".into(), media_id: 32 }).await;
    let seen = server.join().unwrap_or_default();
    match (got, expected) {
        (Ok(removed), Ok(expected)) => {
            assert_eq!(removed, expected);
            assert!(seen.contains(&"DELETE /library/metadata/300/media/32 HTTP/1.1".to_string()), "{seen:?}");
        }
        (Err(error), Err(expected)) => {
            assert!(error.to_string().contains(expected), "{error}");
            assert!(!seen.iter().skip(3).any(|line| line.starts_with("DELETE")), "nothing more after a refusal: {seen:?}");
        }
        (got, expected) => panic!("{got:?} vs {expected:?}"),
    }
}

#[tokio::test]
async fn a_dry_run_sends_no_delete() {
    let (base, server) = fake_plex(vec![(200, media(&[31, 32])), (200, media(&[31, 32]))]);
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap_or_else(|error| panic!("{error}"));
    let found = only(group(vec![versions(0)], KeepPreference::Hd, &HashMap::new()));
    let keep = found.copy("plex:300:31").cloned().unwrap_or_else(|| unreachable!());
    let clients = Clients { http: &http, plex: Some((&base, "token")), arrs: &[], dry_run: true };
    let got = clients.remove(&keep, &Removal::PlexMedia { rating_key: "300".into(), media_id: 32 }).await;
    let seen = server.join().unwrap_or_default();
    assert!(matches!(got, Ok(false)), "{got:?}");
    assert!(seen.iter().all(|line| line.starts_with("GET")), "{seen:?}");
}
