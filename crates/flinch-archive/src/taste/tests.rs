use super::*;
use crate::embedding::{Description, RECIPE_VERSION};
use crate::regret::{PlayHistory, WatchFeatures};
use rstest::rstest;

const DAY: u64 = 86_400;
const HORIZON: u64 = 90 * DAY;
const NOW: u64 = 1_800_000_000;
/// When the closed outcomes below were asked.
const OLD: u64 = NOW - 400 * DAY;
const ANN: Viewer = Viewer::PlexAccount(1);
const BO: Viewer = Viewer::PlexAccount(2);

/// Unit vectors on two axes, each subject titled and described: comedies
/// point one way, horror the other.
fn store(entries: &[(&str, [f32; 2])]) -> VectorStore {
    let mut store = VectorStore::default();
    store.retarget("ollama:test", 0, RECIPE_VERSION);
    for (subject, vector) in entries {
        let description = Description::new(format!("Name of {subject}"), format!("Title: {subject}"));
        store.insert(subject.to_string(), description, vector.to_vec()).expect("a 2-d vector");
    }
    store
}

/// A Clef stand-in: the played share of the three nearest titles, pulled
/// toward even odds.
fn clef(state: &State) -> f64 {
    let near = &state.nearest[..state.nearest.len().min(3)];
    (near.iter().filter(|neighbour| neighbour.played).count() as f64 + 0.5) / (near.len() as f64 + 1.0)
}

/// What the daemon does over two cycles: `run` once to learn its questions,
/// answer every one as `clef` would, then `run` again on the answers.
fn judged<T>(mut clef: impl FnMut(&State) -> f64, mut run: impl FnMut(&mut Judge<'_>) -> T) -> T {
    let mut cache = AnswerCache::default();
    cache.retarget("clef");
    let mut judge = Judge::new(&cache);
    run(&mut judge);
    let asked = judge.finish();
    cache.note(&asked, NOW);
    for (key, state) in cache.queue(&asked) {
        cache.answer(key, clef(&state), NOW);
    }
    let mut judge = Judge::new(&cache);
    let out = run(&mut judge);
    assert_eq!(judge.finish().missing(), 0, "every question was answered");
    out
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

/// Fifteen comedies (`radarr-101`…) of which Ann played five — the five
/// nearest `radarr-10` — and six horror films (`radarr-201`…) Bo played
/// every one of. Ann plays nothing else; Bo never plays a comedy.
fn two_viewers() -> (Vec<Example>, VectorStore) {
    let mut dataset = Vec::new();
    let mut entries: Vec<(String, [f32; 2])> = vec![("radarr-10".to_string(), [0.9, 0.1])];
    for n in 101..=115 {
        let id = format!("radarr-{n}");
        let vector = if n <= 105 { [1.0, 0.11] } else { [1.0, 0.3] };
        dataset.push(if n <= 105 { played_by(&id, OLD, ANN) } else { row(&id, OLD, false) });
        entries.push((id, vector));
    }
    for n in 201..=206 {
        let id = format!("radarr-{n}");
        dataset.push(played_by(&id, OLD, BO));
        entries.push((id, [0.05, 1.0]));
    }
    let entries: Vec<(&str, [f32; 2])> = entries.iter().map(|(id, vector)| (id.as_str(), *vector)).collect();
    (dataset, store(&entries))
}

fn taste_of(record: &Record, vectors: &VectorStore, subject: &str, judge: &mut Judge<'_>) -> Option<f64> {
    let pool = Pool::new(vectors, record.household.subjects.keys().map(String::as_str));
    pool.shortlist(subject).and_then(|shortlist| shortlist.taste(record, judge))
}

fn judged_taste(record: &Record, vectors: &VectorStore, subject: &str) -> f64 {
    judged(clef, |judge| taste_of(record, vectors, subject, judge)).unwrap_or(f64::NAN)
}

#[rstest]
#[case::likely(0.8, (0.8f64 / 0.2).ln())]
#[case::even_odds_is_no_evidence(0.5, 0.0)]
#[case::unlikely(0.2, (0.2f64 / 0.8).ln())]
#[case::certain_is_bounded(1.0, (0.99f64 / 0.01).ln())]
fn clefs_probability_is_the_taste_as_log_odds(#[case] p: f64, #[case] taste: f64) {
    let vectors = vectors();
    let record = Record::as_of(&household(), &Activity::default(), HORIZON, NOW);
    let read = judged(|_| p, |judge| taste_of(&record, &vectors, "radarr-10", judge));
    assert!(read.is_some_and(|read| (read - taste).abs() < 1e-12), "{read:?} vs {taste}");
}

#[test]
fn a_title_like_the_ones_played_reads_warm_and_one_like_the_ones_left_reads_cold() {
    let vectors = vectors();
    let record = Record::as_of(&household(), &Activity::default(), HORIZON, NOW);
    let comedy = judged_taste(&record, &vectors, "radarr-10");
    let horror = judged_taste(&record, &vectors, "radarr-11");
    assert!(comedy > 0.0, "{comedy}");
    assert!(horror < 0.0, "{horror}");
}

#[test]
fn clef_is_shown_the_description_and_the_nearest_titles_marked_played() {
    let vectors = vectors();
    let record = Record::as_of(&household(), &Activity::default(), HORIZON, NOW);
    let mut shown = Vec::new();
    judged(
        |state| {
            shown.push(state.clone());
            0.5
        },
        |judge| taste_of(&record, &vectors, "radarr-10", judge),
    );
    let neighbour = |subject: &str, played| Neighbour { title: format!("Name of {subject}"), played };
    let nearest = [neighbour("radarr-2", true), neighbour("radarr-1", true), neighbour("radarr-4", false), neighbour("radarr-3", false)];
    // The season's own show (sonarr-9) is a neighbour too: its first season's outcome is closed.
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].title, "Title: radarr-10");
    assert_eq!(shown[0].nearest[..4], nearest[..]);
    assert_eq!(shown[0].nearest.len(), 5);
}

