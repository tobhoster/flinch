//! The eviction ledger: credit from disappearance until the bin empties.

use super::*;

const GB: u64 = 1_000_000_000;

fn three_days(_: App) -> u64 {
    3 * 86_400
}

#[test]
fn the_ledger_credits_an_item_from_its_disappearance_until_its_bin_empties() {
    let mut ledger = EvictionLedger::default();
    ledger.record("radarr-1", App::Radarr, "/media", 10 * GB, 1_000);
    ledger.observe(|_| true, three_days, 2_000);
    assert!(ledger.pending_bytes().is_empty(), "still in Maintainerr's collection: nothing freed-but-held yet");
    ledger.observe(|_| false, three_days, 5_000);
    assert_eq!(ledger.pending_bytes().get("/media"), Some(&(10 * GB)));
    ledger.observe(|_| false, three_days, 5_000 + 3 * 86_400 - 1);
    assert_eq!(ledger.pending_bytes().get("/media"), Some(&(10 * GB)), "inside the window, credited from first sighting");
    ledger.observe(|_| false, three_days, 5_000 + 3 * 86_400);
    assert!(ledger.entries.is_empty(), "past the window the measurement shows the space itself");
}

#[test]
fn a_re_downloaded_item_loses_its_credit() {
    let mut ledger = EvictionLedger::default();
    ledger.record("sonarr-4-s2", App::Sonarr, "/tv", 8 * GB, 0);
    ledger.observe(|_| false, three_days, 100);
    ledger.observe(|_| true, three_days, 200);
    assert!(ledger.pending_bytes().is_empty(), "back on disk occupies its bytes again");
}

#[test]
fn an_item_left_on_disk_for_months_stops_being_tracked() {
    let mut ledger = EvictionLedger::default();
    ledger.record("radarr-9", App::Radarr, "/media", GB, 0);
    ledger.observe(|_| true, three_days, STALE_ON_DISK_SECS);
    assert!(ledger.entries.is_empty());
}

#[test]
fn re_recording_keeps_the_original_hand_over() {
    let mut ledger = EvictionLedger::default();
    ledger.record("radarr-1", App::Radarr, "/media", 10 * GB, 1_000);
    ledger.record("radarr-1", App::Radarr, "/media", 10 * GB, 9_000);
    assert_eq!(ledger.entries["radarr-1"].handed_at, 1_000);
}

#[test]
fn the_ledger_round_trips_and_a_corrupt_file_reads_empty() {
    let path = std::env::temp_dir().join(format!("flinch-ledger-{}.json", std::process::id()));
    let mut ledger = EvictionLedger::default();
    ledger.record("radarr-1", App::Radarr, "/media", GB, 7);
    ledger.write(&path).expect("write ledger");
    assert_eq!(EvictionLedger::read(&path), ledger);
    std::fs::write(&path, b"[").expect("corrupt it");
    assert_eq!(EvictionLedger::read(&path), EvictionLedger::default(), "a corrupt ledger is no credit, not phantom credit");
    std::fs::remove_file(&path).ok();
}
