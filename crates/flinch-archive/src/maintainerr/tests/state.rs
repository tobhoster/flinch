//! The ownership files: their shape, and the fail-safe reader.

use super::super::{OwnedState, ProtectedEntry, ScheduledEntry};
use super::{movie, season, SEASONS};
use rstest::rstest;
use std::path::PathBuf;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flinch-owned-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn owned_state_round_trips_in_the_plex_ids_shape() {
    let dir = scratch("round-trip");
    let mut owned = OwnedState::default();
    owned.protected.insert("radarr-7".into(), ProtectedEntry { target: movie("812"), exclusion_ids: vec![3] });
    owned.scheduled.insert(
        "sonarr-12-s3".into(),
        ScheduledEntry { target: season("4500", "4511"), collection_id: SEASONS, added_at: 1_800_000_000 },
    );

    owned.write(&dir).unwrap();

    assert_eq!(OwnedState::read(&dir), owned);
    let scheduled: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("scheduled.json")).unwrap()).unwrap();
    assert_eq!(
        scheduled,
        serde_json::json!({"sonarr-12-s3": {"rating_key": "4500", "season_rating_key": "4511", "collection_id": SEASONS, "added_at": 1_800_000_000u64}})
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[rstest]
#[case::legacy_card_id_set("legacy", r#"["radarr-10","sonarr-7-s2"]"#)]
#[case::corrupt("corrupt", "{\"radarr-7\": {\"rating_key\": ")]
#[case::blank_key("blank", r#"{"radarr-7": {"rating_key": " ", "exclusion_ids": [3]}}"#)]
fn an_unusable_protected_file_owns_nothing(#[case] name: &str, #[case] content: &str) {
    let dir = scratch(name);
    std::fs::write(dir.join("protected.json"), content).unwrap();

    assert_eq!(OwnedState::read(&dir), OwnedState::default());
    std::fs::remove_dir_all(&dir).ok();
}
