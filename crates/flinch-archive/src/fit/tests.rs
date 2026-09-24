//! Fit-pipeline tests.
//!
//! Kept beside the module rather than inside it: the construction rules (cut
//! dates, censoring, grouping, no leakage) are the part that must not drift, and
//! they read better as their own file than as a tail end to the pipeline.

use super::export::PanelRow;
use super::panel::{build_dataset, PanelSpec};
use super::plays::Viewer;
use super::*;
use crate::card::SeasonState;
use proptest::prelude::*;
use rstest::rstest;

mod gate;

const NOW: u64 = 1_800_000_000;
const DAY: u64 = 86_400;

fn play(epoch: u64) -> Play {
    Play { epoch, episode: None, viewer: None, complete: true }
}

fn item(id: &str, kind: LibraryKind, age_days: f32, plays: Vec<u64>) -> FitItem {
    let plays: Vec<Play> = plays.into_iter().map(play).collect();
    FitItem {
        id: id.to_string(),
        title: id.to_string(),
        kind,
        size_bytes: 20_000_000_000,
        age_days,
        episodes_total: Some(8),
        season_index: Some(1),
        show_title: if kind == LibraryKind::Season { Some("Show".to_string()) } else { None },
        audience_plays: plays.clone(),
        plays,
        watch_source: Some(WatchSource::Plex),
        is_newest_season: false,
        series_status: None,
        last_aired_epoch: None,
        guid_resolved: true,
        genres: Vec::new(),
        on_disk: Vec::new(),
    }
}

fn panel(items: &[FitItem], cuts_days: &[f32], horizon_days: f32) -> Vec<Example> {
    build_dataset(items, &PanelSpec { now: NOW, cuts_days, horizon_days, tautulli_coverage_start: None })
}

#[test]
fn the_panel_uses_only_plays_that_preceded_the_cut() {
    // Played 400 days ago: at a 90-day cut that is 310 days old, outside the
    // recency window, and nothing has played it since.
    let played_long_ago = item("season-a", LibraryKind::Season, 500.0, vec![NOW - 400 * DAY]);
    // Played 10 days ago: inside the horizon of a 90-day cut.
    let played_recently = item("season-b", LibraryKind::Season, 500.0, vec![NOW - 10 * DAY]);
    let dataset = panel(&[played_long_ago, played_recently], &[90.0], 90.0);
    assert_eq!(dataset.len(), 2);

    let a = dataset.iter().find(|e| e.item_id == "season-a").expect("row a");
    let b = dataset.iter().find(|e| e.item_id == "season-b").expect("row b");
    assert_eq!(a.label, 1.0, "nothing played it after the cut");
    assert!(a.values.get("recent_play").is_none(), "310 d before the cut is outside the recency window");
    assert!(a.values.get("never_played").is_none(), "it had been played by then");
    assert_eq!(b.label, 0.0, "it was played during the horizon");
    assert!(b.values.get("recent_play").is_none(), "the horizon play must not be a feature");
}

#[test]
fn recency_is_measured_from_the_cut_not_from_today() {
    // Played 190 days ago, cut 90 days ago: 100 days old at the cut (inside the
    // half-weight recency band), and nothing played it afterwards.
    let played = item("season-c", LibraryKind::Season, 500.0, vec![NOW - 190 * DAY]);
    let dataset = panel(&[played], &[90.0], 90.0);
    let row = &dataset[0];
    let recency = row.values.get("recent_play").copied().unwrap_or(0.0);
    assert!((recency - 0.5).abs() < 1e-6, "expected the half-weight band, got {recency}");
    assert!(row.values.get("never_played").is_none(), "it had been played by the cut");
    assert_eq!(row.label, 1.0, "nothing played it during the horizon");
}