#[test]
fn a_viewer_who_plays_comedies_makes_a_comedy_warm_where_another_reads_cold() {
    let (dataset, vectors) = two_viewers();
    let both = activity(&[(ANN, &[OLD - DAY, NOW - DAY]), (BO, &[OLD - DAY, NOW - DAY])]);
    let record = Record::as_of(&dataset, &both, HORIZON, NOW);
    let mut bo_only = record.clone();
    bo_only.viewers.remove(&viewer_key(&ANN));
    let bo = judged_taste(&bo_only, &vectors, "radarr-10");
    assert!(bo < 0.0, "Bo leaves every comedy: {bo}");
    let taste = judged_taste(&record, &vectors, "radarr-10");
    assert!(taste > 0.0, "Ann would watch it: {taste}");
}

#[test]
fn the_warmest_viewer_speaks_and_a_viewer_still_unanswered_holds_the_taste_back() {
    let (dataset, vectors) = two_viewers();
    let both = activity(&[(ANN, &[OLD - DAY, NOW - DAY]), (BO, &[OLD - DAY, NOW - DAY])]);
    let record = Record::as_of(&dataset, &both, HORIZON, NOW);
    let pool = Pool::new(&vectors, record.household.subjects.keys().map(String::as_str));
    let Some(shortlist) = pool.shortlist("radarr-10") else { panic!("radarr-10 has a vector") };
    let mut cache = AnswerCache::default();
    cache.retarget("clef");
    let mut judge = Judge::new(&cache);
    assert_eq!(shortlist.read(&record, &mut judge), None, "nothing is answered yet");
    let asked = judge.finish();
    assert_eq!(asked.missing(), 2, "one question per viewer");
    cache.note(&asked, NOW);
    let queue = cache.queue(&asked);
    // Ann's question leads with the comedies she played; Bo's with ones he left.
    let (ann, bo): (Vec<_>, Vec<_>) = queue.into_iter().partition(|(_, state)| state.nearest[0].played);
    cache.answer(ann[0].0.clone(), 0.9, NOW);
    let mut judge = Judge::new(&cache);
    assert_eq!(shortlist.read(&record, &mut judge), None, "Bo's answer is still to come");
    cache.answer(bo[0].0.clone(), 0.1, NOW);
    let mut judge = Judge::new(&cache);
    let Some(reading) = shortlist.read(&record, &mut judge) else { panic!("both answered") };
    assert!((reading.taste - (0.9f64 / 0.1).ln()).abs() < 1e-12, "Ann's warmth speaks: {}", reading.taste);
    assert_eq!(reading.like.map(|like| (like.played, like.subjects.len())), Some((true, 5)), "the five comedies Ann played");
}

