//! How much of a season the play logs show finished, counting only the
//! episodes still on disk.
//!
//! Plex's history and Tautulli remember every episode ever finished, including
//! ones deleted since — by hand, or by Plex's "Delete episodes after playing".
//! Counted against the files that are left, those plays made a season whose
//! unwatched episodes stay on disk read as complete, and a complete season
//! leaves through its delete collection with no Leaving Soon warning. So a
//! season reads complete only when every episode number on disk was finished;
//! where those numbers are unknown, it reads just short of complete, which
//! announces it instead of deleting it straight away.

use super::WatchTarget;
use std::collections::BTreeSet;

/// What a season whose plays alone would read complete reads while its episodes
/// on disk are unknown: below [`crate::watch::COMPLETE`], and the same ceiling
/// a season that is merely nearly finished gets.
pub const UNVERIFIED: f32 = 0.99;

/// The distinct episodes of one season its plays touched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EpisodePlays {
    finished: BTreeSet<u32>,
    /// Played but never finished.
    started: BTreeSet<u32>,
    /// Plays with no episode number: each kind counts as one episode against
    /// a file count, but cannot be matched to a file on disk.
    unnumbered_finished: bool,
    unnumbered_started: bool,
}

impl EpisodePlays {
    /// Record one play of `episode` (its number within the season).
    pub fn record(&mut self, episode: Option<u32>, finished: bool) {
        match (episode, finished) {
            (Some(number), true) => {
                self.finished.insert(number);
            }
            (Some(number), false) => {
                self.started.insert(number);
            }
            (None, true) => self.unnumbered_finished = true,
            (None, false) => self.unnumbered_started = true,
        }
    }

    /// The season's watched share, 0.01-1.0: finished episodes count fully,
    /// ones only started as half, and the season is complete (1.0) only when
    /// every episode it is counted against was finished.
    ///
    /// For a season on disk it is counted against the episode numbers on disk
    /// ([`WatchTarget::episodes_on_disk`]) and never reads higher than counted
    /// against the file count; with those numbers unknown it stops at
    /// [`UNVERIFIED`]. A season with nothing on disk has nothing to delete and
    /// keeps the file-count share. Without any episode count the plays say
    /// "started", never "complete": 0.5.
    pub fn progress(&self, target: &WatchTarget) -> f32 {
        let Some(total) = target.episode_files.or(target.episodes_total).filter(|total| *total > 0) else { return 0.5 };
        let finished = self.finished.len() + usize::from(self.unnumbered_finished);
        let started = self.started.difference(&self.finished).count() + usize::from(self.unnumbered_started && !self.unnumbered_finished);
        let by_files = share(finished, started, total as usize);
        if !target.on_disk {
            return by_files;
        }
        match target.episodes_on_disk.as_deref().filter(|on_disk| !on_disk.is_empty()) {
            Some(on_disk) => {
                let held = |number: &&u32| on_disk.binary_search(number).is_ok();
                let finished_held = self.finished.iter().filter(held).count();
                let started_held = self.started.difference(&self.finished).filter(held).count();
                share(finished_held, started_held, on_disk.len()).min(by_files)
            }
            None => by_files.min(UNVERIFIED),
        }
    }
}

/// `finished` whole episodes and `started` halves out of `total`: 1.0 only when
/// all were finished, otherwise held between "touched" and "nearly finished".
fn share(finished: usize, started: usize, total: usize) -> f32 {
    if finished >= total {
        1.0
    } else {
        ((finished as f32 + 0.5 * started as f32) / total as f32).clamp(0.01, UNVERIFIED)
    }
}

#[cfg(test)]
mod tests;
