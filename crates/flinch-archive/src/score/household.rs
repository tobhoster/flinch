//! What the rest of a show says about one of its seasons.
//!
//! Sibling signals are about the *other* seasons: counting a season's own
//! completion as "the household still cares about this show" made every
//! finished season argue for keeping itself. The fitter already excluded the
//! item (`fit::panel`); this is the same definition for the daemon, so training
//! and inference agree.

use crate::card::ArchiveCard;
use crate::watch::WatchEntry;
use std::collections::HashMap;

/// Watched fraction at which a season counts as completed.
const COMPLETE: f32 = 0.999;

/// Sibling evidence for one item, never counting the item itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Siblings {
    /// Another season of the show was played at least partly.
    pub played: bool,
    /// Another season of the show was played to completion.
    pub completed: bool,
}

/// Per show: how many of its seasons were played, and completed.
#[derive(Debug, Default)]
pub struct ShowActivity {
    played: HashMap<String, u32>,
    completed: HashMap<String, u32>,
}

fn progress(watch: &HashMap<String, WatchEntry>, id: &str) -> f32 {
    watch.get(id).map_or(0.0, |entry| entry.progress)
}

impl ShowActivity {
    pub fn new(cards: &[ArchiveCard], watch: &HashMap<String, WatchEntry>) -> Self {
        let mut activity = Self::default();
        for card in cards {
            let Some(show) = card.show_title.as_deref() else { continue };
            let progress = progress(watch, &card.id);
            if progress > 0.0 {
                *activity.played.entry(show.to_string()).or_default() += 1;
            }
            if progress >= COMPLETE {
                *activity.completed.entry(show.to_string()).or_default() += 1;
            }
        }
        activity
    }

    /// The other seasons' evidence for `card`; a movie has no siblings.
    pub fn siblings(&self, card: &ArchiveCard, watch: &HashMap<String, WatchEntry>) -> Siblings {
        let Some(show) = card.show_title.as_deref() else {
            return Siblings::default();
        };
        let own = progress(watch, &card.id);
        let others = |counts: &HashMap<String, u32>, counts_itself: bool| {
            counts.get(show).copied().unwrap_or(0).saturating_sub(u32::from(counts_itself)) > 0
        };
        Siblings { played: others(&self.played, own > 0.0), completed: others(&self.completed, own >= COMPLETE) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::golden::{golden_movie, golden_season};
    use crate::watch::WatchSource;
    use rstest::rstest;

    fn season(id: &str, show: &str) -> ArchiveCard {
        let mut card = golden_season();
        card.id = id.to_string();
        card.show_title = Some(show.to_string());
        card
    }

    fn watched(entries: &[(&str, f32)]) -> HashMap<String, WatchEntry> {
        entries
            .iter()
            .map(|(id, progress)| {
                let entry = WatchEntry {
                    id: id.to_string(),
                    last_watched_epoch: Some(1),
                    progress: *progress,
                    rewatch_score: None,
                    source: WatchSource::Plex,
                };
                (id.to_string(), entry)
            })
            .collect()
    }

    #[rstest]
    #[case::a_finished_season_alone_has_no_completed_sibling(&[("s1", 1.0)], "s1", Siblings { played: false, completed: false })]
    #[case::two_finished_seasons_each_see_the_other(&[("s1", 1.0), ("s2", 1.0)], "s1", Siblings { played: true, completed: true })]
    #[case::an_unplayed_season_sees_a_finished_sibling(&[("s1", 1.0)], "s2", Siblings { played: true, completed: true })]
    #[case::a_partial_sibling_is_played_not_completed(&[("s1", 0.4)], "s2", Siblings { played: true, completed: false })]
    #[case::its_own_partial_play_is_not_a_sibling(&[("s2", 0.4)], "s2", Siblings { played: false, completed: false })]
    fn sibling_evidence_never_counts_the_item_itself(#[case] progress: &[(&str, f32)], #[case] item: &str, #[case] expected: Siblings) {
        let cards = vec![season("s1", "Show"), season("s2", "Show"), season("x1", "Other")];
        let watch = watched(progress);
        let activity = ShowActivity::new(&cards, &watch);
        let card = cards.iter().find(|card| card.id == item).expect("card in fixture");
        assert_eq!(activity.siblings(card, &watch), expected);
    }

    #[test]
    fn a_movie_has_no_siblings() {
        let movie = golden_movie();
        let activity = ShowActivity::new(&[movie.clone()], &HashMap::new());
        assert_eq!(activity.siblings(&movie, &HashMap::new()), Siblings::default());
    }
}
