//! flinch-demo — write a demo state snapshot so the UI can be tried without a
//! daemon, Radarr, Sonarr or Plex.
//!
//! The snapshot is real daemon output with every title renamed and every Plex,
//! TMDB and *arr id removed (`demo/anonymize.py` makes it). It is written to
//! `$FLINCH_STATE_DIR` (default `state`) with its timestamps moved so the last
//! run was four minutes ago, and `status.json` carries `"demo": true`. It never
//! overwrites a real daemon's snapshot.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const STATUS: &str = include_str!("../../demo/status.json");
const ITEMS: &str = include_str!("../../demo/items.json");
const HISTORY: &str = include_str!("../../demo/history.json");

/// How long before now the snapshot's last run appears to have been.
const RUN_AGE_S: i64 = 240;

/// Every unix-seconds field in the snapshot, as JSON pointers: into status.json,
/// into each item, into each item's `on_disk` span and into each history point.
const STATUS_TIMES: &[&str] = &[
    "/ran_at_unix",
    "/next_run_unix",
    "/last_error_at",
    "/fit/fitted_at_unix",
    "/fit/metrics/fitted_at_unix",
    "/fit/taste/as_of",
    "/benchmark/scored_at_unix",
    "/benchmark/result/now_unix",
];
const ITEM_TIMES: &[&str] = &["/last_aired_epoch", "/handed_at", "/leaves_at"];
const SPAN_TIMES: &[&str] = &["/from", "/to"];
const HISTORY_TIMES: &[&str] = &["/ran_at_unix"];
/// Into each `outside_deletions` entry, and each held eviction of each volume.
const OUTSIDE_TIMES: &[&str] = &["/at_unix"];
const HELD_TIMES: &[&str] = &["/held_since", "/until"];

struct Snapshot {
    status: Value,
    items: Value,
    history: Value,
}

fn main() -> Result<()> {
    let dir = PathBuf::from(std::env::var("FLINCH_STATE_DIR").unwrap_or_else(|_| "state".to_string()));
    refuse_real_state(&dir)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).context("system clock is before 1970")?.as_secs();
    let snapshot = build(i64::try_from(now).context("system clock out of range")?)?;
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    for (name, value) in [("status.json", &snapshot.status), ("items.json", &snapshot.items), ("history.json", &snapshot.history)] {
        let path = dir.join(name);
        std::fs::write(&path, serde_json::to_string_pretty(value)?).with_context(|| format!("write {}", path.display()))?;
    }
    let items = snapshot.items.as_array().map_or(0, Vec::len);
    println!("flinch-demo: wrote a demo snapshot ({items} items, last run 4 minutes ago) to {}", dir.display());
    Ok(())
}

/// The embedded snapshot, moved in time so its last run was `RUN_AGE_S` before `now`.
fn build(now: i64) -> Result<Snapshot> {
    let mut snapshot = Snapshot {
        status: serde_json::from_str(STATUS).context("embedded status.json")?,
        items: serde_json::from_str(ITEMS).context("embedded items.json")?,
        history: serde_json::from_str(HISTORY).context("embedded history.json")?,
    };
    let ran_at = snapshot.status["ran_at_unix"].as_i64().context("embedded status.json has no ran_at_unix")?;
    let delta = now - RUN_AGE_S - ran_at;
    shift(&mut snapshot.status, STATUS_TIMES, delta);
    for deletion in snapshot.status.get_mut("outside_deletions").and_then(Value::as_array_mut).into_iter().flatten() {
        shift(deletion, OUTSIDE_TIMES, delta);
    }
    let volumes = snapshot.status.pointer_mut("/capacity/volumes").and_then(Value::as_array_mut);
    for volume in volumes.into_iter().flatten() {
        for held in volume.get_mut("held").and_then(Value::as_array_mut).into_iter().flatten() {
            shift(held, HELD_TIMES, delta);
        }
    }
    for item in snapshot.items.as_array_mut().into_iter().flatten() {
        shift(item, ITEM_TIMES, delta);
        for span in item.get_mut("on_disk").and_then(Value::as_array_mut).into_iter().flatten() {
            shift(span, SPAN_TIMES, delta);
        }
    }
    for point in snapshot.history.as_array_mut().into_iter().flatten() {
        shift(point, HISTORY_TIMES, delta);
    }
    if let Some(status) = snapshot.status.as_object_mut() {
        status.insert("demo".to_string(), Value::Bool(true));
    }
    Ok(snapshot)
}

