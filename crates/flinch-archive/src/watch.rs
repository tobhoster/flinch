//! Watch state from the media server (Plex / Jellyfin export), merged into
//! cards.
//!
//! *arr knows files; only the media server knows "watched". This module is the
//! seam: an exported JSON file (or, later, a live Plex token) fills the fields
//! the archive reflex makes its sharpest calls on:
//! `season_state`, `is_watched`, `last_watched_days`, `rewatch_score`.

use crate::card::{ArchiveCard, SeasonState};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Where a watch verdict came from.
///
/// The distinction is load-bearing: a live media-server query is current truth,
/// while a previously exported file can be stale. Treating them as equal is how
/// a months-old export silently outvotes reality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WatchSource {
    /// Queried live from the media server, for this exact item.
    Plex,
    /// Queried live, but only at show level: the media server exposes no season
    /// rows for this show, so the numbers aggregate every season of it. Coarser
    /// than item-level truth, finer than a stale file.
    PlexShow,
    /// Read from a previously exported file: evidence, but it can be stale.
    #[default]
    Export,
    /// Tautulli's own stream database: per user, never trimmed, and it survives
    /// an item being removed from the media server.
    Tautulli,
    /// Tautulli saw this item's whole life and recorded no stream for it.
    ///
    /// Weaker than an item-level "zero playback" state: it says nobody streamed
    /// it through a tracked client since Tautulli started watching the server, not
    /// that no one ever watched it. Only claimed for items added after coverage
    /// began — before that, Tautulli was blind and absence means nothing.
    TautulliAbsence,
    /// Server-wide playback history: someone on this server played it.
    ///
    /// The media server's item fields (`viewCount`, `lastViewedAt`) are *per
    /// account* — the account whose token we borrow — while history is the whole
    /// server's. That is why watched items could read as "no evidence": the
    /// household watched them, the borrowed account did not. History proves a
    /// play but never proves *absence* of one, so it can protect an item and can
    /// never justify reclaiming one.
    PlexHistory,
}

impl WatchSource {
    /// How much the evidence is worth, as a multiplier on the `never_played`
    /// signal. Live truth counts fully; a file is discounted.
    pub fn evidence_factor(self) -> f32 {
        match self {
            WatchSource::Plex => 1.0,
            WatchSource::PlexShow => 0.8,
            WatchSource::TautulliAbsence => 0.7,
            WatchSource::Export => 0.6,
            // Never pays the never-played bonus at all: a stream database proves
            // plays, not their absence.
            WatchSource::PlexHistory | WatchSource::Tautulli => 0.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WatchSource::Plex => "plex",
            WatchSource::PlexShow => "plex_show",
            WatchSource::Export => "export",
            WatchSource::PlexHistory => "plex_history",
            WatchSource::Tautulli => "tautulli",
            WatchSource::TautulliAbsence => "tautulli_no_stream",
        }
    }

    /// Every source, for code that must cover them all.
    pub const ALL: [WatchSource; 6] = [
        WatchSource::Plex,
        WatchSource::PlexShow,
        WatchSource::Export,
        WatchSource::PlexHistory,
        WatchSource::Tautulli,
        WatchSource::TautulliAbsence,
    ];

    /// The inverse of [`WatchSource::label`], for reading persisted snapshots.
    ///
    /// Every label must map back: a source the fitter cannot read turns every
    /// row it touches into "no evidence", which is how a Tautulli-only household
    /// ended up with a panel that had no never-played signal at all.
    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|source| source.label() == label)
    }
}

/// One media item's watch truth.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchEntry {
    /// Matches `ArchiveCard.id` (`radarr-<id>` / `sonarr-<seriesId>-sN`).
    pub id: String,
    /// Epoch seconds of the most recent playback.
    pub last_watched_epoch: Option<u64>,
    /// Fraction of the item consumed (0.0 - 1.0). A season at 1.0 is Completed;
    /// a movie at 1.0 is watched.
    pub progress: f32,
    /// Rewatch propensity from history, 0.0 - 1.0. Absent when unknown.
    #[serde(default)]
    pub rewatch_score: Option<f32>,
    /// Provenance of this entry. Files predating this field are exports.
    #[serde(default)]
    pub source: WatchSource,
}

