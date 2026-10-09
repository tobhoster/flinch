use super::*;
use crate::card::LibraryKind;
use rstest::rstest;

/// Comedies point one way, horror the other; the household plays comedy.
fn vectors(with_show: bool) -> VectorStore {
    let mut entries = vec![
        ("radarr-1", vec![1.0, 0.0]),
        ("radarr-2", vec![1.0, 0.0]),
        ("radarr-3", vec![0.0, 1.0]),
        ("radarr-4", vec![0.0, 1.0]),
        ("radarr-5", vec![0.0, 1.0]),
    ];
    if with_show {
        entries.push(("sonarr-7", vec![0.0, 1.0]));
    }
    VectorStore::from_vectors(entries.into_iter().map(|(subject, vector)| (subject.to_string(), vector)))
}

fn record() -> Record {
    let mut record = Record::default();
    for (subject, played) in [("radarr-1", 1), ("radarr-2", 1), ("radarr-3", 0), ("radarr-4", 0), ("radarr-5", 0)] {
        let outcome = Outcomes { played, total: 1 };
        record.household.overall.played += outcome.played;
        record.household.overall.total += 1;
        record.household.subjects.insert(subject.to_string(), outcome);
    }
    record
}

fn season(n: u32, last_watched_days: Option<f32>, is_watched: Option<bool>) -> ArchiveCard {
    ArchiveCard {
        id: format!("sonarr-7-s{n}"),
        kind: LibraryKind::Season,
        size_bytes: 4 * 1_073_741_824,
        last_watched_days,
        is_watched,
        season_index: Some(n),
        ..crate::golden::golden_movie()
    }
}

fn show(status: &str) -> ArrSeries {
    ArrSeries {
        id: 7,
        title: "Dark Hours".to_string(),
        status: Some(status.to_string()),
        monitored: Some(true),
        tvdb_id: Some(70),
        ..ArrSeries::default()
    }
}

fn run(cards: &[ArchiveCard], series: &[ArrSeries], requests: &[Request], vectors: &VectorStore) -> Vec<Suggestion> {
    let record = record();
    suggest(&Inputs { cards, series, movies: &[], requests, vectors, record: &record })
}

#[test]
fn a_cold_show_nobody_started_is_flagged_with_its_season_size() {
    let cards = [season(1, None, None), season(2, None, None)];
    let out = run(&cards, &[show("continuing")], &[], &vectors(true));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].rule, Rule::ColdUnstarted);
    assert_eq!(out[0].gib_per_season, Some(4.0));
    assert!(out[0].taste.is_some_and(|t| t < COLD_TASTE));
}

#[rstest]
#[case::played_recently(Some(10.0), None)]
#[case::finished_and_ended(Some(400.0), Some(true))]
fn a_show_somebody_started_is_not_called_cold(#[case] last: Option<f32>, #[case] watched: Option<bool>) {
    let cards = [season(1, last, watched)];
    assert!(run(&cards, &[show("ended")], &[], &vectors(true)).is_empty());
}

#[test]
fn a_show_left_unfinished_long_ago_is_abandoned_without_any_vector() {
    let cards = [season(1, Some(200.0), Some(false)), season(2, None, None)];
    let out = run(&cards, &[show("ended")], &[], &VectorStore::default());
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].rule, Rule::Abandoned);
}

#[test]
fn a_show_without_a_vector_is_never_called_cold() {
    let cards = [season(1, None, None)];
    assert!(run(&cards, &[show("continuing")], &[], &vectors(false)).is_empty());
}

#[rstest]
#[case::with_vector(true, 1)]
#[case::without_vector(false, 0)]
fn a_cold_request_is_flagged_for_its_requester_only_with_a_vector(#[case] with_show: bool, #[case] expected: usize) {
    let request = Request { media: MediaRef::Show { tvdb: Some(70), tmdb: None }, seasons: vec![3], requester: "sam".to_string() };
    // An ended show is outside the unstarted rule; only the request names it.
    let out = run(&[season(1, None, None)], &[show("ended")], &[request], &vectors(with_show));
    assert_eq!(out.len(), expected);
    if let Some(first) = out.first() {
        assert_eq!((first.rule, first.requester.as_deref()), (Rule::ColdRequest, Some("sam")));
    }
}

#[test]
fn the_household_record_counts_a_show_once_played_if_any_season_was() {
    let cards = [season(1, Some(3.0), None), season(2, None, None)];
    let record = household_record(&cards, 0);
    assert_eq!(record.household.overall, Outcomes { played: 1, total: 1 });
}
