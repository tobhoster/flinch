//! Upgrade churn: items Radarr or Sonarr keep downloading again. A profile
//! whose cutoff a release never meets, or custom-format scores that keep
//! out-bidding each other, re-grab the same title week after week; every
//! grab is bandwidth, indexer quota and, when it imports, a fresh file.
//!
//! Wire shape: `GET /api/v3/history/since?date=&eventType=` answers a bare
//! `HistoryResource` array; event type 1 is `grabbed` and 3
//! `downloadFolderImported` in both apps (Radarr `History.cs`,
//! https://github.com/Radarr/Radarr/blob/develop/src/NzbDrone.Core/History/History.cs;
//! Sonarr `EpisodeHistory.cs`,
//! https://github.com/Sonarr/Sonarr/blob/v5-develop/src/NzbDrone.Core/History/EpisodeHistory.cs).
//! Sonarr writes one record per episode, so a season pack is one grab per
//! episode sharing a `downloadId`: grabs and imports count distinct downloads.
//!
//! Detection only flags by default; the guard may unmonitor the item or turn
//! upgrades off on its profile when the operator chooses so.

use crate::arr::history::HistoryEpisode;
use crate::capacity::App;
use crate::presence;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

/// How far back grabs count.
pub const WINDOW_SECS: u64 = 30 * 86_400;
/// How long one read of an app's grabs and imports serves.
pub const REFRESH_SECS: u64 = 6 * 3_600;
/// How long a guard step stays recorded: within it the item is not touched again.
pub const APPLIED_KEEP_SECS: u64 = 90 * 86_400;
/// Bounds of [`UpgradeGuardConfig::max_grabs_per_item_30d`].
pub const MAX_GRABS_LIMIT: u32 = 100;

/// `settings.json` `upgrade_guard`. Every field defaults individually.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpgradeGuardConfig {
    /// Read the grab history and flag churning items.
    pub enabled: bool,
    /// Grabs of one item within 30 days above which it is flagged.
    pub max_grabs_per_item_30d: u32,
    /// What happens to a flagged item.
    pub action: GuardAction,
}

impl Default for UpgradeGuardConfig {
    fn default() -> Self {
        Self { enabled: true, max_grabs_per_item_30d: 5, action: GuardAction::Flag }
    }
}

/// A [`UpgradeGuardConfig`] outside its bounds; the message names the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidUpgradeGuardConfig(pub &'static str);

impl UpgradeGuardConfig {
    pub fn validate(&self) -> Result<(), InvalidUpgradeGuardConfig> {
        if !(1..=MAX_GRABS_LIMIT).contains(&self.max_grabs_per_item_30d) {
            return Err(InvalidUpgradeGuardConfig("grabs per item in 30 days (upgrade_guard.max_grabs_per_item_30d) must be 1 to 100"));
        }
        Ok(())
    }
}

/// What the guard does to a churning item.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardAction {
    /// Publish it, change nothing.
    #[default]
    Flag,
    /// Unmonitor the movie, or the season's episodes.
    Unmonitor,
    /// Turn `upgradeAllowed` off on its quality profile: every item on that
    /// profile stops upgrading.
    UpgradesOff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Grab,
    Import,
}

impl EventKind {
    fn code(self) -> u32 {
        match self {
            EventKind::Grab => 1,
            EventKind::Import => 3,
        }
    }
}

/// The `/api/v3/history/since` path and query for one kind of event within
/// the window ending at `now`. Sonarr names the season only on the episode.
pub fn events_path(app: App, kind: EventKind, now: u64) -> String {
    let since = presence::format_utc(now.saturating_sub(WINDOW_SECS));
    let episode = match app {
        App::Radarr => "",
        App::Sonarr => "&includeEpisode=true",
    };
    format!("/api/v3/history/since?date={since}&eventType={}{episode}", kind.code())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    id: u64,
    date: String,
    #[serde(default)]
    movie_id: Option<u32>,
    #[serde(default)]
    series_id: Option<u32>,
    #[serde(default)]
    episode: Option<HistoryEpisode>,
    #[serde(default)]
    download_id: Option<String>,
}

/// One grab or import of a card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub card_id: String,
    /// The download it belongs to; a record without one stands alone.
    pub download: String,
    pub at: u64,
    pub kind: EventKind,
}

/// The events in an instance's `history/since` rows. A record that names no
/// item of the app, no season (Sonarr) or no date is skipped.
pub fn parse_events(app: App, instance: &str, kind: EventKind, rows: Vec<serde_json::Value>) -> Vec<Event> {
    let parse = |record: Record| {
        let card_id = match app {
            App::Radarr => crate::ids::movie_card_id(instance, record.movie_id.filter(|id| *id > 0)?),
            App::Sonarr => crate::ids::season_card_id(instance, record.series_id.filter(|id| *id > 0)?, record.episode?.season_number),
        };
        let at = presence::parse_utc(&record.date)?;
        let download = record.download_id.filter(|id| !id.is_empty()).unwrap_or_else(|| format!("record:{}", record.id));
        Some(Event { card_id, download, at, kind })
    };
    crate::arr::parse_rows::<Record>(rows).parsed.into_iter().filter_map(parse).collect()
}

