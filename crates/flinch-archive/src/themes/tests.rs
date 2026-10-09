use super::*;
use rstest::rstest;

const DAY: u64 = 86_400;
const NOW: u64 = 1_000 * DAY;
const GENRES: [&str; 4] = ["Documentary", "Animation", "Horror", "Western"];

/// `per_group` subjects around each of four orthogonal directions, nudged
/// apart, each group carrying its own genre.
fn library(per_group: usize) -> Vec<(String, Vec<f32>, Vec<String>)> {
    (0..4 * per_group)
        .map(|index| {
            let group = index % 4;
            let mut vector = vec![0.0f32; 8];
            vector[group] = 1.0;
            vector[4 + index % 4] += 0.05 * (index / 4) as f32 / per_group as f32;
            let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
            vector.iter_mut().for_each(|x| *x /= norm);
            (format!("radarr-{index}"), vector, vec![GENRES[group].to_string()])
        })
        .collect()
}

fn members(library: &[(String, Vec<f32>, Vec<String>)]) -> Vec<Member<'_>> {
    library.iter().map(|(subject, vector, genres)| Member { subject, vector, genres }).collect()
}

#[test]
fn the_same_vectors_give_the_same_themes_in_any_order() {
    let library = library(10);
    let forward = members(&library);
    let mut backward = forward.clone();
    backward.reverse();
    let themes = Themes::compute(&forward, NOW);
    assert_eq!(themes, Themes::compute(&backward, NOW));
    assert_eq!(themes, Themes::compute(&forward, NOW));
    assert_eq!(themes.fingerprint, fingerprint(&backward));
}

#[test]
fn separate_groups_become_separate_themes_named_by_their_genre() {
    let library = library(10);
    let themes = Themes::compute(&members(&library), NOW);
    assert_eq!(themes.names.len(), 4, "{:?}", themes.names);
    for (subject, _, genres) in &library {
        assert_eq!(themes.name_of(subject), Some(genres[0].as_str()), "{subject}");
    }
}

#[test]
fn too_few_subjects_make_no_themes() {
    let library = library(3);
    let themes = Themes::compute(&members(&library), NOW);
    assert!(themes.names.is_empty() && themes.assignments.is_empty());
    assert_eq!(themes.computed_at, NOW, "an empty result is still current, not recomputed every cycle");
}

#[test]
fn themes_are_recomputed_daily_or_when_a_vector_changes() {
    let mut library = library(10);
    let themes = Themes::compute(&members(&library), NOW);
    assert!(themes.is_current(fingerprint(&members(&library)), NOW + DAY - 1));
    assert!(!themes.is_current(fingerprint(&members(&library)), NOW + DAY));
    library[0].1[7] = 0.5;
    assert!(!themes.is_current(fingerprint(&members(&library)), NOW + 1));
}

#[rstest]
#[case::few_subjects_still_four(16, 4)]
#[case::square_root_of_half(200, 10)]
#[case::capped(5_000, 24)]
fn the_number_of_themes_grows_with_the_root_of_the_library(#[case] subjects: usize, #[case] expected: usize) {
    assert_eq!(theme_count(subjects), expected);
}

/// Members split by `|`, each member's genres by `,`.
fn genres(spec: &str) -> Vec<Vec<String>> {
    spec.split('|').map(|member| member.split(',').map(str::to_string).collect()).collect()
}

#[rstest]
#[case::most_common_first("Drama,Crime|Crime|Crime,Drama", &[], "Crime · Drama")]
#[case::rare_second_genre_stays_out("Comedy|Comedy|Comedy|Comedy,Music", &[], "Comedy")]
#[case::ties_go_alphabetically("Thriller|Action", &[], "Action · Thriller")]
#[case::duplicates_within_a_member_count_once("Drama,Drama|Crime|Crime|Western", &[], "Crime")]
#[case::taken_name_grows_by_a_genre("Drama,Crime,War|Drama,Crime", &["Crime · Drama"], "Crime · Drama · War")]
#[case::then_by_a_number("Drama|Drama", &["Drama"], "Drama 2")]
#[case::no_genres("| ", &[], "Unlabelled")]
fn a_theme_is_named_from_its_members_genres(#[case] members: &str, #[case] taken: &[&str], #[case] expected: &str) {
    let owned = genres(members);
    let slices: Vec<&[String]> = owned.iter().map(Vec::as_slice).collect();
    let taken: Vec<String> = taken.iter().map(|name| name.to_string()).collect();
    assert_eq!(name(&slices, &taken), expected);
}

