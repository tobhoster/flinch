//! Fit-pipeline tests: the panel's construction rules (cut dates, censoring,
//! arrival, no leakage) and what the fit learns.

use super::panel::{build_dataset, Example, PanelSpec};
use super::*;
use rstest::rstest;

mod gate;

const NOW: u64 = 1_800_000_000;
const DAY: u64 = 86_400;

fn play(epoch: u64) -> Play {
    Play { epoch, episode: None, viewer: None, fraction: 1.0 }
}

pub(super) fn item(id: &str, kind: LibraryKind, age_days: f32, plays: Vec<u64>) -> FitItem {
    let plays: Vec<Play> = plays.into_iter().map(play).collect();
    FitItem {
        id: id.to_string(),
        title: id.to_string(),
        kind,
        size_bytes: 20_000_000_000,
        age_days,
        episodes_total: (kind == LibraryKind::Season).then_some(8),
        season_index: Some(1),
        show_title: (kind == LibraryKind::Season).then(|| "Show".to_string()),
        audience_plays: plays.clone(),
        plays,
        on_disk: Vec::new(),
    }
}

pub(super) fn panel(items: &[FitItem], cuts_days: &[f32], horizon_days: f32) -> Vec<Example> {
    build_dataset(items, &PanelSpec { now: NOW, cuts_days, horizon_days })
}

#[test]
fn the_panel_uses_only_plays_that_preceded_the_cut() {
    let played_long_ago = item("season-a", LibraryKind::Season, 500.0, vec![NOW - 400 * DAY]);
    let played_in_the_horizon = item("season-b", LibraryKind::Season, 500.0, vec![NOW - 10 * DAY]);
    let dataset = panel(&[played_long_ago, played_in_the_horizon], &[90.0], 90.0);
    let a = dataset.iter().find(|e| e.item_id == "season-a").expect("row a");
    let b = dataset.iter().find(|e| e.item_id == "season-b").expect("row b");
    assert_eq!(a.label, 0.0, "nothing played it after the cut");
    assert!((a.features.days_since_last_play - 310.0).abs() < 1e-6, "measured from the cut");
    assert_eq!(b.label, 1.0, "played during the horizon");
    assert!(b.features.never_played, "the horizon play must not reach the features");
    assert!((b.features.days_since_last_play - 410.0).abs() < 1e-6, "censored at its days on disk at the cut");
}

#[test]
fn an_item_played_before_its_recorded_arrival_is_asked_about_from_that_play() {
    // A migration re-imported it 20 days ago; the household first played it
    // 190 days ago. The cuts after that play are real questions.
    let migrated = item("season-m", LibraryKind::Season, 20.0, vec![NOW - 190 * DAY]);
    let cuts: Vec<f32> = panel(&[migrated], &[60.0, 150.0, 210.0], 30.0).iter().map(|row| row.cut_days).collect();
    assert_eq!(cuts, [60.0, 150.0]);
}

#[rstest]
#[case::before_it_arrived(250.0, None)]
#[case::in_the_first_span(150.0, Some(50.0))]
#[case::in_the_gap(90.0, None)]
#[case::in_the_second_span(40.0, Some(10.0))]
fn presence_history_decides_which_cuts_ask_about_an_item(#[case] cut_days: f32, #[case] dwell_days: Option<f64>) {
    // On disk from 200 to 100 days ago, gone, back 50 days ago; never played.
    let mut movie = item("radarr-10", LibraryKind::Movie, 50.0, vec![]);
    movie.on_disk = vec![
        crate::presence::Span { from: NOW - 200 * DAY, to: Some(NOW - 100 * DAY) },
        crate::presence::Span { from: NOW - 50 * DAY, to: None },
    ];
    let dataset = panel(&[movie], &[cut_days], 30.0);
    assert_eq!(dataset.first().map(|row| row.features.days_since_last_play), dwell_days, "censored at the covering span's start");
}

#[test]
fn a_window_that_has_not_finished_is_never_a_row() {
    let quiet = item("season-c", LibraryKind::Season, 500.0, vec![]);
    assert!(panel(std::slice::from_ref(&quiet), &[30.0], 90.0).is_empty(), "an open window would read as not played");
    assert_eq!(panel(&[quiet], &[120.0], 90.0).len(), 1);
}

#[test]
fn items_added_after_the_cut_are_not_judged_at_it() {
    assert!(panel(&[item("season-new", LibraryKind::Season, 10.0, vec![])], &[180.0], 90.0).is_empty());
}

#[test]
fn folds_split_by_item_so_a_title_never_validates_itself() {
    let items: Vec<FitItem> = (0..40).map(|n| item(&format!("radarr-{n}"), LibraryKind::Movie, 800.0, vec![])).collect();
    let dataset = panel(&items, &default_cuts(), HORIZON_DAYS);
    for id in dataset.iter().map(|row| row.item_id.as_str()) {
        let folds: std::collections::HashSet<usize> =
            dataset.iter().filter(|row| row.item_id == id).map(|row| fold_of(&row.item_id)).collect();
        assert_eq!(folds.len(), 1, "{id} spans folds");
    }
}
