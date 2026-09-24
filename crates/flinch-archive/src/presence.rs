//! When each library item was on disk: presence spans from *arr history.
//!
//! The current file's `dateAdded` only says when *this* file arrived. A library
//! incident that re-downloads everything resets it, and an item the household
//! has had since March reads as four weeks old — to the live card and to every
//! past cut the panel asks about. The *arr history records each import and each
//! file deletion, so it can say when the household actually had the item.
//!
//! Rules, per card (a movie, or one season):
//! - an import opens a span; a removed file closes it;
//! - an upgrade (or Radarr's manual override) swaps a file at once, so presence
//!   goes on through it;
//! - a season holds many files: it closes only when every episode the history
//!   saw arrive has been removed, and never while it has files today if the
//!   history removed episodes it never saw arrive — then it cannot prove the
//!   season was ever empty;
//! - files on disk today with no open span (their import was never recorded:
//!   a disk scan writes no history) open a span at today's file date.
//!
//! An item with no history at all has no spans, and callers keep today's rule.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// A stretch of time the item had files on disk: `[from, to)`, unix seconds.
/// `to: None` means still on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub from: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<u64>,
}

impl Span {
    fn covers(&self, at: u64) -> bool {
        self.from <= at && self.to.map_or(true, |to| at < to)
    }
}

/// The span that had the item on disk at `at`, if any.
pub fn covering(spans: &[Span], at: u64) -> Option<&Span> {
    spans.iter().find(|span| span.covers(at))
}

/// What one history record did to an item's files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// A file arrived: a download, or a manual import from outside the library.
    Imported,
    /// A file was swapped for another at once: presence goes on.
    Replaced,
    /// A file left: missing from disk, or deleted.
    Removed,
}

/// One history record, as presence needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileEvent {
    /// Unix seconds.
    pub at: u64,
    /// The *arr's record id: orders records written in the same second.
    pub record: u64,
    /// The episode a season's record is about; `None` for a movie (one file).
    pub episode: Option<u32>,
    pub change: Change,
}

/// An item's presence as its history tells it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Presence {
    /// Sorted, non-overlapping; empty when the item has no history.
    pub spans: Vec<Span>,
    /// The open span starts at today's file date: no import was recorded for it.
    pub fallback: bool,
    /// Stretches on disk the history proves but cannot date: removals of files
    /// whose arrival was never recorded, and files on disk today with no date.
    pub undated: u32,
}

/// Presence of an item that has files on disk today, from its history events
/// (any order) and today's file date.
pub fn derive(mut events: Vec<FileEvent>, file_date: Option<u64>) -> Presence {
    if events.is_empty() {
        return Presence::default();
    }
    events.sort_unstable_by_key(|event| (event.at, event.record));
    let Replay { mut spans, mut undated, untracked } = replay(&events);
    // Its closures may have left untracked episodes behind: it has been on
    // disk since it first arrived.
    if untracked {
        if let Some(first) = spans.first().map(|span| span.from) {
            spans = vec![Span { from: first, to: None }];
        }
    }
    // Files today but no open span: today's file arrived after the history's
    // last removal. One dated before it proves the item never emptied, and
    // the joined spans say so.
    let mut fallback = false;
    if let Some(closed_at) = spans.last().map_or(Some(0), |span| span.to) {
        match file_date {
            Some(date) => {
                spans.push(Span { from: date.max(closed_at), to: None });
                fallback = true;
            }
            None => undated += 1,
        }
    }
    Presence { spans: merged(spans), fallback, undated }
}

#[derive(Default)]
struct Replay {
    spans: Vec<Span>,
    undated: u32,
    /// A season's history removed an episode it never saw arrive.
    untracked: bool,
}

fn replay(events: &[FileEvent]) -> Replay {
    let mut out = Replay::default();
    let mut present = BTreeSet::new();
    let mut open = None;
    for event in events {
        match event.change {
            Change::Imported => {
                open.get_or_insert(event.at);
                present.insert(event.episode);
            }
            Change::Replaced => {}
            Change::Removed => {
                if !present.remove(&event.episode) {
                    // A file the history never saw arrive.
                    out.untracked |= event.episode.is_some();
                    out.undated += u32::from(open.is_none());
                } else if present.is_empty() {
                    if let Some(from) = open.take() {
                        out.spans.push(Span { from, to: Some(event.at) });
                    }
                }
            }
        }
    }
    if let Some(from) = open {
        out.spans.push(Span { from, to: None });
    }
    out
}

/// Sorted, with touching and overlapping spans joined and empty ones dropped.
fn merged(mut spans: Vec<Span>) -> Vec<Span> {
    spans.sort_unstable_by_key(|span| span.from);
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for span in spans.into_iter().filter(|span| span.to != Some(span.from)) {
        match out.last_mut() {
            Some(last) if last.to.map_or(true, |to| span.from <= to) => {
                last.to = last.to.zip(span.to).map(|(a, b)| a.max(b));
            }
            _ => out.push(span),
        }
    }
    out
}

/// Unix seconds of an *arr timestamp: `2026-08-20T10:11:12Z`, with optional
/// fraction and `±hh:mm` offset; a bare date is midnight UTC. Before 1970 is
/// `None`: the *arrs write `0001-01-01T00:00:00Z` for "no date".
pub fn parse_utc(text: &str) -> Option<u64> {
    let text = text.trim();
    let mut date = text.get(..10)?.split('-');
    let (year, month, day) = (field(date.next()?, 4)?, field(date.next()?, 2)?, field(date.next()?, 2)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut seconds = days_from_civil(year, month, day) * 86_400;
    let rest = &text[10..];
    if let Some(time) = rest.strip_prefix(['T', ' ']) {
        let mut clock = time.get(..8)?.split(':');
        seconds += field(clock.next()?, 2)? * 3_600 + field(clock.next()?, 2)? * 60 + field(clock.next()?, 2)?;
        let zone = time[8..].trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
        seconds -= offset(zone)?;
    } else if !rest.is_empty() {
        return None;
    }
    u64::try_from(seconds).ok()
}

/// Seconds east of UTC for `Z`, `""`, or `±hh:mm`.
fn offset(zone: &str) -> Option<i64> {
    let (sign, hhmm) = match zone.as_bytes().first() {
        None | Some(b'Z') if zone.len() <= 1 => return Some(0),
        Some(b'+') => (1, &zone[1..]),
        Some(b'-') => (-1, &zone[1..]),
        _ => return None,
    };
    let (hours, minutes) = hhmm.split_once(':')?;
    Some(sign * (field(hours, 2)? * 3_600 + field(minutes, 2)? * 60))
}

/// A fixed-width run of ASCII digits.
fn field(digits: &str, width: usize) -> Option<i64> {
    if digits.len() != width || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests;
