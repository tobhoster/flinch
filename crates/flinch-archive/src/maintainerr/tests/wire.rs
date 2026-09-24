//! Request bodies, response semantics and collection validation, as tables.

use super::super::validate::{for_section, handover, resolve, validate_collection};
use super::super::wire::{collection_add_request, exclusion_request, read_accepted, read_json, read_return_status, read_version, ADD_EXCLUSION, ADD_MEMBER};
use super::super::{
    CollectionProblem, CollectionTitles, ExclusionRow, Handover, MaintainerrError, MaintainerrTarget, MaintainerrVersion, Route,
};
use super::{collection, leaving, movie, season, titles, valid_collections};
use crate::card::LibraryKind;
use rstest::rstest;
use serde_json::{json, Value};

#[rstest]
#[case::movie(
    movie("100"),
    json!({"mediaId": "100"}),
    json!({"action": 0, "collectionId": 7, "mediaId": "100", "context": {"type": "movie", "id": "100"}})
)]
#[case::season(
    season("200", "201"),
    json!({"mediaId": "200", "context": {"type": "season", "id": "201"}}),
    json!({"action": 0, "collectionId": 7, "mediaId": "200", "context": {"type": "season", "id": "201"}})
)]
fn request_bodies_carry_plex_rating_keys_and_the_season_context(
    #[case] target: MaintainerrTarget,
    #[case] exclusion: Value,
    #[case] add: Value,
) {
    assert_eq!(serde_json::to_value(exclusion_request(&target)).unwrap(), exclusion);
    assert_eq!(serde_json::to_value(collection_add_request(7, &target)).unwrap(), add);
}

/// The kind of result, so a table can state it.
fn verdict(result: Result<(), MaintainerrError>) -> String {
    match result {
        Ok(()) => "ok".to_string(),
        Err(MaintainerrError::Refused { code, message, .. }) => format!("refused {code}: {message}"),
        Err(MaintainerrError::Http { status, message, .. }) => format!("http {status}: {message}"),
        Err(MaintainerrError::Parse { .. }) => "parse".to_string(),
        Err(MaintainerrError::Transport { .. }) => "transport".to_string(),
    }
}

#[rstest]
#[case::code_1_is_success(201, r#"{"code":1,"result":"Success","message":"Success"}"#, "ok")]
#[case::code_0_over_201_is_a_failure(
    201,
    r#"{"code":0,"result":"Failed - no metadata","message":"Failed - no metadata"}"#,
    "refused 0: Failed - no metadata"
)]
#[case::locked(409, r#"{"statusCode":409,"message":"Collection handling is already running."}"#, "http 409: Collection handling is already running.")]
#[case::empty_body_is_not_success(201, "", "parse")]
fn exclusion_success_needs_a_2xx_and_code_1(#[case] status: u16, #[case] body: &str, #[case] expected: &str) {
    assert_eq!(verdict(read_return_status(ADD_EXCLUSION, status, body)), expected);
}

#[rstest]
#[case::created(201, r#"{"id":7,"title":"FLINCH Movies"}"#, "ok")]
#[case::wrong_kind(
    400,
    r#"{"statusCode":400,"message":"This item cannot be applied to the selected collection","error":"Bad Request"}"#,
    "http 400: This item cannot be applied to the selected collection"
)]
#[case::rejected_body(400, r#"{"statusCode":400,"message":["action: Required","context: Required"]}"#, "http 400: action: Required; context: Required")]
#[case::no_collection(404, r#"{"statusCode":404,"message":"Collection 7 not found"}"#, "http 404: Collection 7 not found")]
#[case::plex_refused(502, r#"{"statusCode":502,"message":"The media server refused 1 of 1 item(s)"}"#, "http 502: The media server refused 1 of 1 item(s)")]
fn collection_add_success_is_a_2xx_and_failures_carry_the_message(#[case] status: u16, #[case] body: &str, #[case] expected: &str) {
    assert_eq!(verdict(read_accepted(ADD_MEMBER, status, body)), expected);
}

