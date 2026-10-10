use super::*;
use crate::regret::{PlayHistory, WatchFeatures};
use rstest::rstest;

const DAY: u64 = 86_400;
const HORIZON: u64 = 90 * DAY;
const NOW: u64 = 1_800_000_000;
/// When the closed outcomes below were asked.
const OLD: u64 = NOW - 400 * DAY;
const ANN: Viewer = Viewer::PlexAccount(1);
const BO: Viewer = Viewer::PlexAccount(2);
/// Recency weighting off: every outcome counts 1.
const OFF: Decay = Decay::OFF;

/// Unit vectors on two axes: comedies point one way, horror the other.
fn store(entries: &[(&str, [f32; 2])]) -> VectorStore {
    VectorStore::from_vectors(entries.iter().map(|(subject, vector)| (subject.to_string(), vector.to_vec())))
}

fn features(never_played: bool) -> WatchFeatures {
    let plays =
        if never_played { Vec::new() } else { vec![crate::fit::plays::Play { epoch: 0, episode: None, viewer: None, fraction: 1.0 }] };
    let plays: Vec<&crate::fit::plays::Play> = plays.iter().collect();
    WatchFeatures::read(
        &PlayHistory {
            item: &plays,
            audience: &[],
            episodes_total: None,
            episodes_on_disk: None,
            last_watched_days: None,
            added_days_ago: 100.0,
            marked_complete: false,
        },
        NOW,
    )
}

fn row(item_id: &str, cut_unix: u64, played: bool) -> Example {
    Example {
        item_id: item_id.to_string(),
        cut_days: 0.0,
        cut_unix,
        features: features(true),
        label: if played { 1.0 } else { 0.0 },
        played_by: Vec::new(),
    }
}

fn played_by(item_id: &str, cut_unix: u64, viewer: Viewer) -> Example {
    Example { played_by: vec![viewer], ..row(item_id, cut_unix, true) }
}

/// Each viewer played something at each of these dates.
fn activity(seen: &[(Viewer, &[u64])]) -> Activity {
    let plays: Vec<Play> = seen
        .iter()
        .flat_map(|(viewer, epochs)| {
            epochs.iter().map(|epoch| Play { epoch: *epoch, episode: None, viewer: Some(viewer.clone()), fraction: 1.0 })
        })
        .collect();
    Activity::new(&plays)
}

/// The household plays its comedies and leaves its horror; both outcomes
/// closed long before `NOW`.
fn household() -> Vec<Example> {
    vec![
        row("radarr-1", OLD, true),
        row("radarr-2", OLD, true),
        row("radarr-3", OLD, false),
        row("radarr-4", OLD, false),
        row("sonarr-9-s1", OLD, false),
    ]
}

fn vectors() -> VectorStore {
    store(&[
        ("radarr-1", [1.0, 0.05]),
        ("radarr-2", [0.95, 0.1]),
        ("radarr-3", [0.05, 1.0]),
        ("radarr-4", [0.1, 0.95]),
        ("sonarr-9", [0.0, 1.0]),
        ("radarr-10", [0.9, 0.1]),
        ("radarr-11", [0.1, 0.9]),
    ])
}

/// Fifteen comedies (`radarr-101`…) of which Ann played five, six horror
/// films (`radarr-201`…) Bo played every one of: household-wide, comedies
/// are what this house leaves; Ann plays nothing else.
fn two_viewers() -> (Vec<Example>, VectorStore) {
    let mut dataset = Vec::new();
    let mut entries: Vec<(String, [f32; 2])> = vec![("radarr-10".to_string(), [0.9, 0.1])];
    for n in 101..=115 {
        let id = format!("radarr-{n}");
        dataset.push(if n <= 105 { played_by(&id, OLD, ANN) } else { row(&id, OLD, false) });
        entries.push((id, [1.0, 0.02]));
    }
    for n in 201..=206 {
        let id = format!("radarr-{n}");
        dataset.push(played_by(&id, OLD, BO));
        entries.push((id, [0.05, 1.0]));
    }
    let vectors = VectorStore::from_vectors(entries.into_iter().map(|(subject, vector)| (subject, vector.to_vec())));
    (dataset, vectors)
}

