use super::*;
use crate::card::LibraryKind;
use crate::ids::ExternalIds;
use crate::jellyfin::{evidence, JellyfinItem, UserItems};
use crate::plex::WatchTarget;
use crate::watch_sources::{Played, SourcePlay};
use rstest::rstest;

fn ignoring(names: &[&str]) -> IgnoredViewers {
    IgnoredViewers::new(&names.iter().map(|name| name.to_string()).collect::<Vec<_>>())
}

fn plex_row(account: u64) -> PlexMetadata {
    serde_json::from_value(serde_json::json!({"ratingKey": "5", "accountID": account, "viewedAt": 100})).expect("plex row")
}

fn tautulli_row(user: &str) -> TautulliRow {
    serde_json::from_value(serde_json::json!({"user": user, "user_id": "9", "date": "100"})).expect("tautulli row")
}

#[test]
fn plex_history_rows_go_by_account_name_and_an_unnamed_account_stays() {
    let accounts = HashMap::from([(1, "Owner".to_string()), (7, "Guest".to_string())]);
    let mut rows = vec![plex_row(1), plex_row(7), plex_row(8)];
    assert_eq!(ignoring(&[" guest "]).plex(&mut rows, &accounts), 1);
    assert_eq!(
        rows.iter().map(|row| row.account_id).collect::<Vec<_>>(),
        vec![Some(1), Some(8)],
        "account 8 has no known name: its play counts"
    );
}

#[test]
fn tautulli_streams_go_by_user() {
    let mut rows = vec![tautulli_row("Kids"), tautulli_row("Ann")];
    assert_eq!(ignoring(&["kids"]).tautulli(&mut rows), 1);
    assert_eq!(rows[0].user, "Ann");
}

fn movie(id: &str, tmdb: u32) -> WatchTarget {
    WatchTarget {
        id: id.into(),
        kind: LibraryKind::Movie,
        title: "anything".into(),
        year: None,
        show_title: None,
        season_index: None,
        episodes_total: None,
        episode_files: None,
        episodes_on_disk: None,
        external: ExternalIds { tmdb: Some(tmdb), ..ExternalIds::default() },
        added_epoch: None,
        on_disk: true,
    }
}

fn jellyfin(complete: bool) -> JellyfinRead {
    let played: Vec<JellyfinItem> = serde_json::from_str(
        r#"[{"Id":"m1","Type":"Movie","ProviderIds":{"Tmdb":"603"},"UserData":{"Played":true,"LastPlayedDate":"2024-01-01T00:00:00Z"}}]"#,
    )
    .expect("items");
    let unplayed: Vec<JellyfinItem> = serde_json::from_str(r#"[{"Id":"m1","Type":"Movie","ProviderIds":{"Tmdb":"603"}}]"#).expect("items");
    let users = vec![
        UserItems { user_id: "u1".into(), name: "Guest".into(), items: played },
        UserItems { user_id: "u2".into(), name: "Ann".into(), items: unplayed },
    ];
    JellyfinRead { users, complete, problems: Vec::new() }
}

#[rstest]
#[case::complete_record(true, Some(0.0))]
#[case::incomplete_record(false, None)]
fn an_ignored_users_play_counts_as_none_and_the_records_health_stays_as_read(#[case] complete: bool, #[case] progress: Option<f32>) {
    let targets = [movie("radarr-1", 603)];
    let mut read = jellyfin(complete);
    assert_eq!(ignoring(&["guest"]).jellyfin(&mut read), 1);
    assert_eq!(read.complete, complete, "setting a viewer aside is not a partial read");
    // Complete: nobody else played it, a whole-record zero. Incomplete: no
    // claim either way, as without the ignore list.
    let found = evidence(&targets, &read);
    assert_eq!(found.entries.get("radarr-1").map(|entry| entry.progress), progress);
    assert!(found.plays.get("radarr-1").is_none_or(|plays| plays.item.is_empty()));
}

fn source_play(viewer: Viewer) -> SourcePlay {
    SourcePlay { epoch: 100, viewer, played: Played::Movie(ExternalIds { tmdb: Some(603), ..ExternalIds::default() }), fraction: 1.0 }
}

#[test]
fn tracearr_plays_go_by_username_and_trakt_by_the_sources_name_while_coverage_stays() {
    let mut read = SourceRead {
        plays: vec![
            source_play(Viewer::TracearrUser("1".into())),
            source_play(Viewer::TracearrUser("2".into())),
            source_play(Viewer::TracearrUser("3".into())),
            source_play(Viewer::TraktUser("Guest".into())),
        ],
        epochs: vec![100, 100, 100, 100],
        usernames: HashMap::from([("1".to_string(), "guest".to_string()), ("2".to_string(), "ann".to_string())]),
        ..SourceRead::default()
    };
    assert_eq!(ignoring(&["Guest"]).source(&mut read), 2);
    let left: Vec<&Viewer> = read.plays.iter().map(|play| &play.viewer).collect();
    assert_eq!(left, vec![&Viewer::TracearrUser("2".into()), &Viewer::TracearrUser("3".into())]);
    assert_eq!(read.epochs.len(), 4);
}

#[rstest]
#[case::blank(vec!["  ".to_string()])]
#[case::too_long(vec!["x".repeat(101)])]
#[case::too_many((0..51).map(|n| n.to_string()).collect())]
fn an_ignore_list_the_page_would_refuse_is_invalid(#[case] names: Vec<String>) {
    assert!(validate(&names).is_err());
}
