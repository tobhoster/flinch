//! Recency weighting of taste's outcomes, when the operator asks for it.
//!
//! Tastes drift: what the household played three years ago says less about
//! tonight than what it played last month. With a half-life set, every closed
//! outcome counts `exp(-age / τ)` instead of 1, its age measured from when its
//! window closed (`cut + horizon`) to the record's date, τ = half-life / ln 2.
//! The same weights apply wherever a [`super::Record`] is built — each panel
//! row's own as-of record and the daemon's — so the fit sees exactly what the
//! daemon will ask with, and no outcome closed after a cut ever enters it.

/// Taste as the operator configured it (`settings.json` `taste`).
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct TasteConfig {
    /// Days for an outcome's weight to halve; 0 counts every outcome alike.
    pub half_life_days: f64,
}

/// Bounds of a non-zero [`TasteConfig::half_life_days`]: a week to ten years.
pub const HALF_LIFE_DAYS: (f64, f64) = (7.0, 3650.0);

/// A [`TasteConfig`] outside its bounds; the message names the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidTasteConfig(pub &'static str);

impl TasteConfig {
    pub fn validate(&self) -> Result<(), InvalidTasteConfig> {
        let (low, high) = HALF_LIFE_DAYS;
        if self.half_life_days == 0.0 || (low..=high).contains(&self.half_life_days) {
            return Ok(());
        }
        Err(InvalidTasteConfig("the taste half-life (taste.half_life_days) must be 0 (off) or 7 to 3650 days"))
    }

    pub fn decay(&self) -> Decay {
        Decay::half_life_days(self.half_life_days)
    }
}

/// How much an outcome closed some seconds ago weighs.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Decay {
    /// τ in seconds; `None` weighs every outcome 1.
    tau_secs: Option<f64>,
}

impl Decay {
    /// Every outcome weighs 1.
    pub const OFF: Self = Self { tau_secs: None };

    /// Off unless `days` is positive and finite.
    pub fn half_life_days(days: f64) -> Self {
        let on = days.is_finite() && days > 0.0;
        Self { tau_secs: on.then(|| days * 86_400.0 / std::f64::consts::LN_2) }
    }

    /// The weight of an outcome whose window closed at `closed`, as of `as_of`.
    pub fn weight(self, closed: u64, as_of: u64) -> f64 {
        match self.tau_secs {
            Some(tau) => (-(as_of.saturating_sub(closed) as f64) / tau).exp(),
            None => 1.0,
        }
    }
}
