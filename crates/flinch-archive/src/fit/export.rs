//! The panel as a benchmark: every question FLINCH is fitted and judged on,
//! written so an external decision model can answer the same questions.
//!
//! One JSON object per line. `state` is self-describing and human-readable —
//! what a general-purpose model (a prompt, a classifier over text) can consume —
//! and is derived only from the card and context inference would have seen at
//! the cut, so it cannot know the answer. `features` are FLINCH's own unweighted
//! signals for the same row. Answers come back in the predictions format below
//! and are scored by [`super::bench`].

use super::panel::Example;
use crate::card::{ArchiveCard, LibraryKind};
use crate::score::{self, HouseholdContext};
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::Write;

/// What was true about an item at the cut, in plain terms.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AsOfState {
    /// "movie" or "season".
    pub kind: &'static str,
    pub title: String,
    pub show_title: Option<String>,
    pub season_index: Option<u32>,
    /// Size in GiB (2^30 bytes), the unit every size FLINCH shows is labelled in.
    pub size_gib: f64,
    pub days_on_disk: f32,
    pub episodes_total: Option<u32>,
    /// Distinct episodes played before the cut (seasons).
    pub episodes_played: Option<u32>,
    /// Share of the item played before the cut; `null` when unknowable (a season
    /// with no episode count).
    pub played_fraction: Option<f32>,
    /// `null` if never played before the cut.
    pub days_since_last_play: Option<f32>,
    /// Played again on a separate occasion before the cut.
    pub rewatched: bool,
    /// Distinct household viewers of the movie, or of any season of the show.
    pub viewers: u32,
    pub sibling_played: bool,
    pub sibling_completed: bool,
    /// Seasons of the same show in the library.
    pub siblings: u32,
    pub is_newest_season: bool,
    /// The series had ended and its final episode had aired by the cut.
    pub series_ended: bool,
    /// Where the watch evidence came from (`null` = none: undecidable).
    pub watch_evidence: Option<&'static str>,
    /// A structural guard FLINCH never overrides ("newest-season", …).
    pub hard_guard: Option<&'static str>,
}

impl AsOfState {
    pub fn describe(card: &ArchiveCard, ctx: &HouseholdContext) -> Self {
        let (kind, played_fraction) = match card.kind {
            LibraryKind::Movie => ("movie", Some(if card.is_watched == Some(true) { 1.0 } else { 0.0 })),
            LibraryKind::Season => (
                "season",
                match (card.episodes_watched, card.episodes_total) {
                    (Some(watched), Some(total)) if total > 0 => Some((watched as f32 / total as f32).min(1.0)),
                    (Some(0), _) => Some(0.0),
                    _ => None,
                },
            ),
        };
        Self {
            kind,
            title: card.title.clone(),
            show_title: card.show_title.clone(),
            season_index: card.season_index,
            size_gib: card.size_bytes as f64 / f64::from(1u32 << 30),
            days_on_disk: card.added_days_ago,
            episodes_total: card.episodes_total,
            episodes_played: match card.kind {
                LibraryKind::Season => card.episodes_watched,
                LibraryKind::Movie => None,
            },
            played_fraction,
            days_since_last_play: card.last_watched_days,
            rewatched: ctx.rewatched,
            viewers: ctx.viewers,
            sibling_played: ctx.sibling_season_played,
            sibling_completed: ctx.sibling_season_completed,
            siblings: ctx.siblings,
            is_newest_season: card.is_newest_season == Some(true),
            series_ended: ctx.series_ended,
            watch_evidence: ctx.watch_source.map(|source| source.label()),
            hard_guard: score::hard_guard(card),
        }
    }
}

/// One exported panel line.
#[derive(Debug, Serialize)]
pub struct PanelRow<'a> {
    pub id: &'a str,
    pub cut_days: f32,
    pub cut_unix: u64,
    pub horizon_days: f32,
    /// 1 = nothing played it within the horizon after the cut: safe to reclaim.
    pub label: u8,
    pub state: AsOfState,
    pub features: BTreeMap<&'static str, f32>,
}

impl<'a> PanelRow<'a> {
    pub fn of(example: &'a Example, horizon_days: f32) -> Self {
        Self {
            id: &example.item_id,
            cut_days: example.cut_days,
            cut_unix: example.cut_unix,
            horizon_days,
            label: u8::from(example.label >= 0.5),
            state: AsOfState::describe(&example.card, &example.ctx),
            features: example.values.iter().map(|(name, value)| (*name, *value)).collect(),
        }
    }
}

/// One line of the predictions format `--score` reads.
#[derive(Debug, Serialize)]
pub struct PredictionRow<'a> {
    pub id: &'a str,
    pub cut_days: f32,
    /// Optional on input; when echoed back it proves the row answers this panel.
    pub cut_unix: u64,
    /// P(safe to reclaim), in [0, 1].
    pub p: f32,
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("write failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialisation failed: {0}")]
    Json(#[from] serde_json::Error),
}

/// Write every panel row as JSONL; returns the number of rows written.
pub fn write_panel(mut out: impl Write, dataset: &[Example], horizon_days: f32) -> Result<usize, ExportError> {
    for example in dataset {
        serde_json::to_writer(&mut out, &PanelRow::of(example, horizon_days))?;
        out.write_all(b"\n")?;
    }
    out.flush()?;
    Ok(dataset.len())
}

/// Write one probability per panel row in the predictions format — FLINCH's own
/// answers, as a worked example of the format and a self-check of the harness.
pub fn write_predictions(mut out: impl Write, dataset: &[Example], probabilities: &[f32]) -> Result<usize, ExportError> {
    for (example, p) in dataset.iter().zip(probabilities) {
        let row = PredictionRow { id: &example.item_id, cut_days: example.cut_days, cut_unix: example.cut_unix, p: *p };
        serde_json::to_writer(&mut out, &row)?;
        out.write_all(b"\n")?;
    }
    out.flush()?;
    Ok(dataset.len().min(probabilities.len()))
}