fn taste_of(record: &Record, vectors: &VectorStore, subject: &str) -> f64 {
    let pool = Pool::new(vectors, record.household.subjects.keys().map(String::as_str));
    pool.shortlist(vectors, subject).and_then(|shortlist| shortlist.taste(record)).unwrap_or(f64::NAN)
}

#[test]
fn a_title_like_the_ones_played_reads_warm_and_one_like_the_ones_left_reads_cold() {
    let vectors = vectors();
    let record = Record::as_of(&household(), &Activity::default(), HORIZON, NOW, OFF);
    let comedy = taste_of(&record, &vectors, "radarr-10");
    let horror = taste_of(&record, &vectors, "radarr-11");
    assert!(comedy > 0.0, "{comedy}");
    assert!(horror < 0.0, "{horror}");
}

#[test]
fn a_viewer_who_plays_comedies_makes_a_comedy_warm_where_the_household_reads_cold() {
    let (dataset, vectors) = two_viewers();
    let both = activity(&[(ANN, &[OLD - DAY, NOW - DAY]), (BO, &[OLD - DAY, NOW - DAY])]);
    let record = Record::as_of(&dataset, &both, HORIZON, NOW, OFF);
    let household_only = Record { viewers: BTreeMap::new(), ..record.clone() };
    let household = taste_of(&household_only, &vectors, "radarr-10");
    assert!(household < 0.0, "household-wide, comedies sit unplayed: {household}");
    let taste = taste_of(&record, &vectors, "radarr-10");
    assert!(taste > 0.0, "Ann would watch it: {taste}");
}

#[test]
fn a_viewer_with_no_play_in_the_last_year_does_not_speak() {
    let (dataset, vectors) = two_viewers();
    let ann_gone = activity(&[(ANN, &[OLD - DAY]), (BO, &[OLD - DAY, NOW - DAY])]);
    let record = Record::as_of(&dataset, &ann_gone, HORIZON, NOW, OFF);
    assert!(!record.viewers.contains_key(&viewer_key(&ANN)), "{:?}", record.viewers.keys().collect::<Vec<_>>());
    let taste = taste_of(&record, &vectors, "radarr-10");
    assert!(taste < 0.0, "only Bo, who leaves comedies, is here: {taste}");
}

#[test]
fn a_viewer_with_too_few_titles_of_their_own_does_not_speak() {
    let (mut dataset, _) = two_viewers();
    // Ann's fifth comedy was never hers: four titles point nowhere yet.
    dataset[4].played_by.clear();
    let both = activity(&[(ANN, &[OLD - DAY, NOW - DAY]), (BO, &[OLD - DAY, NOW - DAY])]);
    let record = Record::as_of(&dataset, &both, HORIZON, NOW, OFF);
    assert_eq!(record.viewers.keys().collect::<Vec<_>>(), [&viewer_key(&BO)]);
}

#[rstest]
#[case::counts(OFF)]
#[case::recency_weighted(Decay::half_life_days(30.0))]
fn a_row_never_hears_an_outcome_that_closed_after_its_cut(#[case] decay: Decay) {
    let vectors = vectors();
    let early = NOW - 300 * DAY;
    let seen = activity(&[(ANN, &[early - 200 * DAY, early - DAY])]);
    // The comedy outcomes close 30 days after the query's cut: unknown then,
    // to the household and to the viewer who played them alike.
    let mut dataset =
        vec![played_by("radarr-1", early - 60 * DAY, ANN), played_by("radarr-2", early - 60 * DAY, ANN), row("radarr-10", early, false)];
    fill(&mut dataset, &vectors, &seen, HORIZON, decay);
    assert_eq!(dataset[2].features.taste, 0.0);

    let closed = early - 120 * DAY;
    let mut later =
        vec![row("radarr-1", closed, true), row("radarr-2", closed, true), row("radarr-3", closed, false), row("radarr-10", early, false)];
    fill(&mut later, &vectors, &Activity::default(), HORIZON, decay);
    assert!(later[3].features.taste > 0.0, "closed by the cut: {}", later[3].features.taste);
}