#[test]
fn an_unreadable_exclusion_list_is_an_error_never_an_empty_list() {
    let read = |body: &str| read_json::<Vec<ExclusionRow>>("GET /api/rules/exclusion", 200, body);
    assert!(matches!(read(""), Err(MaintainerrError::Parse { .. })), "Maintainerr answers an empty body when its query failed");
    assert!(matches!(read(r#"{"rows":[]}"#), Err(MaintainerrError::Parse { .. })));
    assert_eq!(read("[]").unwrap(), Vec::<ExclusionRow>::new());
    let rows = read(r#"[{"id":3,"mediaServerId":4511,"ruleGroupId":null,"parent":"4500","type":"season"}]"#).unwrap();
    assert_eq!(rows[0].media_server_id, "4511", "a numeric ratingKey reads as the same key");
}

#[rstest]
#[case::release("3.29.0", MaintainerrVersion::Release { major: 3, minor: 29, patch: 0 })]
#[case::prefixed("v3.9.1", MaintainerrVersion::Release { major: 3, minor: 9, patch: 1 })]
#[case::prerelease("3.10.0-beta.1", MaintainerrVersion::Release { major: 3, minor: 10, patch: 0 })]
#[case::branch("main-bd8a1e0", MaintainerrVersion::Branch("main-bd8a1e0".to_string()))]
fn versions_parse(#[case] raw: &str, #[case] expected: MaintainerrVersion) {
    assert_eq!(MaintainerrVersion::parse(raw), expected);
}

#[rstest]
#[case::object(r#"{"status":1,"version":"3.29.0","commitTag":"","updateAvailable":false}"#)]
#[case::encoded_text(r#""{\"status\":1,\"version\":\"3.29.0\"}""#)]
fn the_status_body_yields_the_version(#[case] body: &str) {
    assert_eq!(read_version(200, body).unwrap(), MaintainerrVersion::Release { major: 3, minor: 29, patch: 0 });
}

#[rstest]
#[case::current("3.29.0", true)]
#[case::first_validated_release("3.10.0", true)]
#[case::older("3.9.9", false)]
#[case::v2("2.19.0", false)]
#[case::status_failure_placeholder("0.0.1", false)]
#[case::branch_build("development-bd8a1e0", true)]
fn handover_needs_maintainerr_3_10(#[case] raw: &str, #[case] allowed: bool) {
    assert_eq!(matches!(handover(&MaintainerrVersion::parse(raw)), Handover::Allowed { .. }), allowed);
}

#[rstest]
#[case::movie_delete("movie", true, 0, LibraryKind::Movie, Ok(()))]
#[case::movie_unmonitor_delete_all("movie", true, 1, LibraryKind::Movie, Ok(()))]
#[case::movie_type_case_insensitive("Movie", true, 0, LibraryKind::Movie, Ok(()))]
#[case::movie_cannot_delete_existing_only("movie", true, 2, LibraryKind::Movie, Err(CollectionProblem::ArrAction { found: 2 }))]
#[case::season_delete_existing("season", true, 2, LibraryKind::Season, Ok(()))]
#[case::season_delete_show_if_empty("season", true, 5, LibraryKind::Season, Ok(()))]
#[case::unmonitor_frees_nothing("season", true, 3, LibraryKind::Season, Err(CollectionProblem::ArrAction { found: 3 }))]
#[case::do_nothing_frees_nothing("movie", true, 4, LibraryKind::Movie, Err(CollectionProblem::ArrAction { found: 4 }))]
#[case::inactive("movie", false, 0, LibraryKind::Movie, Err(CollectionProblem::Inactive))]
#[case::show_collection_for_seasons("show", true, 0, LibraryKind::Season, Err(CollectionProblem::WrongType { found: "show".to_string() }))]
#[case::season_collection_for_movies("season", true, 0, LibraryKind::Movie, Err(CollectionProblem::WrongType { found: "season".to_string() }))]
fn collection_validation(
    #[case] media_type: &str,
    #[case] is_active: bool,
    #[case] arr_action: i64,
    #[case] kind: LibraryKind,
    #[case] expected: Result<(), CollectionProblem>,
) {
    let candidate = collection(1, "FLINCH", media_type, "1", is_active, arr_action);
    assert_eq!(validate_collection(&candidate, kind, Route::Delete), expected);
}

/// Leaving Soon is a warning or nothing: a collection that acts on
/// Maintainerr's next run, or that Plex does not show, warns nobody.
#[rstest]
#[case::warns(Some(14), true, false, false, Ok(()))]
#[case::recommended_is_shown_too(Some(7), false, true, false, Ok(()))]
#[case::no_window(None, true, false, false, Err(CollectionProblem::NoWarningWindow))]
#[case::zero_window(Some(0), true, false, false, Err(CollectionProblem::NoWarningWindow))]
#[case::hidden(Some(14), false, false, false, Err(CollectionProblem::NotShownInPlex))]
#[case::maintainerr_only(Some(14), true, false, true, Err(CollectionProblem::NotShownInPlex))]
fn a_leaving_soon_collection_must_warn(
    #[case] window: Option<i64>,
    #[case] home: bool,
    #[case] recommended: bool,
    #[case] maintainerr_only: bool,
    #[case] expected: Result<(), CollectionProblem>,
) {
    let candidate = super::super::CollectionInfo {
        delete_after_days: window,
        visible_on_home: home,
        visible_on_recommended: recommended,
        keep_in_maintainerr_only: maintainerr_only,
        ..leaving(1, "movie", "1")
    };
    assert_eq!(validate_collection(&candidate, LibraryKind::Movie, Route::LeavingSoon), expected);
    assert_eq!(validate_collection(&candidate, LibraryKind::Movie, Route::Delete), Ok(()), "a delete route never needs a window");
}

#[test]
fn one_leaving_soon_title_resolves_each_kind_to_its_own_library() {
    let titles = CollectionTitles { leaving: "Leaving Soon".to_string(), ..titles() };
    let ids = |found: Vec<&super::super::CollectionInfo>| found.iter().map(|c| c.id).collect::<Vec<_>>();
    let both = [leaving(50, "movie", "1"), leaving(51, "season", "2")];
    assert_eq!(resolve(&both, &titles, LibraryKind::Movie, Route::LeavingSoon).map(ids), Ok(vec![50]));
    assert_eq!(resolve(&both, &titles, LibraryKind::Season, Route::LeavingSoon).map(ids), Ok(vec![51]));

    // The TV one made with the show type is wrong for seasons, never for movies.
    let show_typed = [leaving(50, "movie", "1"), leaving(51, "show", "2")];
    assert!(resolve(&show_typed, &titles, LibraryKind::Movie, Route::LeavingSoon).is_ok());
    let wrong = resolve(&show_typed, &titles, LibraryKind::Season, Route::LeavingSoon).unwrap_err();
    assert_eq!(wrong[0].problem, CollectionProblem::WrongType { found: "show".to_string() });
}

#[test]
fn a_missing_title_or_any_invalid_namesake_makes_the_kind_misconfigured() {
    let missing = resolve(&valid_collections()[..1], &titles(), LibraryKind::Season, Route::Delete).unwrap_err();
    assert_eq!(missing[0].problem, CollectionProblem::NotFound);

    let mut collections = valid_collections();
    collections.push(collection(30, "flinch movies", "movie", "3", false, 0));
    let invalid = resolve(&collections, &titles(), LibraryKind::Movie, Route::Delete).unwrap_err();
    assert_eq!((invalid[0].collection_id, &invalid[0].problem), (Some(30), &CollectionProblem::Inactive));
}

#[rstest]
#[case::bound_section(Some(1), Some(10))]
#[case::other_section(Some(9), None)]
#[case::unknown_section_single_collection(None, Some(10))]
fn the_collection_must_be_bound_to_the_items_section(#[case] section: Option<u32>, #[case] expected: Option<i64>) {
    let collections = valid_collections();
    let movies = resolve(&collections, &titles(), LibraryKind::Movie, Route::Delete).unwrap();
    assert_eq!(for_section(&movies, section).map(|c| c.id), expected);
}

#[test]
fn an_unknown_section_with_several_collections_is_ambiguous() {
    let mut collections = valid_collections();
    collections.push(collection(11, "FLINCH Movies", "movie", "3", true, 0));
    let movies = resolve(&collections, &titles(), LibraryKind::Movie, Route::Delete).unwrap();
    assert_eq!(for_section(&movies, None), None);
    assert_eq!(for_section(&movies, Some(3)).map(|c| c.id), Some(11));
}