#[test]
fn dwell_is_measured_at_the_cut_not_from_today() {
    // Added 500 days ago, asked about 365 days ago: it had been on disk 135
    // days then. Adding instead of subtracting the cut distance once made every
    // older row look nearly two years staler than the daemon ever saw it.
    let untouched = item("movie-d", LibraryKind::Movie, 500.0, vec![]);
    let dataset = panel(&[untouched], &[365.0], 30.0);
    assert!((dataset[0].card.added_days_ago - 135.0).abs() < 1e-3, "got {}", dataset[0].card.added_days_ago);
    let dwell = dataset[0].values["dwell"];
    assert!((dwell - 135.0 / 365.0).abs() < 1e-4, "dwell in years at the cut, got {dwell}");
}

#[test]
fn an_item_played_before_its_recorded_arrival_is_asked_about_from_that_play() {
    // A migration re-imported it 20 days ago; the household first played it
    // 190 days ago. The play proves it existed, so the cuts after that play
    // are real questions — and the cut before it is still not.
    let migrated = item("season-m", LibraryKind::Season, 20.0, vec![NOW - 190 * DAY]);
    let dataset = panel(&[migrated], &[60.0, 150.0, 210.0], 30.0);
    let cuts: Vec<f32> = dataset.iter().map(|row| row.cut_days).collect();
    assert_eq!(cuts, [60.0, 150.0], "only cuts after the first play");
    let at_60 = &dataset[0];
    assert!((at_60.card.added_days_ago - 130.0).abs() < 1e-3, "dwell from the proving play, got {}", at_60.card.added_days_ago);
}

#[rstest]
#[case::before_it_arrived(250.0, None)]
#[case::in_the_first_span(150.0, Some(50.0))]
#[case::in_the_gap(90.0, None)]
#[case::in_the_second_span(40.0, Some(10.0))]
fn presence_history_decides_which_cuts_ask_about_an_item(#[case] cut_days: f32, #[case] dwell_days: Option<f32>) {
    // On disk from 200 to 100 days ago, gone, back 50 days ago. The play 190
    // days ago would alone claim it was there all along; history knows better.
    let mut movie = item("radarr-10", LibraryKind::Movie, 50.0, vec![NOW - 190 * DAY]);
    movie.on_disk = vec![
        crate::presence::Span { from: NOW - 200 * DAY, to: Some(NOW - 100 * DAY) },
        crate::presence::Span { from: NOW - 50 * DAY, to: None },
    ];
    let dataset = panel(&[movie], &[cut_days], 30.0);
    // Dwell from the covering span's start: what the daemon showed that day.
    assert_eq!(dataset.first().map(|row| row.card.added_days_ago), dwell_days);
}

#[test]
fn a_window_that_has_not_finished_is_never_labelled_safe() {
    let item = item("season-c", LibraryKind::Season, 500.0, vec![]);
    // 30-day cut with a 90-day horizon ends in the future: the outcome is
    // unknown, so there must be no row at all.
    assert!(panel(&[item.clone()], &[30.0], 90.0).is_empty(), "a censored window must not become a positive example");
    // The same cut is legitimate once the horizon fits inside the past.
    assert_eq!(panel(&[item], &[120.0], 90.0).len(), 1);
}

#[test]
fn items_added_after_the_cut_are_not_judged_at_it() {
    let fresh = item("season-new", LibraryKind::Season, 10.0, vec![]);
    assert!(panel(&[fresh], &[180.0], 90.0).is_empty());
}

#[test]
fn a_never_played_item_is_a_positive_example_with_its_features() {
    let untouched = item("movie-x", LibraryKind::Movie, 400.0, vec![]);
    let dataset = panel(&[untouched], &[120.0], 90.0);
    assert_eq!(dataset.len(), 1);
    assert_eq!(dataset[0].label, 1.0);
    assert!(dataset[0].values.get("never_played").is_some());
    assert!(dataset[0].values.get("dwell").is_some());
}