/// How much of the household's watch record one cycle actually read.
///
/// "Never played" is a claim about the whole record, so it may only be acted
/// on when the whole record was read: a failed or truncated source turns
/// silence into a guess. The daemon forces the never-played rule off whenever
/// [`EvidenceHealth::never_played_reclaim_safe`] says no.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EvidenceHealth {
    pub plex_configured: bool,
    /// Every library section was listed to its end, with GUIDs.
    pub plex_items_ok: bool,
    /// Server-wide play history was paged to its end or past the horizon.
    pub plex_history_complete: bool,
    pub tautulli_configured: bool,
    /// Every Tautulli history page was read, and Tautulli is recording now.
    pub tautulli_complete: bool,
    /// The server has more than one account, or its accounts could not be read.
    pub multi_account: bool,
    /// Settings name a Plex URL without its token, so that URL was not used:
    /// the evidence (if any) came from another server than the operator set.
    #[serde(default)]
    pub plex_settings_unpaired: bool,
}

impl EvidenceHealth {
    /// Whether Plex item state saying "nothing played" is evidence. Item state
    /// belongs to the borrowed admin account alone; on a shared server it is
    /// only trusted when server-wide history and Tautulli were both complete,
    /// so a play by anyone else would have shown up there.
    pub fn admin_zero_is_evidence(&self) -> bool {
        !self.multi_account || (self.plex_history_complete && self.tautulli_configured && self.tautulli_complete)
    }

    /// Whether Tautulli's silence may be claimed at all this cycle: targets must
    /// be resolvable in Plex (so the check is by ratingKey) and Tautulli's
    /// record must have been read in full.
    pub fn absence_is_claimable(&self) -> bool {
        self.plex_configured && self.plex_items_ok && self.tautulli_configured && self.tautulli_complete
    }

    /// Whether the never-played reclaim rule may be armed this cycle: Plex was
    /// read completely, and so was Tautulli if it is configured.
    pub fn never_played_reclaim_safe(&self) -> bool {
        self.plex_configured
            && self.plex_items_ok
            && self.plex_history_complete
            && (!self.tautulli_configured || self.tautulli_complete)
            && !self.plex_settings_unpaired
    }

    /// What is missing, in words, for the log and the status page.
    pub fn problems(&self) -> Vec<&'static str> {
        let mut problems = Vec::new();
        if self.plex_settings_unpaired {
            problems.push("the Plex URL in Settings has no token, so it is not used: enter the Plex token again");
        }
        if !self.plex_configured {
            problems.push("plex not configured");
        } else {
            if !self.plex_items_ok {
                problems.push("plex library listing failed or was incomplete");
            }
            if !self.plex_history_complete {
                problems.push("plex play history incomplete");
            }
        }
        if self.tautulli_configured && !self.tautulli_complete {
            problems.push("tautulli history incomplete, not recording, or not kept for every user and library");
        }
        // Only Plex item state has an admin whose "no plays" can hide someone
        // else's play; the flag is fail-closed `true` when Plex is absent.
        if self.plex_configured && self.multi_account && !self.admin_zero_is_evidence() {
            problems.push("several plex accounts, or unreadable: admin-only 'no plays' is not evidence");
        }
        problems
    }
}

/// Apply watch state to cards in place.
///
/// Fail-closed: when media server data is missing the item stays "not watched &
/// not completed", and the policy then refuses to delete it. A daemon with a
/// dead media-server export protects everything until it is healthy again,
/// which is the safe direction.
pub fn apply(cards: &mut [ArchiveCard], watch: &HashMap<String, WatchEntry>) {
    for card in cards.iter_mut() {
        let Some(entry) = watch.get(&card.id) else {
            card.last_watched_days = None;
            card.is_watched = Some(false);
            card.season_state = Some(SeasonState::Empty);
            continue;
        };
        card.last_watched_days = entry.last_watched_epoch.map(|epoch| (now_epoch().saturating_sub(epoch)) as f32 / 86_400.0);
        match card.kind {
            crate::card::LibraryKind::Season => {
                card.season_state = Some(if entry.progress >= 0.999 {
                    SeasonState::Completed
                } else if entry.progress > 0.0 {
                    SeasonState::Partial
                } else {
                    SeasonState::Empty
                });
                card.episodes_watched = card.episodes_total.map(|total| (total as f32 * entry.progress.clamp(0.0, 1.0)).round() as u32);
            }
            crate::card::LibraryKind::Movie => {
                card.is_watched = Some(entry.progress >= 0.999);
                card.rewatch_score = entry.rewatch_score;
            }
        }
    }
}