/// One churning item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Churn {
    pub card_id: String,
    /// Distinct downloads grabbed within the window.
    pub grabs: u32,
    /// Distinct downloads imported within the window.
    pub imports: u32,
    pub last_grab_at: u64,
}

/// The cards grabbed more than `limit` times within the window ending at
/// `now`, most grabbed first.
pub fn detect<'a>(events: impl IntoIterator<Item = &'a Event>, limit: u32, now: u64) -> Vec<Churn> {
    let since = now.saturating_sub(WINDOW_SECS);
    let mut per_card: BTreeMap<&str, (HashSet<&str>, HashSet<&str>, u64)> = BTreeMap::new();
    for event in events.into_iter().filter(|event| event.at >= since) {
        let (grabs, imports, last) = per_card.entry(event.card_id.as_str()).or_default();
        match event.kind {
            EventKind::Grab => {
                grabs.insert(event.download.as_str());
                *last = (*last).max(event.at);
            }
            EventKind::Import => {
                imports.insert(event.download.as_str());
            }
        }
    }
    let count = |set: &HashSet<&str>| u32::try_from(set.len()).unwrap_or(u32::MAX);
    let mut churn: Vec<Churn> = per_card
        .into_iter()
        .filter(|(_, (grabs, _, _))| count(grabs) > limit)
        .map(|(card_id, (grabs, imports, last_grab_at))| Churn {
            card_id: card_id.to_owned(),
            grabs: count(&grabs),
            imports: count(&imports),
            last_grab_at,
        })
        .collect();
    churn.sort_by(|a, b| b.grabs.cmp(&a.grabs).then_with(|| a.card_id.cmp(&b.card_id)));
    churn
}

/// One read of an app's grabs and imports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRead {
    pub read_at: u64,
    pub events: Vec<Event>,
}

/// The event cache (`arr-grabs.json`): each instance's last read, kept
/// independently so one instance's outage never discards another's. The
/// default instances keep the slots they always had.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct EventCache {
    #[serde(default)]
    pub radarr: Option<EventRead>,
    #[serde(default)]
    pub sonarr: Option<EventRead>,
    /// Every other instance's, by [`crate::ids::instance_key`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, EventRead>,
}

impl EventCache {
    pub fn read(&self, app: App, instance: &str) -> Option<&EventRead> {
        match (app, instance.is_empty()) {
            (App::Radarr, true) => self.radarr.as_ref(),
            (App::Sonarr, true) => self.sonarr.as_ref(),
            (_, false) => self.extra.get(&crate::ids::instance_key(app, instance)),
        }
    }

    pub fn store(&mut self, app: App, instance: &str, read: EventRead) {
        match (app, instance.is_empty()) {
            (App::Radarr, true) => self.radarr = Some(read),
            (App::Sonarr, true) => self.sonarr = Some(read),
            (_, false) => {
                self.extra.insert(crate::ids::instance_key(app, instance), read);
            }
        }
    }

    /// Whether the instance's last read is younger than [`REFRESH_SECS`]. A
    /// read dated in the future (a clock step back) is stale.
    pub fn is_fresh(&self, app: App, instance: &str, now: u64) -> bool {
        self.read(app, instance).is_some_and(|read| read.read_at <= now && now - read.read_at < REFRESH_SECS)
    }

    pub fn events(&self) -> impl Iterator<Item = &Event> {
        [self.radarr.as_ref(), self.sonarr.as_ref()].into_iter().flatten().chain(self.extra.values()).flat_map(|read| &read.events)
    }
}

/// A guard step taken on one card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applied {
    pub action: GuardAction,
    pub at: u64,
    /// The profile turned off, for [`GuardAction::UpgradesOff`].
    #[serde(default)]
    pub profile: Option<u32>,
}

/// The guard's record (`upgrade-guard.json`): card id → its step. A card is
/// stepped on once per [`APPLIED_KEEP_SECS`], so an operator who undoes the
/// step is not overruled every cycle.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct GuardLedger {
    #[serde(default)]
    pub applied: BTreeMap<String, Applied>,
}

impl GuardLedger {
    pub fn prune(&mut self, now: u64) {
        self.applied.retain(|_, applied| applied.at > now || now - applied.at < APPLIED_KEEP_SECS);
    }

    /// Whether FLINCH already turned upgrades off on `profile` of the
    /// instance `card_id` belongs to: profile ids are per instance.
    pub fn profile_off(&self, card_id: &str, profile: u32) -> bool {
        let instance = |id: &str| crate::ids::ArrRef::card(id).map(|item| (item.app, item.instance.to_string()));
        let wanted = instance(card_id);
        self.applied
            .iter()
            .any(|(id, applied)| applied.action == GuardAction::UpgradesOff && applied.profile == Some(profile) && instance(id) == wanted)
    }
}

/// One flagged item, for the status page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChurnItem {
    #[serde(flatten)]
    pub churn: Churn,
    pub title: String,
    /// The step taken on it, when one was.
    pub applied: Option<Applied>,
    /// Why the chosen step was not taken, when it was not.
    pub note: Option<String>,
}

/// `status.json` `upgrade_churn`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChurnStatus {
    pub limit: u32,
    pub action: GuardAction,
    pub items: Vec<ChurnItem>,
    /// What could not be read or written, one sentence each.
    pub problems: Vec<String>,
}

#[cfg(test)]
mod tests;