#[test]
fn a_viewer_with_no_play_in_the_last_year_does_not_speak() {
    let (dataset, vectors) = two_viewers();
    let ann_gone = activity(&[(ANN, &[OLD - DAY]), (BO, &[OLD - DAY, NOW - DAY])]);
    let record = Record::as_of(&dataset, &ann_gone, HORIZON, NOW);
    assert!(!record.viewers.contains_key(&viewer_key(&ANN)), "{:?}", record.viewers.keys().collect::<Vec<_>>());
    let taste = judged_taste(&record, &vectors, "radarr-10");
    assert!(taste < 0.0, "only Bo, who leaves comedies, is here: {taste}");
}

#[test]
fn a_viewer_with_too_few_titles_of_their_own_does_not_speak() {
    let (mut dataset, _) = two_viewers();
    // Ann's fifth comedy was never hers: four titles point nowhere yet.
    dataset[4].played_by.clear();
    let both = activity(&[(ANN, &[OLD - DAY, NOW - DAY]), (BO, &[OLD - DAY, NOW - DAY])]);
    let record = Record::as_of(&dataset, &both, HORIZON, NOW);
    assert_eq!(record.viewers.keys().collect::<Vec<_>>(), [&viewer_key(&BO)]);
}

#[test]
fn a_row_never_hears_an_outcome_that_closed_after_its_cut() {
    let vectors = vectors();
    let early = NOW - 300 * DAY;
    let seen = activity(&[(ANN, &[early - 200 * DAY, early - DAY])]);
    // The comedy outcomes close 30 days after the query's cut: unknown then,
    // to the household and to the viewer who played them alike, so there is
    // nothing to ask.
    let mut dataset =
        vec![played_by("radarr-1", early - 60 * DAY, ANN), played_by("radarr-2", early - 60 * DAY, ANN), row("radarr-10", early, false)];
    let mut cache = AnswerCache::default();
    cache.retarget("clef");
    let mut judge = Judge::new(&cache);
    fill(&mut dataset, &vectors, &seen, HORIZON, &mut judge);
    assert_eq!(dataset[2].features.taste, 0.0);
    assert_eq!(judge.finish().missing(), 0, "no neighbour had closed: no question");

    let closed = early - 120 * DAY;
    let later =
        vec![row("radarr-1", closed, true), row("radarr-2", closed, true), row("radarr-3", closed, false), row("radarr-10", early, false)];
    let filled = judged(clef, |judge| {
        let mut rows = later.clone();
        fill(&mut rows, &vectors, &Activity::default(), HORIZON, judge);
        rows[3].features.taste
    });
    assert!(filled > 0.0, "closed by the cut: {filled}");
}

#[test]
fn a_season_is_never_judged_by_its_own_show() {
    let vectors = vectors();
    // Only the show's own first season, played, is close to it.
    let record = Record::as_of(&[row("sonarr-9-s1", OLD, true), row("radarr-1", OLD, false)], &Activity::default(), HORIZON, NOW);
    let taste = judged(clef, |judge| for_cards(&[card("sonarr-9-s2", None)], &vectors, &record, judge));
    assert!(taste.get("sonarr-9-s2").is_some_and(|taste| *taste <= 0.0), "{taste:?}");
}

