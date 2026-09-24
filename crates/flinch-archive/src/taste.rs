//! The taste signal: how readily this household plays titles of an item's
//! genres, learned from its own outcomes.
//!
//! Every other signal in [`crate::score`] reads the item's own plays, so a fresh
//! download can only score "never played" and "on disk N days". What the
//! household does with the *rest* of the library says more: a home that plays
//! every new comedy within a month and leaves horror untouched will do the same
//! with the next download. The panel already records that as outcomes — "was
//! this played within the horizon after the cut?" — so the rate is counted, not
//! guessed, and needs no model server and no network.
//!
//! The result is `HouseholdContext::taste`, a P(played within the horizon),
//! and it becomes the zero-prior `taste` feature: it moves no decision until
//! the daily held-out fit gives it a weight and that fit beats the priors.
//!
//! Leak-freedom: the rates for an as-of date count only panel outcomes whose
//! window had closed by then (`cut + horizon <= as_of`). A panel row is asked
//! with the rates as of its own cut; the daemon with the rates the daily refit
//! learned from every closed outcome, stored in `fit.json`.

use crate::card::{ArchiveCard, SeasonState};
use crate::fit::panel::Example;
use crate::fit::FitItem;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Pseudo-outcomes pulling each genre's rate toward the household's overall
/// played rate: a genre seen twice says little, one seen fifty times speaks
/// for itself.
pub const PRIOR_OUTCOMES: f32 = 10.0;
/// Rates are kept off 0 and 1 before they are combined in logit space.
const RATE_BOUNDS: (f32, f32) = (0.01, 0.99);

/// Closed outcomes of one kind: how many there were, and how many were played.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcomes {
    pub played: u32,
    pub total: u32,
}

impl Outcomes {
    fn add(&mut self, played: bool) {
        self.played += u32::from(played);
        self.total += 1;
    }
}

/// The household's played rate overall and per genre, as of one date.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GenreRates {
    /// The latest date whose outcomes these counts may include, unix seconds.
    pub as_of: u64,
    pub overall: Outcomes,
    /// Normalised genre name → its outcomes.
    pub genres: BTreeMap<String, Outcomes>,
}

impl GenreRates {
    /// Count every panel row whose outcome window had closed by `as_of`.
    pub fn as_of(dataset: &[Example], genres: &ItemGenres<'_>, horizon_secs: u64, as_of: u64) -> Self {
        let mut rates = Self { as_of, ..Self::default() };
        for row in dataset.iter().filter(|row| row.cut_unix.saturating_add(horizon_secs) <= as_of) {
            let played = row.label < 0.5;
            rates.overall.add(played);
            for genre in genres.of_id(&row.item_id) {
                rates.genres.entry(genre.clone()).or_default().add(played);
            }
        }
        rates
    }

    /// P(played within the horizon) for a title of these genres; `None` without
    /// genres or without a single closed outcome.
    ///
    /// Each genre's rate is Beta-smoothed toward the overall rate, and the
    /// genres are averaged in logit space. A title's genres overlap heavily
    /// (Action and Thriller travel together), so treating them as independent
    /// evidence would count one preference two or three times; the mean keeps a
    /// three-genre title on the same scale as a one-genre title and between its
    /// genres' own rates.
    pub fn played(&self, genres: &[String]) -> Option<f32> {
        if self.overall.total == 0 {
            return None;
        }
        let overall = self.overall.played as f32 / self.overall.total as f32;
        let logits: Vec<f32> = normalise(genres)
            .iter()
            .map(|genre| {
                let seen = self.genres.get(genre).copied().unwrap_or_default();
                let rate = (seen.played as f32 + PRIOR_OUTCOMES * overall) / (seen.total as f32 + PRIOR_OUTCOMES);
                let rate = rate.clamp(RATE_BOUNDS.0, RATE_BOUNDS.1);
                (rate / (1.0 - rate)).ln()
            })
            .collect();
        if logits.is_empty() {
            return None;
        }
        let mean = logits.iter().sum::<f32>() / logits.len() as f32;
        Some(1.0 / (1.0 + (-mean).exp()))
    }
}

/// The taste of one item as of the rates' date: only for an item nobody had
/// played, because once plays exist they speak for the household better than
/// its genres do. The panel and the daemon both ask through here.
pub fn taste(rates: &GenreRates, card: &ArchiveCard, genres: &[String]) -> Option<f32> {
    unplayed(card).then(|| rates.played(genres)).flatten()
}

/// Nothing shows a play of this item: no recency, not watched, no episode.
/// The panel's cards derive every field from plays before the cut, so for a
/// panel row this is exactly "no play before the cut".
pub fn unplayed(card: &ArchiveCard) -> bool {
    card.last_watched_days.is_none()
        && card.is_watched != Some(true)
        && !matches!(card.season_state, Some(SeasonState::Partial | SeasonState::Completed))
        && card.episodes_watched.unwrap_or(0) == 0
}

/// Every item's genres, normalised once for counting.
#[derive(Debug, Default)]
pub struct ItemGenres<'a>(HashMap<&'a str, Vec<String>>);

impl<'a> ItemGenres<'a> {
    pub fn of(items: &'a [FitItem]) -> Self {
        Self(items.iter().map(|item| (item.id.as_str(), normalise(&item.genres))).collect())
    }

    /// The normalised genres of the item with this id; none for an unknown id.
    pub fn of_id(&self, id: &str) -> &[String] {
        self.0.get(id).map_or(&[], Vec::as_slice)
    }
}

/// Genre names as counted: trimmed, lower-case, each once. Radarr and Sonarr
/// spell the same genre alike, but not always in the same case.
fn normalise(genres: &[String]) -> Vec<String> {
    let mut names: Vec<String> = genres.iter().map(|genre| genre.trim().to_lowercase()).filter(|genre| !genre.is_empty()).collect();
    names.sort_unstable();
    names.dedup();
    names
}

#[cfg(test)]
mod tests;
