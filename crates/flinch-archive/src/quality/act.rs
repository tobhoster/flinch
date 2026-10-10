//! Acting on downgrade advice, when the operator asks (`quality_actions`, off
//! by default): move the item to the compact quality profile and ask its *arr
//! to search, so a smaller copy replaces the large file and the title stays.
//!
//! Why a profile move downgrades: a quality the new profile does not list
//! ranks lowest under it (`QualityProfile.GetIndex` falls through to index 0,
//! https://github.com/Radarr/Radarr/blob/develop/src/NzbDrone.Core/Profiles/Qualities/QualityProfile.cs),
//! so any allowed release reads as an upgrade and replaces the file. A
//! compact profile that lists the current quality leaves the file alone;
//! the ledger then records that nothing smaller landed.
//!
//! Never acted on: a pinned item, one someone is partway through, one without
//! watch evidence, one a rule forbids or forces out, one the plan evicts or
//! already handed over, one moved
//! before. Never beyond the daily cap, and never without a release on the
//! indexers at least 30% smaller ([`SMALLER_SHARE`]) unless the operator
//! waived that. Sonarr keeps one profile per series, so a show moves only
//! when every season on disk is advised a downgrade: otherwise a season
//! advised to keep its original would follow it.

use super::QualityAction;
use crate::arr::{ArrMovie, ArrSeries};
use crate::capacity::App;
use crate::plan::{Exclusion, MediaCandidate};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

mod ledger;
pub use ledger::{Action, ActionLedger, Outcome, LANDED_SHARE, LEDGER_KEEP_SECS, OUTCOME_WINDOW_SECS, STATUS_ROWS};

pub const DAY_SECS: u64 = 86_400;
/// A release counts as compact at most this share of the file: 30% smaller.
pub const SMALLER_SHARE: f64 = 0.70;
/// Bound of [`QualityActionsConfig::max_per_day`].
pub const MAX_PER_DAY: u32 = 50;

/// `settings.json` `quality_actions`. Every field defaults individually.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QualityActionsConfig {
    /// Move items advised a downgrade. Off: advice stays advice.
    pub enabled: bool,
    /// Items moved per 24 hours at most; each season counts.
    pub max_per_day: u32,
    /// Move only when Prowlarr lists a release at least 30% smaller.
    pub require_smaller_release: bool,
    /// The compact profile's name in Radarr, used when the quality sync
    /// manages none. Blank: no fallback.
    pub radarr_profile: String,
    /// The same for Sonarr.
    pub sonarr_profile: String,
}

impl Default for QualityActionsConfig {
    fn default() -> Self {
        Self { enabled: false, max_per_day: 3, require_smaller_release: true, radarr_profile: String::new(), sonarr_profile: String::new() }
    }
}

/// A [`QualityActionsConfig`] outside its bounds; the message names the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidQualityActionsConfig(pub &'static str);

impl QualityActionsConfig {
    pub fn validate(&self) -> Result<(), InvalidQualityActionsConfig> {
        if !(1..=MAX_PER_DAY).contains(&self.max_per_day) {
            return Err(InvalidQualityActionsConfig("quality actions per day (quality_actions.max_per_day) must be 1 to 50"));
        }
        Ok(())
    }

    /// The default instance's fallback profile name for `app`, when one is
    /// set; an extra instance names its own ([`crate::arr::instances`]).
    pub fn profile_name(&self, app: App) -> Option<&str> {
        let name = match app {
            App::Radarr => &self.radarr_profile,
            App::Sonarr => &self.sonarr_profile,
        };
        Some(name.trim()).filter(|name| !name.is_empty())
    }
}

/// Where a card lives in its *arr: a movie, or a season of a series, of one
/// instance (borrowed from the library read; empty for the default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArrItem<'a> {
    pub app: App,
    pub instance: &'a str,
    /// The movie id, or the series id.
    pub id: u32,
    pub season: Option<u32>,
    /// The quality profile (the series' for a season); `None` when unread.
    pub profile: Option<u32>,
}

