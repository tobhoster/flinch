//! The weight table: one logit weight per named feature of
//! [`super::features`], the hand-set priors, and which of them training may move.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreWeights {
    /// Base rate, in logit space. Zero for the hand-set prior model.
    pub bias: f32,
    /// Nothing was ever played: the strongest reclaim signal available.
    pub never_played: f32,
    /// Partially played: less safe than never played, safer than completed.
    pub partially_played: f32,
    /// Dwell time on disk, per year, capped.
    pub dwell_per_year: f32,
    /// Recency of play, penalised (recently played ⇒ keep).
    pub recent_play: f32,
    /// A sibling season was played ⇒ this show matters to the household.
    /// Applies only to items the household has not finished themselves.
    pub sibling_played: f32,
    /// A sibling season was *completed* ⇒ strong keep for an unfinished season:
    /// the household is working through the show.
    pub sibling_completed: f32,
    /// Finished and untouched for [`super::COMPLETED_COLD_DAYS`]: the classic
    /// safe reclaim (a watched film, a completed season nobody has reopened).
    pub completed_cold: f32,
    /// The newest aired season is never a candidate.
    pub newest_season: f32,
    /// Favorites and keep-collections are absolute.
    pub protected_by_tag: f32,
    /// Duplicates are cheap to reclaim.
    pub duplicate: f32,
    /// Larger items are better first targets (bytes reclaimed per operation).
    pub size_per_10gib: f32,
    /// No watch evidence at all: fail-closed. Dwell time alone must never clear
    /// the floor, or the model would reward never having asked.
    pub no_evidence: f32,
    /// Actively watched within the recency window. Shares `recent_play`'s value
    /// in the prior model; separable once fitted.
    pub active: f32,
    /// Evidence of a rewatch. The household signals below carry a zero prior:
    /// they describe habits the hand-set model has no business guessing, so
    /// only a fit on this household's own outcomes gives them a weight.
    pub rewatched: f32,
    /// Per extra household viewer beyond the first, capped at [`super::MAX_EXTRA_VIEWERS`].
    pub viewer_breadth: f32,
    /// The series has ended: no new season will pull the household back to it.
    pub series_ended: f32,
    /// The household's play rate for an unplayed item's genres, as the logit
    /// of P(nobody plays it). Zero prior like the household signals: only the
    /// held-out fit may say it helps.
    pub taste: f32,
}

impl ScoreWeights {
    /// Look up a feature's weight. Unknown names are ignored by the dot product,
    /// so a weights file from a newer build degrades to the features it shares.
    pub fn get(&self, name: &str) -> f32 {
        match name {
            "bias" => self.bias,
            "never_played" => self.never_played,
            "partially_played" => self.partially_played,
            "dwell" => self.dwell_per_year,
            "recent_play" => self.recent_play,
            "sibling_played" => self.sibling_played,
            "sibling_completed" => self.sibling_completed,
            "completed_cold" => self.completed_cold,
            "newest_season" => self.newest_season,
            "protected_by_tag" => self.protected_by_tag,
            "duplicate" => self.duplicate,
            "size" => self.size_per_10gib,
            "no_evidence" => self.no_evidence,
            "active" => self.active,
            "rewatched" => self.rewatched,
            "viewer_breadth" => self.viewer_breadth,
            "series_ended" => self.series_ended,
            "taste" => self.taste,
            _ => 0.0,
        }
    }

    /// Set a feature's weight. `false` for a name this table does not carry,
    /// so a caller can tell a dropped weight from an applied one.
    pub fn set(&mut self, name: &str, value: f32) -> bool {
        let slot = match name {
            "bias" => &mut self.bias,
            "never_played" => &mut self.never_played,
            "partially_played" => &mut self.partially_played,
            "dwell" => &mut self.dwell_per_year,
            "recent_play" => &mut self.recent_play,
            "sibling_played" => &mut self.sibling_played,
            "sibling_completed" => &mut self.sibling_completed,
            "completed_cold" => &mut self.completed_cold,
            "newest_season" => &mut self.newest_season,
            "protected_by_tag" => &mut self.protected_by_tag,
            "duplicate" => &mut self.duplicate,
            "size" => &mut self.size_per_10gib,
            "no_evidence" => &mut self.no_evidence,
            "active" => &mut self.active,
            "rewatched" => &mut self.rewatched,
            "viewer_breadth" => &mut self.viewer_breadth,
            "series_ended" => &mut self.series_ended,
            "taste" => &mut self.taste,
            _ => return false,
        };
        *slot = value;
        true
    }

    /// Names this table can weight, in a stable order.
    pub fn names() -> &'static [&'static str] {
        &[
            "never_played",
            "partially_played",
            "dwell",
            "recent_play",
            "sibling_played",
            "sibling_completed",
            "completed_cold",
            "newest_season",
            "protected_by_tag",
            "duplicate",
            "size",
            "no_evidence",
            "active",
            "rewatched",
            "viewer_breadth",
            "series_ended",
            "taste",
        ]
    }

    /// Features whose prior is a policy decision rather than a belief about the
    /// household, and which training must not move: the fail-closed ignorance
    /// penalty and the structural tag guards.
    pub fn frozen() -> &'static [&'static str] {
        &["no_evidence", "protected_by_tag"]
    }
}

impl Default for ScoreWeights {
    fn default() -> Self {
        // Priors, not constants of nature: `calibrate` in the UI/history loop is
        // what moves these. Signs carry the domain knowledge; magnitudes are
        // deliberately conservative so the guard under-reclaims rather than
        // surprises.
        Self {
            bias: 0.0,
            never_played: 1.6,
            partially_played: 0.2,
            dwell_per_year: 0.45,
            recent_play: -1.4,
            sibling_played: -0.9,
            sibling_completed: -1.8,
            // As strong as never-played: both say nobody is coming back. Without
            // it the priors ranked a film finished a year ago below one nobody
            // ever opened, and the default floor could never free watched space.
            completed_cold: 1.6,
            newest_season: -3.0,
            protected_by_tag: -6.0,
            duplicate: 1.2,
            size_per_10gib: 0.08,
            no_evidence: -2.2,
            active: -1.4,
            rewatched: 0.0,
            viewer_breadth: 0.0,
            series_ended: 0.0,
            taste: 0.0,
        }
    }
}
