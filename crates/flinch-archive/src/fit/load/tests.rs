//! The fitter must see exactly the plays the daemon joined, from state files
//! of any age.

use super::*;

/// A throwaway state directory holding `files`, removed when dropped.
struct StateDir(PathBuf);

impl StateDir {
    fn with(name: &str, files: &[(&str, &str)]) -> Self {
        let dir = std::env::temp_dir().join(format!("flinch-load-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("state dir");
        for (file, body) in files {
            std::fs::write(dir.join(file), body).expect("state file");
        }
        Self(dir)
    }
}

impl Drop for StateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An item's own plays and its audience plays, as epochs.
fn plays_of(household: &Household, id: &str) -> (Vec<u64>, Vec<u64>) {
    let item = household.items.iter().find(|item| item.id == id).expect("item loaded");
    (item.plays.iter().map(|play| play.epoch).collect(), item.audience_plays.iter().map(|play| play.epoch).collect())
}

#[test]
fn plays_from_before_a_re_add_attach_by_the_plex_guids_the_daemon_kept() {
    let items = r#"[
        {"id":"radarr-1","title":"Heat","kind":"movie","age_days":20.0,"year":1995,
         "play_keys":{"kind":"movie","rating_keys":["100"],"plex_guids":["plex://movie/heat"]}},
        {"id":"sonarr-7-s1","title":"Andor S1","kind":"season","age_days":20.0,"season_label":"S1","episodes":2,
         "play_keys":{"kind":"season","show_rating_keys":["70"],"season_rating_keys":["71"],"season":1,"episode_guids":["plex://episode/e1"]}}
    ]"#;
    // Every stream predates the re-add: its ratingKeys are gone.
    let streams = r#"[
        {"media_type":"movie","rating_key":"7","title":"Heat","year":"1995","guid":"plex://movie/heat","date":"1000","percent_complete":"100"},
        {"media_type":"movie","rating_key":"8","title":"Heat","year":"1995","date":"1100","percent_complete":"100"},
        {"media_type":"episode","rating_key":"501","parent_rating_key":"51","grandparent_rating_key":"50","parent_media_index":"1","media_index":"1","guid":"plex://episode/e1","date":"2000","percent_complete":"100"},
        {"media_type":"episode","rating_key":"511","parent_rating_key":"52","grandparent_rating_key":"50","parent_media_index":"2","media_index":"1","guid":"plex://episode/e3","date":"2100","percent_complete":"100"}
    ]"#;
    let state = StateDir::with("guid-join", &[("items.json", items), ("tautulli.json", streams)]);
    let household = load_household(&state.0).expect("household loads");
    assert_eq!(plays_of(&household, "radarr-1"), (vec![1000], vec![1000]), "the GUID play joins; the same title and year alone does not");
    assert_eq!(plays_of(&household, "sonarr-7-s1"), (vec![2000], vec![2000]), "only the episode Plex files under season 1");
}

#[test]
fn items_json_written_before_the_guid_keys_still_loads_with_its_rating_key_plays() {
    let items = r#"[
        {"id":"radarr-2","title":"Dune","kind":"movie","age_days":30.0,"play_keys":{"kind":"movie","rating_keys":["40"]}},
        {"id":"sonarr-7-s1","title":"Andor S1","kind":"season","age_days":30.0,"season_label":"S1",
         "play_keys":{"kind":"season","show_rating_keys":["70"],"season_rating_keys":["71"],"season":1}}
    ]"#;
    let streams = r#"[
        {"media_type":"movie","rating_key":"40","date":"3000","percent_complete":"100"},
        {"media_type":"episode","rating_key":"701","parent_rating_key":"71","grandparent_rating_key":"70","parent_media_index":"1","media_index":"1","date":"3100","percent_complete":"100"}
    ]"#;
    let state = StateDir::with("old-keys", &[("items.json", items), ("tautulli.json", streams)]);
    let household = load_household(&state.0).expect("household loads");
    assert_eq!(household.unreadable_rows, 0);
    assert_eq!(plays_of(&household, "radarr-2").0, vec![3000]);
    assert_eq!(plays_of(&household, "sonarr-7-s1").0, vec![3100]);
}

#[test]
fn rows_written_before_presence_history_keep_todays_arrival_rule() {
    use crate::fit::panel::{build_dataset, PanelSpec};
    const NOW: u64 = 1_800_000_000;
    // Both files say 20 days on disk and both were played 190 days ago
    // (1_783_584_000). Only the second row carries history: back 100 days ago
    // (1_791_360_000), after a stretch it was gone.
    let items = r#"[
        {"id":"radarr-1","title":"Heat","kind":"movie","age_days":20.0,"play_keys":{"kind":"movie","rating_keys":["100"]}},
        {"id":"radarr-2","title":"Dune","kind":"movie","age_days":20.0,"play_keys":{"kind":"movie","rating_keys":["200"]},
         "on_disk":[{"from":1791360000}]}
    ]"#;
    let streams = r#"[
        {"media_type":"movie","rating_key":"100","date":"1783584000","percent_complete":"100"},
        {"media_type":"movie","rating_key":"200","date":"1783584000","percent_complete":"100"}
    ]"#;
    let state = StateDir::with("presence", &[("items.json", items), ("tautulli.json", streams)]);
    let household = load_household(&state.0).expect("household loads");
    assert_eq!(household.unreadable_rows, 0, "a row without on_disk is still a library item");
    let spec = PanelSpec { now: NOW, cuts_days: &[150.0], horizon_days: 30.0, tautulli_coverage_start: None };
    let asked: Vec<(String, f32)> =
        build_dataset(&household.items, &spec).into_iter().map(|row| (row.item_id, row.card.added_days_ago)).collect();
    // The old row is dated by its first play; the new one was not on disk then.
    assert_eq!(asked, [("radarr-1".to_string(), 40.0)]);
}
