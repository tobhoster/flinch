use super::*;
use rstest::rstest;

const NOW: u64 = 2_000_000_000;

fn play(days_ago: u64, fraction: f32, episode: Option<u32>, viewer: &str) -> Play {
    Play { epoch: NOW - days_ago * DAY_SECS, episode, viewer: Some(Viewer::TautulliUser(viewer.to_string())), fraction }
}

fn read(item: &[Play], audience: &[Play], episodes_total: Option<u32>, added_days_ago: f32) -> WatchFeatures {
    let item: Vec<&Play> = item.iter().collect();
    let audience: Vec<&Play> = audience.iter().collect();
    WatchFeatures::read(&PlayHistory { item: &item, audience: &audience, episodes_total, last_watched_days: None, added_days_ago }, NOW)
}

fn cold(days: f64) -> WatchFeatures {
    WatchFeatures {
        days_since_last_play: days,
        never_played: false,
        lifetime_scrobbles: 1.0,
        active_series_velocity: 0,
        annual_cyclical_offset: 0.0,
        partway: false,
    }
}

#[test]
fn a_show_being_watched_is_likely_watched_and_a_finished_title_is_not() {
    let model = HazardModel::default();
    let binge = WatchFeatures { active_series_velocity: 10, ..cold(1.0) };
    assert!(model.p_watch(&binge) > 0.5, "{}", model.p_watch(&binge));
    assert!(model.p_watch(&cold(1.0)) < 0.5, "finished yesterday is rarely replayed: {}", model.p_watch(&cold(1.0)));
    assert!(model.p_watch(&cold(365.0)) < 0.05, "{}", model.p_watch(&cold(365.0)));
}

#[test]
fn every_positive_signal_raises_the_chance_of_a_watch() {
    let model = HazardModel::default();
    let base = cold(120.0);
    for raised in [
        WatchFeatures { days_since_last_play: 10.0, ..base },
        WatchFeatures { active_series_velocity: 6, ..base },
        WatchFeatures { annual_cyclical_offset: 1.0, ..base },
    ] {
        assert!(model.p_watch(&raised) > model.p_watch(&base), "{raised:?}");
    }
}

#[test]
fn someone_partway_through_holds_the_chance_at_ninety_five_percent() {
    let model = HazardModel::default();
    assert_eq!(model.p_watch(&WatchFeatures { partway: true, ..cold(365.0) }), PARTWAY_FLOOR);
}

#[test]
fn a_never_played_item_is_censored_at_its_days_on_disk() {
    let features = read(&[], &[], None, 200.0);
    assert!(features.never_played);
    assert_eq!(features.days_since_last_play, 200.0);
    assert_eq!(features.annual_cyclical_offset, 0.0, "no play, no season to recur");
}

#[rstest]
#[case::movie_stopped_halfway(&[play(3, 0.5, None, "ann")], None, true)]
#[case::movie_barely_started(&[play(3, 0.05, None, "ann")], None, false)]
#[case::movie_finished(&[play(3, 1.0, None, "ann")], None, false)]
#[case::movie_abandoned_long_ago(&[play(90, 0.5, None, "ann")], None, false)]
#[case::season_three_of_ten(&[play(3, 1.0, Some(1), "ann"), play(2, 1.0, Some(2), "ann"), play(1, 1.0, Some(3), "ann")], Some(10), true)]
#[case::season_done(&[play(1, 1.0, Some(1), "ann"), play(1, 1.0, Some(2), "ann")], Some(2), false)]
fn partway_needs_an_active_viewer_between_ten_and_ninety_percent(
    #[case] plays: &[Play],
    #[case] episodes: Option<u32>,
    #[case] expected: bool,
) {
    assert_eq!(read(plays, &[], episodes, 400.0).partway, expected);
}

#[test]
fn a_finished_season_counts_one_viewing_and_show_plays_set_the_velocity() {
    let season: Vec<Play> = (1..=8).map(|episode| play(20, 1.0, Some(episode), "ann")).collect();
    let show = [play(1, 1.0, Some(1), "bo"), play(5, 1.0, Some(2), "bo"), play(30, 1.0, Some(3), "bo")];
    let features = read(&season, &show, Some(8), 400.0);
    assert_eq!(features.lifetime_scrobbles, 1.0);
    assert_eq!(features.active_series_velocity, 2, "only the last 14 days");
}

#[rstest]
#[case::one_gigabyte_unknown_seeders(1_000_000_000, None, false, 1.0)]
#[case::ten_gigabytes(10_000_000_000, None, false, 1.3)]
#[case::one_seeder(1_000_000_000, Some(1), false, 3.0)]
#[case::zero_seeders_count_as_one(1_000_000_000, Some(0), false, 3.0)]
#[case::out_of_retention(1_000_000_000, Some(100), true, 6.02)]
#[case::a_tiny_file_is_never_free(1, None, false, MIN_FRICTION)]
fn friction_follows_size_seeders_and_retention(#[case] bytes: u64, #[case] seeders: Option<u32>, #[case] oor: bool, #[case] expected: f64) {
    let friction = Reacquisition { size_bytes: bytes, seeders, usenet_out_of_retention: oor }.friction();
    assert!((friction - expected).abs() < 1e-9, "{friction}");
}

#[test]
fn the_strongest_weighted_claim_sets_the_household_weight() {
    assert_eq!(household([]), 1.0, "unclaimed keeps its own P × C");
    let claims = [
        Claim { weight: 1.0, watchlisted: false, requested: true },
        Claim { weight: 0.5, watchlisted: true, requested: true },
        Claim { weight: -3.0, watchlisted: true, requested: false },
    ];
    assert_eq!(household(claims), 2.75, "max(1.5, 0.5·3.5, 0) + 1");
}