/// Two themes by hand: "Drama" (a show and four films) and "Horror" (five films).
fn two_themes() -> Themes {
    let mut assignments: BTreeMap<String, usize> = (1..=4).map(|id| (format!("radarr-{id}"), 0)).collect();
    assignments.insert("sonarr-9".to_string(), 0);
    assignments.extend((11..=15).map(|id| (format!("radarr-{id}"), 1)));
    Themes { computed_at: NOW - DAY, fingerprint: 1, names: vec!["Drama".to_string(), "Horror".to_string()], assignments }
}

fn holding(card_id: &str, gib: u64, played_days_ago: Option<u64>, planned: bool) -> Holding<'_> {
    Holding { card_id, bytes: gib << 30, last_play: played_days_ago.map(|days| NOW - days * DAY), planned }
}

#[test]
fn theme_stats_count_titles_bytes_plays_and_planned_evictions() {
    let holdings = [
        holding("sonarr-9-s1", 10, Some(30), false),
        holding("sonarr-9-s2", 10, Some(30), true),
        holding("radarr-1", 5, Some(400), false),
        holding("radarr-2", 5, None, false),
        holding("radarr-11", 40, None, true),
        holding("radarr-12", 40, Some(500), false),
        holding("radarr-13", 40, None, false),
        holding("radarr-14", 40, None, false),
        holding("radarr-15", 40, None, false),
        holding("radarr-99", 3, Some(1), false),
        holding("radarr-3", 0, Some(1), false),
    ];
    let status = status(&two_themes(), &holdings, NOW);
    let [horror, drama] = status.themes.as_slice() else { panic!("two themes: {:?}", status.themes) };
    assert_eq!(
        (horror.name.as_str(), horror.titles, horror.bytes, horror.played, horror.planned_bytes, horror.cold),
        ("Horror", 5, 200 << 30, 0, 40 << 30, true),
        "largest first; a play older than a year does not count"
    );
    assert_eq!(
        (drama.name.as_str(), drama.titles, drama.bytes, drama.played, drama.planned_bytes, drama.cold),
        ("Drama", 3, 30 << 30, 1, 10 << 30, false),
        "two seasons are one title; nothing on disk is no title"
    );
    assert!((drama.played_share - 1.0 / 3.0).abs() < 1e-12);
    assert_eq!((status.unthemed_titles, status.unthemed_bytes), (1, 3 << 30));

    let cold = cold_cards(&two_themes(), &status, holdings.iter().map(|holding| holding.card_id));
    assert_eq!(cold.len(), 5);
    assert_eq!(cold.get("radarr-11"), Some(&ColdTheme { name: "Horror".to_string(), played_share: 0.0 }));
    assert!(!cold.contains_key("radarr-1") && !cold.contains_key("radarr-99"));
}

const HORROR: [&str; 5] = ["radarr-11", "radarr-12", "radarr-13", "radarr-14", "radarr-15"];

#[rstest]
#[case::unplayed_theme_beside_a_played_one(5, true, true)]
#[case::no_play_anywhere_is_missing_evidence(5, false, false)]
#[case::too_few_titles(4, true, false)]
fn a_theme_is_cold_only_on_evidence(#[case] horror: usize, #[case] played_elsewhere: bool, #[case] cold: bool) {
    let mut holdings: Vec<Holding> = HORROR[..horror].iter().map(|id| holding(id, 40, None, false)).collect();
    holdings.push(holding("radarr-1", 5, played_elsewhere.then_some(10), false));
    let status = status(&two_themes(), &holdings, NOW);
    assert_eq!(status.themes.iter().any(|theme| theme.cold), cold, "{:?}", status.themes);
}