#[cfg(not(test))]
fn now_epoch() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
fn now_epoch() -> u64 {
    1_800_000_000
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::golden::golden_movie;

    fn golden_season() -> ArchiveCard {
        crate::golden::golden_season()
    }

    fn health(plex: (bool, bool, bool), tautulli: (bool, bool), multi_account: bool) -> EvidenceHealth {
        EvidenceHealth {
            plex_configured: plex.0,
            plex_items_ok: plex.1,
            plex_history_complete: plex.2,
            tautulli_configured: tautulli.0,
            tautulli_complete: tautulli.1,
            multi_account,
            plex_settings_unpaired: false,
        }
    }

    #[rstest::rstest]
    #[case::everything_read(health((true, true, true), (true, true), true), true)]
    #[case::plex_alone_complete(health((true, true, true), (false, false), false), true)]
    #[case::plex_down(health((true, false, true), (true, true), false), false)]
    #[case::history_truncated(health((true, true, false), (true, true), false), false)]
    #[case::tautulli_truncated(health((true, true, true), (true, false), false), false)]
    // Without Plex nothing can be resolved by GUID, so no silence is evidence.
    #[case::tautulli_alone(health((false, false, false), (true, true), false), false)]
    // The operator's Plex was not read, whatever the environment's Plex said.
    #[case::settings_url_without_its_token(EvidenceHealth { plex_settings_unpaired: true, ..health((true, true, true), (true, true), false) }, false)]
    fn never_played_reclaim_is_armed_only_on_a_fully_read_record(#[case] health: EvidenceHealth, #[case] safe: bool) {
        assert_eq!(health.never_played_reclaim_safe(), safe, "{:?}", health.problems());
        if safe {
            assert!(health.problems().is_empty(), "an armed cycle reports no problem: {:?}", health.problems());
        } else {
            assert!(!health.problems().is_empty(), "a disarmed cycle says why");
        }
    }

    #[rstest::rstest]
    #[case::one_account(health((true, true, false), (false, false), false), true)]
    #[case::shared_but_fully_covered(health((true, true, true), (true, true), true), true)]
    #[case::shared_without_tautulli(health((true, true, true), (false, false), true), false)]
    #[case::shared_history_truncated(health((true, true, false), (true, true), true), false)]
    fn admin_only_zeros_count_only_where_nobody_else_could_have_played_unseen(#[case] health: EvidenceHealth, #[case] evidence: bool) {
        assert_eq!(health.admin_zero_is_evidence(), evidence);
    }

    #[test]
    fn a_cycle_without_plex_makes_no_claim_about_plex_accounts() {
        let tautulli_only = |multi_account| health((false, false, false), (true, true), multi_account);
        assert_eq!(tautulli_only(true).problems(), tautulli_only(false).problems());
    }

    #[test]
    fn completed_recent_season_and_watched_movie_are_reflected() {
        let mut cards = vec![golden_season(), golden_movie()];
        let mut watch = HashMap::new();
        // 10 days ago, fully consumed
        watch.insert(
            "season-mvp".to_string(),
            WatchEntry {
                id: "season-mvp".to_string(),
                last_watched_epoch: Some(now_epoch() - 864_000),
                progress: 1.0,
                rewatch_score: None,
                source: WatchSource::Plex,
            },
        );
        watch.insert(
            "movie-rewatch".to_string(),
            WatchEntry {
                id: "movie-rewatch".to_string(),
                last_watched_epoch: Some(now_epoch() - 864_000),
                progress: 1.0,
                rewatch_score: Some(0.9),
                source: WatchSource::Plex,
            },
        );
        apply(&mut cards, &watch);

        assert_eq!(cards[0].season_state, Some(SeasonState::Completed));
        assert!(cards[0].last_watched_days.unwrap() < 11.0);
        assert_eq!(cards[1].is_watched, Some(true));
        assert_eq!(cards[1].rewatch_score, Some(0.9));
    }

    #[test]
    fn missing_media_server_entry_fails_closed() {
        let mut cards = vec![golden_season(), golden_movie()];
        apply(&mut cards, &HashMap::new());
        assert_eq!(cards[0].season_state, Some(SeasonState::Empty));
        assert_eq!(cards[1].is_watched, Some(false));
        assert!(cards[1].last_watched_days.is_none());
    }

    #[test]
    fn every_persisted_source_label_reads_back_as_its_source() {
        for source in WatchSource::ALL {
            // Exhaustive on purpose: a new variant stops compiling here until it
            // is also listed in `ALL`, so no label can go unreadable.
            match source {
                WatchSource::Plex
                | WatchSource::PlexShow
                | WatchSource::Export
                | WatchSource::PlexHistory
                | WatchSource::Tautulli
                | WatchSource::TautulliAbsence => {}
            }
            assert_eq!(WatchSource::from_label(source.label()), Some(source));
        }
        assert_eq!(WatchSource::from_label("jellyfin"), None, "an unknown label is no evidence, not a guess");
    }
}