#[test]
fn rewatching_one_episode_does_not_complete_a_season() {
    // Eight plays of episode 1 used to count as eight episodes of an
    // eight-episode season: "completed", when the household saw one episode.
    let mut season = item("season-r", LibraryKind::Season, 500.0, vec![]);
    season.plays = (1..=8).map(|week| Play { epoch: NOW - 400 * DAY + week * 7 * DAY, episode: Some(1), viewer: None, complete: true }).collect();
    let dataset = panel(&[season], &[90.0], 30.0);
    assert_eq!(dataset[0].card.season_state, Some(SeasonState::Partial));
    assert_eq!(dataset[0].card.episodes_watched, Some(1));
}

#[test]
fn tautulli_silence_counts_at_a_cut_only_where_the_daemon_would_have_counted_it() {
    // Today Tautulli has a play for this movie, 50 days ago. At a 200-day cut
    // it had none: the question is what evidence the daemon held back then.
    let mut movie = item("movie-t", LibraryKind::Movie, 600.0, vec![NOW - 50 * DAY]);
    movie.watch_source = Some(WatchSource::Tautulli);
    let at_cut = |coverage: Option<u64>| {
        let spec = PanelSpec { now: NOW, cuts_days: &[200.0], horizon_days: 30.0, tautulli_coverage_start: coverage };
        build_dataset(std::slice::from_ref(&movie), &spec).remove(0)
    };
    // Tautulli was already recording when it arrived: its silence is evidence.
    let watched = at_cut(Some(NOW - 700 * DAY));
    assert_eq!(watched.ctx.watch_source, Some(WatchSource::TautulliAbsence));
    assert!(watched.values.contains_key("never_played"));
    // Tautulli started after it arrived: silence proves nothing.
    let blind = at_cut(Some(NOW - 300 * DAY));
    assert_eq!(blind.ctx.watch_source, None);
    assert!(blind.values.contains_key("no_evidence"));
    // Not resolved to Plex by catalogue id: the daemon never claims its silence.
    let unresolved = FitItem { guid_resolved: false, ..movie.clone() };
    let spec = PanelSpec { now: NOW, cuts_days: &[200.0], horizon_days: 30.0, tautulli_coverage_start: Some(NOW - 700 * DAY) };
    assert_eq!(build_dataset(&[unresolved], &spec)[0].ctx.watch_source, None);
    // After the play, the play-log source is today's.
    let spec = PanelSpec { now: NOW, cuts_days: &[40.0], horizon_days: 30.0, tautulli_coverage_start: None };
    assert_eq!(build_dataset(&[movie.clone()], &spec)[0].ctx.watch_source, Some(WatchSource::Tautulli));
}

/// One of two otherwise identical groups of seasons carries `signal` at a
/// 120-day cut. `replayed` says which group the household comes back to in the
/// horizon. Every other feature is equal across the groups, so only the signal
/// can explain the labels.
fn household_where(signal: &str, replayed_with_signal: bool) -> Vec<FitItem> {
    let first_watch = NOW - 700 * DAY;
    (0..80)
        .map(|index| {
            let carries = index % 2 == 0;
            let mut season = item(&format!("sonarr-{index}-s1"), LibraryKind::Season, 800.0, vec![]);
            season.show_title = Some(format!("Show {index}"));
            season.plays = (1..=8).map(|episode| Play { epoch: first_watch + episode as u64 * DAY, episode: Some(episode), viewer: None, complete: true }).collect();
            if carries {
                match signal {
                    "rewatched" => season.plays.push(Play { epoch: first_watch + 100 * DAY, episode: Some(1), viewer: None, complete: true }),
                    "viewer_breadth" => {
                        for (offset, name) in ["alice", "bob", "carol"].into_iter().enumerate() {
                            season.plays[offset].viewer = Some(Viewer::TautulliUser(name.to_string()));
                        }
                    }
                    "series_ended" => {
                        season.series_status = Some("ended".to_string());
                        season.last_aired_epoch = Some(first_watch - 30 * DAY);
                    }
                    other => panic!("no synthetic household for {other}"),
                }
            }
            // One in five goes against the pattern, in both groups: a signal,
            // not an oracle.
            let replayed = (carries == replayed_with_signal) != (index % 5 == 1);
            if replayed {
                season.plays.push(Play { epoch: NOW - 100 * DAY, episode: Some(2), viewer: None, complete: true });
            }
            season.audience_plays = season.plays.clone();
            season
        })
        .collect()
}