/// Adds `delta` to each numeric field at `pointers`; absent and null fields stay as they are.
fn shift(value: &mut Value, pointers: &[&str], delta: i64) {
    for pointer in pointers {
        if let Some(field) = value.pointer_mut(pointer) {
            if let Some(seconds) = field.as_i64() {
                *field = Value::from(seconds + delta);
            }
        }
    }
}

/// Errors unless `dir` is empty of state or already holds a demo snapshot.
fn refuse_real_state(dir: &Path) -> Result<()> {
    let path = dir.join("status.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let is_demo = serde_json::from_str::<Value>(&text).is_ok_and(|status| status["demo"] == Value::Bool(true));
    if !is_demo {
        bail!(
            "{} is not a demo snapshot: refusing to overwrite a real daemon's state. Point FLINCH_STATE_DIR at an empty directory.",
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flinch_archive::daemon::HistoryPoint;
    use flinch_archive::{ItemSnapshot, StatusSnapshot};

    const NOW: i64 = 2_000_000_000;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("flinch-demo-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_last_run_lands_four_minutes_ago_and_offsets_survive() {
        let original = build(NOW).unwrap();
        let embedded: Value = serde_json::from_str(STATUS).unwrap();
        let items: Value = serde_json::from_str(ITEMS).unwrap();
        assert_eq!(original.status["ran_at_unix"], NOW - 240);
        let gap = |status: &Value| status["ran_at_unix"].as_i64().unwrap() - status["fit"]["fitted_at_unix"].as_i64().unwrap();
        assert_eq!(gap(&original.status), gap(&embedded));
        let from = |items: &Value| items[0]["on_disk"][0]["from"].as_i64().unwrap();
        let ran = embedded["ran_at_unix"].as_i64().unwrap();
        assert_eq!(from(&original.items) - (NOW - 240), from(&items) - ran);
        assert_eq!(original.status["demo"], true);
    }

    #[test]
    fn the_embedded_snapshot_matches_the_daemon_schema() {
        let snapshot = build(NOW).unwrap();
        let items: Vec<ItemSnapshot> = serde_json::from_value(snapshot.items).unwrap();
        let _: StatusSnapshot = serde_json::from_value(snapshot.status).unwrap();
        let history: Vec<HistoryPoint> = serde_json::from_value(snapshot.history).unwrap();
        assert!(!items.is_empty() && !history.is_empty());
    }

    /// A new timestamp field in the daemon output must be added to the lists above.
    #[test]
    fn no_timestamp_is_left_behind() {
        fn stale(key: &str, value: &Value, around: i64, found: &mut Vec<String>) {
            match value {
                Value::Array(entries) => entries.iter().for_each(|entry| stale(key, entry, around, found)),
                Value::Object(fields) => fields.iter().for_each(|(key, entry)| stale(key, entry, around, found)),
                Value::Number(number) if !key.ends_with("_bytes") => {
                    if number.as_i64().is_some_and(|n| (n - around).abs() < 5 * 365 * 86_400) {
                        found.push(format!("{key}={number}"));
                    }
                }
                _ => {}
            }
        }
        let ran: i64 = serde_json::from_str::<Value>(STATUS).unwrap()["ran_at_unix"].as_i64().unwrap();
        let snapshot = build(ran + 20 * 365 * 86_400).unwrap();
        let mut found = Vec::new();
        for value in [&snapshot.status, &snapshot.items, &snapshot.history] {
            stale("", value, ran, &mut found);
        }
        assert!(found.is_empty(), "unshifted timestamps: {found:?}");
    }

    #[test]
    fn a_real_status_is_never_overwritten() {
        let (real, empty) = (tmp("real"), tmp("empty"));
        std::fs::write(real.join("status.json"), r#"{"scanned":4,"ran_at_unix":1}"#).unwrap();
        assert!(refuse_real_state(&real).is_err());
        std::fs::write(real.join("status.json"), r#"{"scanned":4,"demo":true}"#).unwrap();
        assert!(refuse_real_state(&real).is_ok());
        assert!(refuse_real_state(&empty).is_ok());
        for dir in [real, empty] {
            std::fs::remove_dir_all(dir).ok();
        }
    }
}