#[rstest]
#[case::off(OFF, 1.0)]
#[case::half_life_30_days(Decay::half_life_days(30.0), (-60.0 * std::f64::consts::LN_2 / 30.0).exp())]
#[case::half_life_a_year(Decay::half_life_days(365.0), 0.5f64.powf(60.0 / 365.0))]
fn an_outcome_60_days_older_weighs_its_decay_relative_to_a_fresh_one(#[case] decay: Decay, #[case] relative: f64) {
    let fresh = NOW - HORIZON;
    let dataset = [row("radarr-1", fresh, true), row("radarr-2", fresh - 60 * DAY, true)];
    let record = Record::as_of(&dataset, &Activity::default(), HORIZON, NOW, decay);
    let weight = |subject: &str| record.household.subjects[subject].total;
    assert!((weight("radarr-1") - 1.0).abs() < 1e-12, "closed at as_of, weight 1: {}", weight("radarr-1"));
    assert!((weight("radarr-2") / weight("radarr-1") - relative).abs() < 1e-12, "{} vs {relative}", weight("radarr-2"));
    assert_eq!(record.household.subjects["radarr-2"].played, weight("radarr-2"), "a played outcome weighs the same played as counted");
}

#[test]
fn a_season_is_never_judged_by_its_own_show() {
    let vectors = vectors();
    // Only the show's own first season, played, is close to it.
    let record = Record::as_of(&[row("sonarr-9-s1", OLD, true), row("radarr-1", OLD, false)], &Activity::default(), HORIZON, NOW, OFF);
    let taste = for_cards(&[card("sonarr-9-s2", None)], &vectors, &record);
    assert!(taste.get("sonarr-9-s2").is_some_and(|taste| *taste <= 0.0), "{taste:?}");
}

#[test]
fn only_titles_nobody_played_are_asked_and_seasons_share_their_show() {
    let vectors = vectors();
    let record = Record::as_of(&household(), &Activity::default(), HORIZON, NOW, OFF);
    let cards = [
        card("radarr-10", None),
        card("radarr-11", Some(3.0)),
        card("sonarr-9-s2", None),
        card("sonarr-9-s3", None),
        card("radarr-99", None),
    ];
    let taste = for_cards(&cards, &vectors, &record);
    assert!(taste.contains_key("radarr-10"));
    assert!(!taste.contains_key("radarr-11"), "played: its plays speak for it");
    assert_eq!(taste.get("sonarr-9-s2"), taste.get("sonarr-9-s3"));
    assert!(!taste.contains_key("radarr-99"), "no vector, no taste");
}

#[test]
fn played_rows_keep_their_taste_at_zero() {
    let vectors = vectors();
    let mut dataset = household();
    dataset.push(Example { features: features(false), ..row("radarr-10", NOW - 100 * DAY, false) });
    fill(&mut dataset, &vectors, &Activity::default(), HORIZON, OFF);
    assert_eq!(dataset[5].features.taste, 0.0);
}

#[test]
fn a_reading_names_the_nearest_titles_that_went_its_way() {
    let vectors = vectors();
    let record = Record::as_of(&household(), &Activity::default(), HORIZON, NOW, OFF);
    let readings = read_cards(&[card("radarr-10", None), card("radarr-11", None)], &vectors, &record);
    let title = |subject: &str| match subject {
        "radarr-1" => Some("Hot Fuzz"),
        "radarr-2" => Some("Paddington"),
        "radarr-4" => Some("The Conjuring"),
        "sonarr-9" => Some("Chernobyl"),
        _ => None,
    };
    let note = |id: &str| readings.get(id).and_then(|reading| reading.like.as_ref()).and_then(|like| like.note(title));
    assert_eq!(note("radarr-10").as_deref(), Some("like Paddington, Hot Fuzz (played here)"), "nearest first");
    // radarr-3, second nearest among the unplayed, has no name here: the next one speaks.
    assert_eq!(note("radarr-11").as_deref(), Some("like The Conjuring, Chernobyl (unplayed here)"));
}

fn card(id: &str, last_watched_days: Option<f32>) -> ArchiveCard {
    ArchiveCard { id: id.to_string(), last_watched_days, ..crate::golden::golden_movie() }
}
