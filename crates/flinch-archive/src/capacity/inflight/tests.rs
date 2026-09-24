//! The eviction ledger: credit from disappearance until the disk shows the
//! space back — or holds on to it.

use super::*;

const GB: u64 = 1_000_000_000;
const DAY: u64 = 86_400;

fn three_days(_: App) -> u64 {
    3 * DAY
}

fn no_bin(_: App) -> u64 {
    0
}

/// The one volume's reading, in GB.
fn disk(used_gb: u64, library_gb: u64) -> BTreeMap<String, Occupancy> {
    BTreeMap::from([("/media".to_string(), Occupancy { used: used_gb * GB, library: library_gb * GB })])
}

fn hand(ledger: &mut EvictionLedger, id: &str, gb: u64, now: u64) {
    ledger.record(HandedOver { id, title: id, app: App::Radarr, volume: "/media", bytes: gb * GB }, now);
}

fn credit(ledger: &EvictionLedger) -> Credit {
    ledger.credits().get("/media").copied().unwrap_or_default()
}

/// A 10 GB movie handed over at 0 on a disk 500 GB full, 110 GB of it
/// library (the movie included), deleted into the recycle bin at `gone`.
fn deleted_at(gone: u64) -> EvictionLedger {
    let mut ledger = EvictionLedger::default();
    hand(&mut ledger, "radarr-1", 10, 0);
    ledger.observe(|_| true, three_days, &disk(500, 110), gone - 60);
    ledger.observe(|_| false, three_days, &disk(500, 100), gone);
    ledger
}

#[test]
fn an_item_is_credited_from_its_disappearance_until_the_disk_shows_it_freed() {
    let mut ledger = EvictionLedger::default();
    hand(&mut ledger, "radarr-1", 10, 1_000);
    ledger.observe(|_| true, three_days, &disk(500, 110), 2_000);
    assert_eq!(credit(&ledger), Credit::default(), "still in Maintainerr's collection: nothing freed-but-held yet");
    ledger.observe(|_| false, three_days, &disk(500, 100), 5_000);
    assert_eq!(credit(&ledger).pending, 10 * GB);
    ledger.observe(|_| false, three_days, &disk(490, 100), 5_000 + 3 * DAY - 1);
    assert_eq!(credit(&ledger).pending, 10 * GB, "inside the window, credited from first sighting");
    ledger.observe(|_| false, three_days, &disk(490, 100), 5_000 + 3 * DAY);
    assert!(ledger.entries.is_empty(), "past the window the disk shows the space itself");
}

#[test]
fn bytes_the_disk_never_releases_are_held_and_stay_credited_for_a_while() {
    let mut ledger = deleted_at(DAY);
    let due = DAY + 3 * DAY;
    ledger.observe(|_| false, three_days, &disk(500, 100), due + SETTLE_GRACE_SECS - 1);
    assert_eq!(credit(&ledger), Credit { pending: 10 * GB, held: 0 }, "the bin may still empty today");
    ledger.observe(|_| false, three_days, &disk(500, 100), due + SETTLE_GRACE_SECS);
    assert_eq!(credit(&ledger), Credit { pending: 0, held: 10 * GB }, "no drop: still credited, so nothing more is evicted for it");
    let held = &ledger.held()["/media"];
    assert_eq!((held[0].id.as_str(), held[0].until), ("radarr-1", due + SETTLE_GRACE_SECS + HELD_CREDIT_SECS));
    ledger.observe(|_| false, three_days, &disk(500, 100), due + SETTLE_GRACE_SECS + HELD_CREDIT_SECS);
    assert!(ledger.entries.is_empty(), "past the held window the measurement alone drives eviction again");
}

#[test]
fn held_bytes_that_free_later_end_their_credit() {
    let mut ledger = deleted_at(DAY);
    let held_at = DAY + 3 * DAY + SETTLE_GRACE_SECS;
    ledger.observe(|_| false, three_days, &disk(500, 100), held_at);
    assert_eq!(credit(&ledger).held, 10 * GB);
    // The torrent that shared the file finished seeding and was removed.
    ledger.observe(|_| false, three_days, &disk(490, 100), held_at + DAY);
    assert!(ledger.entries.is_empty());
}

#[test]
fn a_drop_a_download_hides_shows_once_the_download_imports() {
    let mut ledger = deleted_at(DAY);
    let due = DAY + 3 * DAY;
    // The bin empties (−10 GB) while a 30 GB download lands: no net drop.
    ledger.observe(|_| false, three_days, &disk(520, 100), due + 3_600);
    ledger.observe(|_| false, three_days, &disk(520, 100), due + SETTLE_GRACE_SECS);
    assert_eq!(credit(&ledger).held, 10 * GB, "the error is a held report and less eviction, never more");
    // The download imports: its 30 GB are library now.
    ledger.observe(|_| false, three_days, &disk(520, 130), due + SETTLE_GRACE_SECS + 3_600);
    assert!(ledger.entries.is_empty(), "the hidden drop shows");
}

#[test]
fn without_a_recycle_bin_a_drop_between_readings_still_counts() {
    let mut ledger = EvictionLedger::default();
    hand(&mut ledger, "radarr-1", 10, 0);
    ledger.observe(|_| true, no_bin, &disk(500, 110), 100);
    // Deleted outright: the first reading without it is already 10 GB lighter.
    ledger.observe(|_| false, no_bin, &disk(490, 100), 200);
    assert!(ledger.entries.is_empty());
}

