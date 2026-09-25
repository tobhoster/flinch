//! The per-item feature card for the archive reflex's keep/evict decisions.
//!
//! ONE card per library item (a season group or a movie). Everything the model
//! may use must be in here, and nothing that a live *arr instance would not
//! have. The card is the swap point for real data: Sonarr/Radarr history ->
//! this struct via `From<WatchSummary>` in `history.rs`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LibraryKind {
    Season,
    Movie,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SeriesType {
    Standard,
    Anime,
    Documentary,
    Reality,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SeasonState {
    /// All episodes watched to completion.
    Completed,
    /// Some but not all episodes watched.
    Partial,
    /// Nothing watched.
    Empty,
}

/// How far back the item sits in the household's watch rotation. This is the
/// single strongest feature in the archive problem: a completed season that
/// has not been touched in a year is nearly always safe to reclaim; the fuzzy
/// edge is the 60-180 day band where "will they rewatch before the next
/// season?" genuinely varies by household.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Recency {
    /// Touched within the last 30 days — never a candidate.
    Active,
    /// 30-90 days. Only clear-cut signals (favorite, keep-collection) matter.
    Warm,
    /// 90-180 days. Season/movie structure starts to matter.
    Cold,
    /// More than 180 days untouched. Safe unless explicitly protected.
    Coldest,
}

impl Recency {
    pub fn from_days(last_watched_days: Option<f32>) -> Self {
        match last_watched_days {
            None => Self::Coldest, // never watched: not consuming space-for-reward
            Some(days) if days < 30.0 => Self::Active,
            Some(days) if days < 90.0 => Self::Warm,
            Some(days) if days < 180.0 => Self::Cold,
            Some(_) => Self::Coldest,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchiveCard {
    pub id: String,
    pub title: String,
    pub kind: LibraryKind,
    /// Bytes on disk. The whole point: reclaiming this must be worth a delete.
    pub size_bytes: u64,
    /// Days since the library added this item.
    pub added_days_ago: f32,
    /// Days since anyone watched any part. `None` = never watched.
    pub last_watched_days: Option<f32>,
    pub in_keep_collection: bool,
    /// Explicit manual pin; overrides everything except explicit never-delete.
    pub is_favorite: bool,
    pub duplicate_count: u32,

    // Season-only
    #[serde(default)]
    pub series_type: Option<SeriesType>,
    #[serde(default)]
    pub season_state: Option<SeasonState>,
    #[serde(default)]
    pub season_index: Option<u32>,
    #[serde(default)]
    pub is_newest_season: Option<bool>,
    #[serde(default)]
    pub episodes_total: Option<u32>,
    #[serde(default)]
    pub episodes_watched: Option<u32>,

    // Movie-only
    #[serde(default)]
    pub is_watched: Option<bool>,
    /// Release year (movies) — part of the Plex correlation key.
    #[serde(default)]
    pub movie_year: Option<u32>,
    /// Show title without the season suffix (seasons) — the other half of the
    /// Plex correlation key. Kept separate from `title` so display text stays
    /// "Show S2" while matching uses "Show".
    #[serde(default)]
    pub show_title: Option<String>,
    /// 0.0 = no rewatch value, 1.0 = the household rewatches it every year.
    /// In practice this can only come from watch history, and defaults to 0.0
    /// (the conservative value: better to under-delete than over-delete).
    #[serde(default)]
    pub rewatch_score: Option<f32>,
}

impl ArchiveCard {
    /// Every fixed-size feature the model can consume, in a stable order.
    /// The index contract: a trained head depends on this order, so a change
    /// here must invalidate the checkpoint — the hash covers it in `decide.rs`.
    pub fn features(&self) -> Vec<f32> {
        let mut out = Vec::with_capacity(24);
        out.push(self.size_bytes as f32 / 1e9);
        out.push(self.added_days_ago);
        // -1.0 encodes "never watched" distinctly: a huge number would look
        // like a decade-old item, and the Recency feature downstream already
        // codes never-watched as Coldest. Both signals must stay separable.
        out.push(match self.last_watched_days {
            Some(days) => days,
            None => -1.0,
        });
        out.push(self.in_keep_collection as u8 as f32);
        out.push(self.is_favorite as u8 as f32);
        out.push(self.duplicate_count as f32);
        out.push(Recency::from_days(self.last_watched_days) as u8 as f32);

        // Season block
        out.push(match self.series_type {
            Some(SeriesType::Standard) => 0.0,
            Some(SeriesType::Anime) => 1.0,
            Some(SeriesType::Documentary) => 2.0,
            Some(SeriesType::Reality) => 3.0,
            None => -1.0,
        });
        out.push(match self.season_state {
            Some(SeasonState::Completed) => 1.0,
            Some(SeasonState::Partial) => 0.5,
            Some(SeasonState::Empty) => 0.0,
            None => -1.0,
        });
        out.push(self.season_index.unwrap_or(0) as f32);
        out.push(self.is_newest_season.unwrap_or(false) as u8 as f32);
        out.push(self.episodes_total.unwrap_or(0) as f32);
        out.push(match (self.episodes_watched, self.episodes_total) {
            (Some(watched), Some(total)) if total > 0 => watched as f32 / total as f32,
            _ => 0.0,
        });

        // Movie block
        out.push(self.is_watched.unwrap_or(false) as u8 as f32);
        out.push(self.rewatch_score.unwrap_or(0.0));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::golden::golden_movie;

    #[test]
    fn never_watched_is_coldest_not_active() {
        assert_eq!(Recency::from_days(None), Recency::Coldest);
        assert_eq!(Recency::from_days(Some(10.0)), Recency::Active);
        assert_eq!(Recency::from_days(Some(120.0)), Recency::Cold);
    }

    #[test]
    fn feature_vector_is_stable_and_finite() {
        let card = golden_movie();
        let features = card.features();
        assert_eq!(features.len(), 15);
        // A never-finite value would poison the head silently.
        assert!(features.iter().all(|f| f.is_finite()), "features must be finite");
    }

    #[test]
    fn never_watched_encodes_as_minus_one_not_a_huge_age() {
        let mut card = golden_movie();
        card.last_watched_days = None;
        assert_eq!(card.features()[2], -1.0, "never-watched must stay distinct from old");
    }
}
