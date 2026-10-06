//! How long a card has been on disk, as of now.

use crate::presence::{self, Span};

/// Days on disk: since the presence span covering now began — the number the
/// fitter computes at a past cut — else since the current file `arrived`. An
/// unknown or unparseable arrival is *fresh*: dwell can only be proven by a
/// real date, and a guessed "old" would make a just-downloaded item eligible
/// for never-played reclaim on day one.
pub(super) fn days_on_disk(on_disk: &[Span], arrived: Option<&str>) -> f32 {
    let now = now_epoch();
    match presence::covering(on_disk, now).map(|span| span.from).or_else(|| arrived.and_then(presence::parse_utc)) {
        Some(epoch) => now.saturating_sub(epoch) as f32 / 86_400.0,
        None => 0.0,
    }
}

#[cfg(not(test))]
fn now_epoch() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
pub(super) fn now_epoch() -> u64 {
    1_800_000_000
}