#[test]
fn one_drop_pays_for_one_eviction_only() {
    let mut ledger = EvictionLedger::default();
    hand(&mut ledger, "radarr-1", 10, 0);
    hand(&mut ledger, "radarr-2", 8, 0);
    ledger.observe(|_| true, three_days, &disk(500, 118), 100);
    ledger.observe(|_| false, three_days, &disk(500, 100), 200);
    // The bin releases the first; a torrent keeps seeding the second.
    let due = 200 + 3 * DAY;
    ledger.observe(|_| false, three_days, &disk(490, 100), due);
    ledger.observe(|_| false, three_days, &disk(490, 100), due + SETTLE_GRACE_SECS);
    assert_eq!(ledger.entries.keys().collect::<Vec<_>>(), ["radarr-2"]);
    assert_eq!(credit(&ledger), Credit { pending: 0, held: 8 * GB });
}

#[test]
fn an_eviction_gone_before_the_first_reading_is_never_judged_held() {
    let mut ledger = EvictionLedger::default();
    hand(&mut ledger, "radarr-1", 10, 0);
    // First reading ever: the bin may have emptied before it, unseen.
    ledger.observe(|_| false, three_days, &disk(490, 100), 100);
    ledger.observe(|_| false, three_days, &disk(490, 100), 100 + 3 * DAY);
    assert!(ledger.entries.is_empty(), "credited through its window, then left to the measurement");
}

#[test]
fn an_unmeasured_disk_gives_no_verdict() {
    let mut ledger = deleted_at(DAY);
    ledger.observe(|_| false, three_days, &BTreeMap::new(), DAY + 3 * DAY + SETTLE_GRACE_SECS);
    assert_eq!(credit(&ledger), Credit { pending: 10 * GB, held: 0 });
}

#[test]
fn a_re_downloaded_item_loses_its_credit() {
    let mut ledger = EvictionLedger::default();
    ledger.record(HandedOver { id: "sonarr-4-s2", title: "Show S2", app: App::Sonarr, volume: "/tv", bytes: 8 * GB }, 0);
    ledger.observe(|_| false, three_days, &BTreeMap::new(), 100);
    ledger.observe(|_| true, three_days, &BTreeMap::new(), 200);
    assert!(ledger.credits().is_empty(), "back on disk occupies its bytes again");
}

#[test]
fn an_item_left_on_disk_for_months_stops_being_tracked() {
    let mut ledger = EvictionLedger::default();
    hand(&mut ledger, "radarr-9", 1, 0);
    ledger.observe(|_| true, three_days, &BTreeMap::new(), STALE_ON_DISK_SECS);
    assert!(ledger.entries.is_empty());
}

#[test]
fn a_bin_that_is_never_emptied_keeps_its_credit_through_its_whole_window() {
    let never_emptied = |_: App| STALE_ON_DISK_SECS;
    let mut ledger = EvictionLedger::default();
    hand(&mut ledger, "radarr-1", 10, 0);
    // Maintainerr deleted it ten days after the hand-over.
    ledger.observe(|_| false, never_emptied, &BTreeMap::new(), 10 * DAY);
    ledger.observe(|_| false, never_emptied, &BTreeMap::new(), 95 * DAY);
    assert_eq!(credit(&ledger).pending, 10 * GB, "credited until its bin's window ends, not ninety days after the hand-over");
}

#[test]
fn re_recording_keeps_the_original_hand_over() {
    let mut ledger = EvictionLedger::default();
    hand(&mut ledger, "radarr-1", 10, 1_000);
    hand(&mut ledger, "radarr-1", 10, 9_000);
    assert_eq!((ledger.entries["radarr-1"].handed_at, ledger.handoffs["radarr-1"]), (1_000, 1_000));
}

#[test]
fn a_hand_over_is_remembered_while_tracked_and_well_past_it_unless_taken_back() {
    let mut ledger = EvictionLedger::default();
    hand(&mut ledger, "radarr-1", 1, 0);
    hand(&mut ledger, "radarr-2", 1, 0);
    ledger.forget("radarr-2");
    assert_eq!(ledger.handoffs.keys().collect::<Vec<_>>(), ["radarr-1"], "taken back: a later deletion is not FLINCH's");
    ledger.observe(|_| true, three_days, &BTreeMap::new(), STALE_ON_DISK_SECS);
    assert!(ledger.entries.is_empty() && ledger.handoffs.contains_key("radarr-1"), "no longer tracked, still remembered");
    ledger.observe(|_| true, three_days, &BTreeMap::new(), HANDOFF_MEMORY_SECS);
    assert!(ledger.handoffs.is_empty());
}

#[test]
fn a_ledger_written_before_hand_overs_were_remembered_backfills_them() {
    let mut ledger = EvictionLedger::default();
    hand(&mut ledger, "radarr-1", 1, 7);
    ledger.handoffs.clear();
    ledger.observe(|_| true, three_days, &BTreeMap::new(), 10);
    assert_eq!(ledger.handoffs.get("radarr-1"), Some(&7));
}

#[test]
fn the_ledger_round_trips_and_a_corrupt_file_reads_empty() {
    let path = std::env::temp_dir().join(format!("flinch-ledger-{}.json", std::process::id()));
    let mut ledger = deleted_at(DAY);
    ledger.observe(|_| false, three_days, &disk(500, 100), DAY + 3 * DAY + SETTLE_GRACE_SECS);
    ledger.write(&path).expect("write ledger");
    assert_eq!(EvictionLedger::read(&path), ledger);
    std::fs::write(&path, b"[").expect("corrupt it");
    assert_eq!(EvictionLedger::read(&path), EvictionLedger::default(), "a corrupt ledger is no credit, not phantom credit");
    std::fs::remove_file(&path).ok();
}