/// Every movie and season of the libraries by card id, with or without files.
pub fn arr_items<'a>(movies: &'a [ArrMovie], series: &'a [ArrSeries]) -> HashMap<String, ArrItem<'a>> {
    let movies = movies.iter().map(|movie| {
        let item = ArrItem { app: App::Radarr, instance: &movie.instance, id: movie.id, season: None, profile: movie.quality_profile_id };
        (movie.card_id(), item)
    });
    let seasons = series.iter().flat_map(|show| {
        show.seasons.iter().map(move |season| {
            let item = ArrItem {
                app: App::Sonarr,
                instance: &show.instance,
                id: show.id,
                season: Some(season.season_number),
                profile: show.quality_profile_id,
            };
            (show.season_card_id(season.season_number), item)
        })
    });
    movies.chain(seasons).collect()
}

/// The compact profile id in each instance (profile ids are per instance);
/// absent when there is none. A handful of instances: a list is enough.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Compact {
    /// (app, instance, profile id); the instance empty for the default.
    pub profiles: Vec<(App, String, u32)>,
}

impl Compact {
    pub fn of(&self, app: App, instance: &str) -> Option<u32> {
        self.profiles.iter().find(|(known, name, _)| *known == app && name == instance).map(|(_, _, profile)| *profile)
    }

    pub fn set(&mut self, app: App, instance: &str, profile: u32) {
        self.profiles.retain(|(known, name, _)| !(*known == app && name == instance));
        self.profiles.push((app, instance.to_string(), profile));
    }
}

/// What a selection reads.
pub struct Inputs<'a> {
    pub candidates: &'a [MediaCandidate],
    pub items: &'a HashMap<String, ArrItem<'a>>,
    /// Cards this cycle's plan evicts.
    pub evicting: &'a HashSet<&'a str>,
    /// Card id → the smallest whole release Prowlarr found, in bytes.
    pub smallest_release: &'a HashMap<String, u64>,
    /// The planner's grace period: a file newer than this is left to settle.
    pub grace_days: u32,
    pub compact: Compact,
    pub now: u64,
}

/// One profile move: a movie, or a series with every season it searches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Move {
    pub app: App,
    /// The instance it moves in; empty for the default.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instance: String,
    /// The movie id, or the series id.
    pub id: u32,
    pub from_profile: u32,
    pub to_profile: u32,
    pub cards: Vec<MoveCard>,
}