#[rstest]
fn the_fitter_learns_a_household_signal_in_whichever_direction_the_data_points(
    #[values("rewatched", "viewer_breadth", "series_ended")] signal: &str,
    #[values(true, false)] replayed_with_signal: bool,
) {
    let dataset = panel(&household_where(signal, replayed_with_signal), &[120.0], 30.0);
    assert!(dataset.iter().any(|row| row.values.contains_key(signal)), "{signal} must reach the panel");
    let weight = train::fit_scorecard(&dataset).weights.get(signal).copied().unwrap_or(0.0);
    // Replayed ⇒ not safe to reclaim ⇒ the signal must argue against reclaiming.
    let expected_sign = if replayed_with_signal { -1.0 } else { 1.0 };
    assert!(weight * expected_sign > 0.5, "{signal} learnt {weight}, expected sign {expected_sign}");
}

prop_compose! {
    /// A season and a sibling season of the same show with arbitrary pre-cut
    /// history, a cut date, and a play that happens at or after the cut.
    fn history_and_a_later_play()(
        cut_days in 60u64..400,
        before in proptest::collection::vec((1u64..600, 1u32..9, 0u8..3), 0..12),
        later_days in 0u64..60,
        later_episode in 1u32..9,
        ended in any::<bool>(),
        last_aired_days in 0u64..800,
    ) -> (Vec<FitItem>, f32, u64, u32) {
        let cut = NOW - cut_days * DAY;
        let history: Vec<Play> = before
            .into_iter()
            .map(|(days, episode, viewer)| Play {
                epoch: cut - days * DAY,
                episode: Some(episode),
                viewer: Some(Viewer::TautulliUser(format!("viewer-{viewer}"))),
                complete: true,
            })
            .collect();
        let season = |id: &str, index: u32| {
            let mut season = item(id, LibraryKind::Season, 900.0, vec![]);
            season.season_index = Some(index);
            season.series_status = ended.then(|| "ended".to_string());
            season.last_aired_epoch = Some(NOW - last_aired_days * DAY);
            season
        };
        let mut target = season("sonarr-1-s1", 1);
        target.plays = history.clone();
        target.audience_plays = history;
        let sibling = season("sonarr-1-s2", 2);
        (vec![target, sibling], cut_days as f32, cut + later_days * DAY, later_episode)
    }
}

proptest! {
    /// No post-cut leakage: a play at or after the cut may change the label,
    /// and nothing else — not the exported state, not the features — for the
    /// item itself or for its sibling.
    #[test]
    fn a_play_after_the_cut_changes_the_label_and_nothing_else(
        (items, cut_days, later_epoch, later_episode) in history_and_a_later_play(),
    ) {
        let horizon_days = 30.0;
        let before = panel(&items, &[cut_days], horizon_days);
        let mut with_later_play = items.clone();
        let later = Play { epoch: later_epoch, episode: Some(later_episode), viewer: Some(Viewer::TautulliUser("late".into())), complete: true };
        with_later_play[0].plays.push(later.clone());
        with_later_play[0].audience_plays.push(later.clone());
        with_later_play[1].audience_plays.push(later);
        let after = panel(&with_later_play, &[cut_days], horizon_days);

        prop_assert_eq!(before.len(), after.len());
        for (old, new) in before.iter().zip(&after) {
            let (old_row, new_row) = (PanelRow::of(old, horizon_days), PanelRow::of(new, horizon_days));
            prop_assert_eq!(&old_row.state, &new_row.state);
            prop_assert_eq!(&old_row.features, &new_row.features);
        }
        let target = after.iter().find(|row| row.item_id == "sonarr-1-s1").expect("target row");
        let in_horizon = later_epoch < target.cut_unix + (horizon_days as u64) * DAY;
        prop_assert_eq!(target.label, if in_horizon { 0.0 } else { before[0].label });
    }
}
