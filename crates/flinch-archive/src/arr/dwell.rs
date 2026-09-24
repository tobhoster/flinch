//! How long a card has been on disk, as of now.

use crate::presence::{self, Span};

/// Days on disk: since the presence span covering now began — the number the
/// fitter computes at a past cut — else since the current file `arrived`. An
/// unknown or unparseable arrival is *fresh*: dwell can only be proven by a
/// real date, and a guessed "old" would make a just-downloaded item eligible
/// for never-played reclaim on day one.
pub(super) fn days_on_disk(on_disk: &[Span], arrived: Option<&str>) -> f32 {
    let now = now_epoch();
    match presence::covering(on_disk, now).map(|span| span.from).or_else(|| arrived.and_then(chrono_lite)) {
        Some(epoch) => now.saturating_sub(epoch) as f32 / 86_400.0,
        None => 0.0,
    }
}

#[cfg(not(test))]
fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
pub(super) fn now_epoch() -> u64 {
    1_800_000_000
}

/// Minimal ISO-8601 date (yyyy-mm-dd) parser. Only the parts the API sends.
///
/// Pre-1970 dates are `None`, not an underflow: Sonarr and Radarr use
/// `0001-01-01T00:00:00Z` as a null date, and `year - 1970` on a u64 would
/// panic in the daemon loop.
pub(super) fn chrono_lite(input: &str) -> Option<u64> {
    let bytes = input.as_bytes();
    if bytes.len() < 10 {
        return None;
    }
    let year: u64 = std::str::from_utf8(&bytes[0..4]).ok()?.parse().ok()?;
    let month: u64 = std::str::from_utf8(&bytes[5..7]).ok()?.parse().ok()?;
    let day: u64 = std::str::from_utf8(&bytes[8..10]).ok()?.parse().ok()?;
    let days_prior = year.checked_sub(1970)? * 365 + year.checked_sub(1968)? / 4; // ~leap days; close enough for a recency feature
    Some(days_prior * 86_400 + month.saturating_sub(1) * 2_592_000 + day * 86_400)
}
