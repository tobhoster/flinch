//! Taste contracts: rates learnt only from outcomes closed by the as-of date,
//! genres that tell the household's preferences apart, and no taste where
//! there is nothing to learn from.

use super::*;
use crate::card::LibraryKind;
use crate::fit::panel::{build_dataset, PanelSpec};
use crate::fit::plays::Play;

const NOW: u64 = 1_800_000_000;
const DAY: u64 = 86_400;

fn item(id: &str, genres: &[&str], age_days: f32, plays: &[u64]) -> FitItem {
    let plays: Vec<Play> = plays.iter().map(|&epoch| Play { epoch, episode: None, viewer: None, complete: true }).collect();
    FitItem {
        id: id.to_string(),
        title: id.to_string(),
        kind: LibraryKind::Movie,
        size_bytes: 8_000_000_000,
        age_days,
        episodes_total: None,
        season_index: None,
        show_title: None,
        audience_plays: plays.clone(),
        plays,
        watch_source: None,
        is_newest_season: false,
        series_status: None,
        last_aired_epoch: None,
        guid_resolved: true,
        genres: genres.iter().map(|genre| genre.to_string()).collect(),
        on_disk: Vec::new(),
    }
}

fn panel(items: &[FitItem], cuts_days: &[f32]) -> Vec<Example> {
    build_dataset(items, &PanelSpec { now: NOW, cuts_days, horizon_days: 30.0, tautulli_coverage_start: None })
}

fn taste_of(dataset: &[Example], id: &str, cut_days: f32) -> Option<f32> {
    dataset.iter().find(|row| row.item_id == id && row.cut_days == cut_days).expect("the item has a row at the cut").ctx.taste
}

#[test]
fn a_household_that_plays_comedies_expects_a_new_comedy_to_be_played_more_than_a_new_horror() {
    // Four comedies, each played every 20 days for a year; four horrors never touched.
    let every_20_days: Vec<u64> = (1..18).map(|step| NOW - step * 20 * DAY).collect();
    let mut items: Vec<FitItem> = (0..4).map(|index| item(&format!("comedy-{index}"), &["Comedy"], 500.0, &every_20_days)).collect();
    items.extend((0..4).map(|index| item(&format!("horror-{index}"), &["Horror"], 500.0, &[])));
    items.push(item("new-comedy", &["Comedy"], 40.0, &[]));
    items.push(item("new-horror", &["Horror"], 40.0, &[]));
    let dataset = panel(&items, &[30.0, 90.0, 150.0, 210.0, 270.0]);

    let comedy = taste_of(&dataset, "new-comedy", 30.0).expect("closed outcomes and a genre");
    let horror = taste_of(&dataset, "new-horror", 30.0).expect("closed outcomes and a genre");
    assert!(comedy > horror, "comedy {comedy} must beat horror {horror}");
    let row = dataset.iter().find(|row| row.item_id == "new-horror" && row.cut_days == 30.0).expect("row");
    assert!(row.values.get("taste").is_some_and(|value| *value > 0.0), "an unwatched genre reads as likely safe");
}

/// A 30-day horizon at cuts 150, 90 and 30 days ago: rows cut at 150 close
/// before the 90-day cut; rows cut at 90 close only before the 30-day cut.
fn leak_household(comedy_play: Option<u64>) -> Vec<Example> {
    let items = [
        item("target", &["Comedy"], 400.0, &[]),
        item("comedy", &["Comedy"], 400.0, comedy_play.as_slice()),
        item("horror", &["Horror"], 400.0, &[]),
        item("untagged", &[], 400.0, &[]),
    ];
    panel(&items, &[30.0, 90.0, 150.0])
}

#[test]
fn a_rows_taste_uses_only_outcomes_closed_before_its_cut() {
    // The play at 80 days ago falls in the window of the row cut at 90 days,
    // which closes 60 days ago: after the 90-day cut, before the 30-day cut.
    let played = leak_household(Some(NOW - 80 * DAY));
    let unplayed = leak_household(None);
    let at_90 = taste_of(&played, "target", 90.0);
    assert!(at_90.is_some(), "the rows cut at 150 days had closed");
    assert_eq!(at_90, taste_of(&unplayed, "target", 90.0), "an outcome still open at the cut must not move its taste");
    let (after, before) = (taste_of(&played, "target", 30.0), taste_of(&unplayed, "target", 30.0));
    assert!(after > before, "once closed, the play must count: {after:?} vs {before:?}");
}

#[test]
fn no_closed_outcome_no_genre_or_a_play_means_no_taste() {
    let dataset = leak_household(Some(NOW - 80 * DAY));
    assert_eq!(taste_of(&dataset, "target", 150.0), None, "nothing had closed by the earliest cut");
    assert_eq!(taste_of(&dataset, "untagged", 30.0), None, "no genre, nothing to learn from");
    assert_eq!(taste_of(&dataset, "comedy", 30.0), None, "its own play speaks for it");
    assert_eq!(GenreRates::default().played(&["Comedy".to_string()]), None);
}

#[test]
fn genres_combine_between_their_own_rates_without_double_counting() {
    let rates = GenreRates {
        as_of: NOW,
        overall: Outcomes { played: 1000, total: 2000 },
        genres: BTreeMap::from([
            ("comedy".to_string(), Outcomes { played: 800, total: 1000 }),
            ("romance".to_string(), Outcomes { played: 800, total: 1000 }),
            ("horror".to_string(), Outcomes { played: 50, total: 1000 }),
        ]),
    };
    let names = |genres: &[&str]| genres.iter().map(|genre| genre.to_string()).collect::<Vec<_>>();
    let comedy = rates.played(&names(&["Comedy"])).expect("rate");
    let horror = rates.played(&names(&["Horror"])).expect("rate");
    let both = rates.played(&names(&["Comedy", "Horror"])).expect("rate");
    assert!(horror < both && both < comedy, "a mixed title sits between its genres");
    let agreeing = rates.played(&names(&["Comedy", "Romance"])).expect("rate");
    assert!((agreeing - comedy).abs() < 1e-6, "two genres saying the same thing are not twice the evidence");
    assert_eq!(rates.played(&names(&[" comedy ", "Comedy"])), Some(comedy), "one genre, however it is spelt");
    assert_eq!(rates.played(&names(&["Western"])), Some(0.5), "an unseen genre is the household's overall rate");
}