#[test]
fn only_titles_nobody_played_are_asked_and_seasons_share_their_show() {
    let vectors = vectors();
    let record = Record::as_of(&household(), &Activity::default(), HORIZON, NOW);
    let cards = [
        card("radarr-10", None),
        card("radarr-11", Some(3.0)),
        card("sonarr-9-s2", None),
        card("sonarr-9-s3", None),
        card("radarr-99", None),
    ];
    let taste = judged(clef, |judge| for_cards(&cards, &vectors, &record, judge));
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
    let mut judge = Judge::off();
    fill(&mut dataset, &vectors, &Activity::default(), HORIZON, &mut judge);
    assert_eq!(dataset[5].features.taste, 0.0);
}

#[test]
fn a_reading_names_the_nearest_titles_that_went_its_way() {
    let vectors = vectors();
    let record = Record::as_of(&household(), &Activity::default(), HORIZON, NOW);
    let readings = judged(clef, |judge| read_cards(&[card("radarr-10", None), card("radarr-11", None)], &vectors, &record, judge));
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

#[test]
fn unanswered_questions_are_collected_once_and_the_judge_switched_off_asks_none() {
    let vectors = vectors();
    let record = Record::as_of(&household(), &Activity::default(), HORIZON, NOW);
    let cards = [card("radarr-10", None), card("sonarr-9-s2", None), card("sonarr-9-s3", None), card("radarr-99", None)];
    let mut cache = AnswerCache::default();
    cache.retarget("clef");
    let mut judge = Judge::new(&cache);
    assert!(read_cards(&cards, &vectors, &record, &mut judge).is_empty(), "nothing is answered yet: no taste anywhere");
    let asked = judge.finish();
    assert_eq!(asked.missing(), 2, "radarr-10 and the show once; radarr-99 has no vector");
    assert!(cache.note(&asked, NOW));
    assert_eq!(cache.pending(), 2);
    assert!(!cache.note(&asked, NOW), "asking again the same day changes nothing");

    let mut off = Judge::off();
    assert!(read_cards(&cards, &vectors, &record, &mut off).is_empty());
    assert_eq!(off.finish().missing(), 0);
}

#[test]
fn a_title_without_its_description_is_not_asked() {
    let mut vectors = vectors();
    let bare = Description { hash: "x".into(), title: String::new(), text: String::new() };
    vectors.insert("radarr-10".into(), bare, vec![0.9, 0.1]).expect("a 2-d vector");
    let record = Record::as_of(&household(), &Activity::default(), HORIZON, NOW);
    let mut cache = AnswerCache::default();
    cache.retarget("clef");
    let mut judge = Judge::new(&cache);
    assert_eq!(taste_of(&record, &vectors, "radarr-10", &mut judge), None);
    assert_eq!(judge.finish().missing(), 0);
}

/// The keys of the questions `cards` ask with `record`.
fn keys(cards: &[ArchiveCard], vectors: &VectorStore, record: &Record) -> Vec<String> {
    let mut cache = AnswerCache::default();
    cache.retarget("clef");
    let mut judge = Judge::new(&cache);
    read_cards(cards, vectors, record, &mut judge);
    let asked = judge.finish();
    cache.note(&asked, NOW);
    cache.queue(&asked).into_iter().map(|(key, _)| key).collect()
}

#[test]
fn a_day_later_the_same_outcomes_ask_the_same_question() {
    let vectors = vectors();
    let cards = [card("radarr-10", None)];
    let today = keys(&cards, &vectors, &Record::as_of(&household(), &Activity::default(), HORIZON, NOW));
    // A day on, radarr-1 was played again: more plays, the same outcome.
    let mut tomorrow = household();
    tomorrow.push(row("radarr-1", OLD + 30 * DAY, true));
    let later = keys(&cards, &vectors, &Record::as_of(&tomorrow, &Activity::default(), HORIZON, NOW + DAY));
    assert_eq!(today.len(), 1);
    assert_eq!(later, today, "the cached answer still serves");
    // An outcome that flips is another question.
    let mut flipped = household();
    flipped[2].label = 1.0;
    assert_ne!(keys(&cards, &vectors, &Record::as_of(&flipped, &Activity::default(), HORIZON, NOW)), today);
}

#[test]
fn another_model_or_question_version_is_another_key() {
    let state = State { nearest: vec![Neighbour { title: "Heat (1995)".into(), played: true }], title: "Title: Ronin".into() };
    assert_eq!(judge::key("clef", &state), judge::key("clef", &state.clone()));
    assert_ne!(judge::key("clef", &state), judge::key("clef:cloud", &state));
    assert_eq!(judge::key("clef", &state).len(), 64, "hex SHA-256");
    let shown = serde_json::to_string(&state).expect("encodes");
    assert_eq!(shown, r#"{"nearest":[{"played":true,"title":"Heat (1995)"}],"title":"Title: Ronin"}"#, "canonical key order");
}

#[test]
fn the_question_names_the_horizon() {
    let questions = judge::questions();
    let Some(crate::ollama::Question::Noul { instructions }) = questions.get(judge::QUESTION) else { panic!("{questions:?}") };
    assert!(instructions.contains("within 90 days of it arriving"), "{instructions}");
}

#[test]
fn a_new_model_clears_the_cache_and_unused_answers_age_out() {
    let vectors = vectors();
    let record = Record::as_of(&household(), &Activity::default(), HORIZON, NOW);
    let cards = [card("radarr-10", None), card("radarr-11", None)];
    let mut cache = AnswerCache::default();
    cache.retarget("clef");
    let mut judge = Judge::new(&cache);
    read_cards(&cards, &vectors, &record, &mut judge);
    let asked = judge.finish();
    cache.note(&asked, NOW);
    let queue = cache.queue(&asked);
    cache.answer(queue[0].0.clone(), 0.7, NOW);
    cache.spend(1, NOW);
    assert_eq!((cache.answered(), cache.pending(), cache.budget_left(500, NOW)), (1, 1, 499));

    assert!(!cache.prune(NOW + judge::KEEP_DAYS * DAY), "used today: kept for the whole window");
    assert!(cache.prune(NOW + (judge::KEEP_DAYS + 1) * DAY));
    assert_eq!((cache.answered(), cache.pending()), (0, 0));

    cache.answer("k".into(), 0.5, NOW);
    assert!(!cache.retarget("clef"));
    assert!(cache.retarget("clef:cloud"), "another judge's answers are not this one's");
    assert_eq!((cache.answered(), cache.model()), (0, "clef:cloud"));
}

#[test]
fn this_cycles_questions_are_asked_first() {
    let vectors = vectors();
    let record = Record::as_of(&household(), &Activity::default(), HORIZON, NOW);
    let mut cache = AnswerCache::default();
    cache.retarget("clef");
    let ask = |cache: &AnswerCache, cards: &[ArchiveCard]| {
        let mut judge = Judge::new(cache);
        read_cards(cards, &vectors, &record, &mut judge);
        judge.finish()
    };
    let panel = ask(&cache, &[card("radarr-10", None), card("radarr-11", None)]);
    cache.note(&panel, NOW);
    let cards = ask(&cache, &[card("radarr-11", None)]);
    cache.note(&cards, NOW);
    let queue = cache.queue(&cards);
    assert_eq!(queue.len(), 2);
    assert_eq!(queue[0].1.title, "Title: radarr-11");
}

fn card(id: &str, last_watched_days: Option<f32>) -> ArchiveCard {
    ArchiveCard { id: id.to_string(), last_watched_days, ..crate::golden::golden_movie() }
}