impl Move {
    pub fn bytes(&self) -> u64 {
        self.cards.iter().map(|card| card.bytes).sum()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveCard {
    pub card_id: String,
    pub title: String,
    pub season: Option<u32>,
    /// Its bytes on disk when moved.
    pub bytes: u64,
    /// The smallest whole release Prowlarr listed, when known.
    pub release_bytes: Option<u64>,
}

/// Why an item advised a downgrade was not moved this cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hold {
    /// The *arr did not say which profile it is on.
    ProfileUnknown,
    /// No compact profile exists for its app.
    NoCompactProfile,
    AlreadyCompact,
    /// Prowlarr lists no release at least 30% smaller (or was not asked yet).
    NoSmallerRelease,
    /// Another season of the show keeps its original or cannot move.
    OtherSeasonKeeps,
    DailyCap,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    pub card_id: String,
    pub title: String,
    pub hold: Hold,
}

#[derive(Debug, Default, PartialEq)]
pub struct Selection {
    pub moves: Vec<Move>,
    pub held: Vec<Held>,
}

/// Whether an exclusion still lets the title's quality change: an item kept
/// out of the plan only for its disk or for never having been played still
/// keeps the title. Every other reason (and any added later) holds it.
fn allows(exclusion: Option<&Exclusion>) -> bool {
    matches!(exclusion, None | Some(Exclusion::NoGovernedDisk | Exclusion::NeverPlayedOff | Exclusion::NeverPlayedHeld(_)))
}

/// Whether `release` bytes are at least 30% smaller than `file` bytes.
pub fn compact_enough(release: u64, file: u64) -> bool {
    file > 0 && release as f64 <= file as f64 * SMALLER_SHARE
}

/// This cycle's moves, biggest first within the day's remaining cap, and why
/// every other advised item waits.
pub fn select(inputs: &Inputs<'_>, ledger: &ActionLedger, config: &QualityActionsConfig) -> Selection {
    let mut held = Vec::new();
    // *arr ids repeat across instances: a series is its instance and id.
    let mut seasons_on_disk: HashMap<(&str, u32), usize> = HashMap::new();
    let mut moves: BTreeMap<(App, &str, u32), Move> = BTreeMap::new();
    for candidate in inputs.candidates {
        let Some(item) = inputs.items.get(&candidate.id) else { continue };
        if item.app == App::Sonarr {
            *seasons_on_disk.entry((item.instance, item.id)).or_default() += 1;
        }
        let advised = matches!(candidate.quality.action, QualityAction::DowngradeQuality { .. });
        // A rule that forces the item out wants it gone, not smaller.
        let untouchable = candidate.protect
            || candidate.handed
            || candidate.force.is_some()
            || candidate.age_days < inputs.grace_days as f32
            || !allows(candidate.exclusion.as_ref());
        if !advised || untouchable || inputs.evicting.contains(candidate.id.as_str()) || ledger.blocks(&candidate.id, inputs.now) {
            continue;
        }
        let release_bytes = inputs.smallest_release.get(&candidate.id).copied();
        let smaller = release_bytes.is_some_and(|bytes| compact_enough(bytes, candidate.size_bytes));
        let target = match (item.profile, inputs.compact.of(item.app, item.instance)) {
            (None, _) => Err(Hold::ProfileUnknown),
            (_, None) => Err(Hold::NoCompactProfile),
            (Some(from), Some(to)) if from == to => Err(Hold::AlreadyCompact),
            _ if config.require_smaller_release && !smaller => Err(Hold::NoSmallerRelease),
            (Some(from), Some(to)) => Ok((from, to)),
        };
        let card = MoveCard {
            card_id: candidate.id.clone(),
            title: candidate.title.clone(),
            season: item.season,
            bytes: candidate.size_bytes,
            release_bytes,
        };
        match target {
            Ok((from_profile, to_profile)) => moves
                .entry((item.app, item.instance, item.id))
                .or_insert_with(|| Move {
                    app: item.app,
                    instance: item.instance.to_string(),
                    id: item.id,
                    from_profile,
                    to_profile,
                    cards: Vec::new(),
                })
                .cards
                .push(card),
            Err(hold) => held.push(Held { card_id: card.card_id, title: card.title, hold }),
        }
    }
    let mut ready = Vec::new();
    for movement in moves.into_values() {
        let whole = movement.app == App::Radarr
            || seasons_on_disk.get(&(movement.instance.as_str(), movement.id)).copied() == Some(movement.cards.len());
        if whole {
            ready.push(movement);
        } else {
            held.extend(hold_all(movement, Hold::OtherSeasonKeeps));
        }
    }
    // Biggest first; the key keeps equal sizes in a stable order.
    ready.sort_by(|a, b| b.bytes().cmp(&a.bytes()).then((a.app, &a.instance, a.id).cmp(&(b.app, &b.instance, b.id))));
    let mut budget = (config.max_per_day as usize).saturating_sub(ledger.acted_since(inputs.now.saturating_sub(DAY_SECS)));
    let mut selection = Selection { moves: Vec::new(), held };
    for movement in ready {
        if movement.cards.len() <= budget {
            budget -= movement.cards.len();
            selection.moves.push(movement);
        } else {
            selection.held.extend(hold_all(movement, Hold::DailyCap));
        }
    }
    selection
}

fn hold_all(movement: Move, hold: Hold) -> impl Iterator<Item = Held> {
    movement.cards.into_iter().map(move |card| Held { card_id: card.card_id, title: card.title, hold })
}

/// `status.json` `quality_actions`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QualityActionsStatus {
    pub enabled: bool,
    /// This cycle's moves were printed, not sent.
    pub dry_run: bool,
    pub max_per_day: u32,
    /// Items moved (or tried) in the last 24 hours.
    pub acted_today: usize,
    /// This cycle's moves.
    pub moves: Vec<Move>,
    /// Advised items not moved this cycle, and why.
    pub held: Vec<Held>,
    /// The newest moves and what became of them.
    pub recent: Vec<Action>,
    /// Bytes the landed moves gave back.
    pub reclaimed_bytes: u64,
    /// What could not be read or written this cycle, one sentence each.
    pub problems: Vec<String>,
}

#[cfg(test)]
mod tests;
