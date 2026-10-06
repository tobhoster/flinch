//! The panel: every cut date × every item that was on disk at that cut, asked
//! exactly as the daemon would have asked it then.
//!
//! Each row carries the hazard's features at the cut, read only from plays
//! *before* it, and its outcome: whether anything played the item in the
//! horizon *after* it. Only fully observed windows are rows: a window still
//! open at `now` would read as "not played" and teach the model that recent
//! items are cold.

use super::FitItem;
use crate::presence;
use crate::regret::{PlayHistory, WatchFeatures};

const DAY_SECS: u64 = 86_400;

/// Which questions the panel asks.
#[derive(Debug, Clone, Copy)]
pub struct PanelSpec<'a> {
    /// The panel's "today", unix seconds. Pin it to reproduce a panel later.
    pub now: u64,
    /// Cut dates, in days before `now`.
    pub cuts_days: &'a [f32],
    pub horizon_days: f32,
}

/// One panel row: an "as of" question and its observed answer.
#[derive(Debug, Clone)]
pub struct Example {
    pub item_id: String,
    /// When the question was asked, in days before the panel's `now`.
    pub cut_days: f32,
    pub cut_unix: u64,
    /// What the hazard would have read at the cut.
    pub features: WatchFeatures,
    /// 1.0 = played during the horizon after the cut.
    pub label: f32,
}

/// Build the panel: every cut date × every item that was on disk at that cut.
pub fn build_dataset(items: &[FitItem], spec: &PanelSpec<'_>) -> Vec<Example> {
    let horizon = (spec.horizon_days * 86_400.0) as u64;
    let mut examples = Vec::new();
    for days in spec.cuts_days {
        let cut = spec.now.saturating_sub((*days * 86_400.0) as u64);
        if cut + horizon > spec.now {
            continue;
        }
        for item in items {
            let Some(arrival) = arrival_by(item, cut, spec.now) else { continue };
            let item_plays: Vec<_> = item.plays.iter().filter(|play| play.epoch < cut).collect();
            let audience: Vec<_> = item.audience_plays.iter().filter(|play| play.epoch < cut).collect();
            let history = PlayHistory {
                item: &item_plays,
                audience: &audience,
                episodes_total: item.episodes_total,
                last_watched_days: None,
                added_days_ago: (cut - arrival) as f32 / DAY_SECS as f32,
            };
            examples.push(Example {
                item_id: item.id.clone(),
                cut_days: *days,
                cut_unix: cut,
                features: WatchFeatures::read(&history, cut),
                label: if item.played_between(cut, cut + horizon) { 1.0 } else { 0.0 },
            });
        }
    }
    examples
}

/// When the item that was on disk at `cut` arrived there, or `None` if it was
/// not on disk then.
///
/// With presence history, it was on disk iff a span covers the cut, and it
/// arrived when that span began: the start the daemon dates its live card
/// from, so both compute the same dwell for the same date.
///
/// Without it: its recorded arrival, or an earlier play. A library migration
/// re-imports files and resets arrival dates (seen live: 12 of 41 items played
/// before their recorded arrival), which silently dropped their whole history
/// from the panel. A play before the cut proves the item was there, and uses
/// nothing from after it.
fn arrival_by(item: &FitItem, cut: u64, now: u64) -> Option<u64> {
    if !item.on_disk.is_empty() {
        return presence::covering(&item.on_disk, cut).map(|span| span.from);
    }
    let recorded = item.added_epoch(now);
    let first_play = item.plays.iter().map(|play| play.epoch).filter(|epoch| *epoch < cut).min();
    let arrival = first_play.map_or(recorded, |played| played.min(recorded));
    (arrival <= cut).then_some(arrival)
}
