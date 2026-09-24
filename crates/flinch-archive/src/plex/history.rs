//! Correlating the media server's playback history with library items.
//!
//! Server-wide history proves plays that per-account item state cannot see. It
//! proves plays only — never their absence — so it can refresh recency but
//! never grant "never played". Rows join by ratingKey through the target's
//! [`Resolution`]; a title is never enough.

use super::join::RowKey;
use super::resolve::Resolution;
use super::{PlexMetadata, WatchTarget};
use crate::card::LibraryKind;
use crate::watch::{WatchEntry, WatchSource};
use std::collections::{HashMap, HashSet};

/// Watch entries from server-wide playback history.
///
/// A play proves watching; silence proves nothing (history can be trimmed, and
/// a manual "mark as played" never writes a row), so an entry exists only where
/// a play was found and it never claims the absence of one.
pub fn history_entries(targets: &[WatchTarget], resolution: &Resolution, rows: &[PlexMetadata]) -> HashMap<String, WatchEntry> {
    let keyed: Vec<(RowKey, &PlexMetadata)> =
        rows.iter().filter(|row| row.viewed_at.is_some()).filter_map(|row| Some((RowKey::plex(row)?, row))).collect();
    let mut out = HashMap::new();
    for target in targets {
        let join = resolution.join(target);
        let plays: Vec<&PlexMetadata> = keyed.iter().filter(|(key, _)| join.matches(key)).map(|(_, row)| *row).collect();
        if plays.is_empty() {
            continue;
        }
        let last_watched_epoch = plays.iter().filter_map(|row| row.viewed_at).max();
        let progress = match target.kind {
            LibraryKind::Movie => 1.0,
            LibraryKind::Season => {
                // Distinct episodes, not rows: a rewatch is not a second episode.
                let distinct = plays.iter().map(|row| (row.parent_index, row.index)).collect::<HashSet<_>>().len() as f32;
                match target.episode_files.or(target.episodes_total).filter(|total| *total > 0) {
                    Some(total) => (distinct / total as f32).clamp(0.0, 1.0),
                    // No episode count to divide by: claim "started", never "complete".
                    None => 0.5,
                }
            }
        };
        out.insert(
            target.id.clone(),
            WatchEntry { id: target.id.clone(), last_watched_epoch, progress, rewatch_score: None, source: WatchSource::PlexHistory },
        );
    }
    out
}

/// Let playback history fill in or refresh what item state knew.
///
/// Newer evidence wins. A stale item-level "no playback" (epoch absent) loses to
/// a history play, which is the whole point: the household's watch must not be
/// erased by the borrowed account's silence. Equal or newer item state is kept,
/// because it is the stronger, per-item source.
pub fn merge_history(entries: &mut HashMap<String, WatchEntry>, history: HashMap<String, WatchEntry>) {
    for (id, played) in history {
        match entries.get(&id) {
            Some(existing) if existing.last_watched_epoch.unwrap_or(0) >= played.last_watched_epoch.unwrap_or(0) => {}
            _ => {
                entries.insert(id, played);
            }
        }
    }
}
