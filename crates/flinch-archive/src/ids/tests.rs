use super::*;
use rstest::rstest;

#[rstest]
#[case::movie("radarr-7", App::Radarr, "", 7, None)]
#[case::season("sonarr-11-s1", App::Sonarr, "", 11, Some(1))]
#[case::specials("sonarr-11-s0", App::Sonarr, "", 11, Some(0))]
#[case::show("sonarr-11", App::Sonarr, "", 11, None)]
#[case::named_movie("radarr@4k-7", App::Radarr, "4k", 7, None)]
#[case::named_season("sonarr@anime-11-s2", App::Sonarr, "anime", 11, Some(2))]
#[case::named_show("sonarr@anime_hd-11", App::Sonarr, "anime_hd", 11, None)]
fn ids_name_their_instance_and_item(
    #[case] text: &str,
    #[case] app: App,
    #[case] instance: &str,
    #[case] id: u32,
    #[case] season: Option<u32>,
) {
    let parsed = ArrRef::parse(text).expect("a well-formed id");
    assert_eq!(parsed, ArrRef { app, instance, id, season });
    assert_eq!(parsed.id_text(), text, "written back exactly as read");
}

#[rstest]
#[case::empty("")]
#[case::bare_app("radarr")]
#[case::no_number("radarr-")]
#[case::not_a_number("radarr-x")]
#[case::movie_with_season("radarr-7-s1")]
#[case::empty_name("radarr@-7")]
#[case::dash_in_name("sonarr@anime-hd-7-s1")]
#[case::upper_name("radarr@4K-7")]
#[case::path("radarr@../x-7")]
#[case::empty_season("sonarr-7-s")]
#[case::two_seasons("sonarr-7-s1-s2")]
#[case::other_app("lidarr-7")]
#[case::glued("radarrx-7")]
#[case::overflow("radarr-99999999999")]
fn malformed_ids_name_nothing(#[case] text: &str) {
    assert_eq!(ArrRef::parse(text), None);
}

#[test]
fn a_show_subject_is_not_a_card() {
    assert!(ArrRef::card("sonarr@anime-7").is_none());
    assert!(ArrRef::card("sonarr@anime-7-s1").is_some());
    assert_eq!(ArrRef::card("sonarr@anime-7-s1").map(|item| item.subject().id_text()), Some("sonarr@anime-7".to_string()));
}

#[test]
fn the_default_instance_keeps_todays_ids() {
    assert_eq!(movie_card_id(DEFAULT_INSTANCE, 7), "radarr-7");
    assert_eq!(season_card_id(DEFAULT_INSTANCE, 11, 2), "sonarr-11-s2");
    assert_eq!(show_subject(DEFAULT_INSTANCE, 11), "sonarr-11");
    assert_eq!(movie_card_id("4k", 7), "radarr@4k-7");
    assert_eq!(instance_key(App::Sonarr, "anime"), "sonarr@anime");
}

#[rstest]
#[case("4k", true)]
#[case("anime_2", true)]
#[case("", false)]
#[case("anime-hd", false)]
#[case("Anime", false)]
#[case("a/b", false)]
#[case("abcdefghijklmnopqrstuvwxy", false)]
fn instance_names_stay_id_and_file_safe(#[case] name: &str, #[case] valid: bool) {
    assert_eq!(valid_instance_name(name), valid);
}
