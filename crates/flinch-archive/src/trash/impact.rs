//! What a previewed change would cost on disk, in GiB, before it is applied.
//!
//! Why: a profile's cutoff and the size table decide what every future
//! download of its items weighs, and the operator approves the change on the
//! Quality profiles page; FLINCH governs the disks it lands on.
//!
//! The estimate, per item on the profile, from the Prowlarr release sizes
//! FLINCH already caches ([`crate::signals::release`]: the smallest and the
//! largest whole release found for the title):
//!
//! - a profile reaches a fraction `f` of the release spread: the max size
//!   (MB/min) of its cutoff quality over the largest max of the table;
//! - an item weighs `smallest + (largest − smallest) × f`, so a change moves
//!   it by `(largest − smallest) × (f_after − f_before)`;
//! - items without a cached search count at the mean of those with one.
//!
//! It is a forecast of where upgrades settle, not of what is on disk today;
//! the page says so. The disk line is the last forecast's projected use across
//! the library volumes, before and after the change.

use super::client::{ArrProfile, ArrProfileItem, Live};
use super::desired::{limits, Desired, DesiredProfile};
use super::diff::{Action, Change, Kind};
use crate::arr::{ArrMovie, ArrSeries};
use crate::capacity::App;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// One item's cached release sizes, in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sizes {
    pub smallest: Option<u64>,
    pub largest: Option<u64>,
}

/// Which items each profile of one app holds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    /// Profile id → its items' release sizes.
    pub profiles: BTreeMap<u32, Vec<Sizes>>,
    /// False when some item's profile could not be read: then no profile is
    /// known to be unused.
    pub complete: bool,
}

impl Usage {
    /// `instance`'s movies (Radarr) or seasons (Sonarr) by profile, with the
    /// cached release sizes by card id; profile ids are per instance, so no
    /// other instance's items count.
    pub fn of(
        (app, instance): (App, &str),
        movies: &[ArrMovie],
        series: &[ArrSeries],
        smallest: &HashMap<String, u64>,
        largest: &HashMap<String, u64>,
    ) -> Self {
        let sizes = |card: &str| Sizes { smallest: smallest.get(card).copied(), largest: largest.get(card).copied() };
        let items: Vec<(Option<u32>, Sizes)> = match app {
            App::Radarr => movies
                .iter()
                .filter(|movie| movie.instance == instance)
                .map(|movie| (movie.quality_profile_id, sizes(&movie.card_id())))
                .collect(),
            App::Sonarr => series
                .iter()
                .filter(|show| show.instance == instance)
                .flat_map(|show| {
                    show.seasons.iter().map(move |season| (show.quality_profile_id, show.season_card_id(season.season_number)))
                })
                .map(|(profile, card)| (profile, sizes(&card)))
                .collect(),
        };
        let mut usage = Usage { profiles: BTreeMap::new(), complete: true };
        for (profile, sizes) in items {
            match profile {
                Some(id) => usage.profiles.entry(id).or_default().push(sizes),
                None => usage.complete = false,
            }
        }
        usage
    }

    pub fn items(&self, profile: u32) -> usize {
        self.profiles.get(&profile).map_or(0, Vec::len)
    }
}

/// The last capacity forecast, summed over the library volumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskOutlook {
    pub projected_used_bytes: u64,
    pub capacity_bytes: u64,
}

/// One change's estimate, as the page shows it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Impact {
    /// Items on the profile (or, for sizes, on every profile).
    pub items: u32,
    /// Of those, items with a cached release search.
    pub sampled: u32,
    /// Estimated change in bytes once upgrades settle; negative frees space.
    pub delta_bytes: i64,
    /// Projected use over capacity, before and after, when a forecast exists.
    pub utilization_before: Option<f64>,
    pub utilization_after: Option<f64>,
    pub warnings: Vec<String>,
}

/// Quality name → max MB/min, with an unlimited max read as the app's limit.
type Table = BTreeMap<String, f64>;

fn live_table(app: App, live: &Live) -> Table {
    live.definitions.iter().map(|d| (d.typed.quality.name.clone(), d.typed.max_size.unwrap_or(limits(app).max))).collect()
}

fn desired_table(app: App, live: &Live, desired: &Desired<'_>) -> Table {
    let mut table = live_table(app, live);
    for target in desired.sizes.iter().flat_map(|sizes| &sizes.qualities) {
        table.insert(target.quality.clone(), target.max);
    }
    table
}

/// The share of the release spread a cutoff reaches under `table`.
fn reach(table: &Table, qualities: &[String]) -> Option<f64> {
    let top = table.values().copied().fold(0.0_f64, f64::max);
    let cap = qualities
        .iter()
        .filter_map(|q| table.get(q))
        .copied()
        .fold(None, |best: Option<f64>, max| Some(best.map_or(max, |b| b.max(max))))?;
    (top > 0.0).then(|| (cap / top).clamp(0.0, 1.0))
}

fn live_cutoff(profile: &ArrProfile) -> Vec<String> {
    let Some(item) = profile.items.iter().find(|item| item.cutoff_id() == Some(profile.cutoff)) else { return Vec::new() };
    if item.items.is_empty() {
        vec![item.label().to_string()]
    } else {
        item.items.iter().map(ArrProfileItem::label).map(str::to_string).collect()
    }
}

fn desired_cutoff(profile: &DesiredProfile) -> Vec<String> {
    match profile.ladder.iter().find(|rung| rung.name == profile.cutoff) {
        Some(rung) if !rung.members.is_empty() => rung.members.clone(),
        _ => vec![profile.cutoff.clone()],
    }
}

/// Bytes the items move by when their reach goes from `before` to `after`.
fn shift(items: &[Sizes], before: f64, after: f64) -> (u32, i64) {
    let spreads: Vec<f64> = items.iter().filter_map(|s| Some(s.largest?.saturating_sub(s.smallest?) as f64)).collect();
    if spreads.is_empty() {
        return (0, 0);
    }
    let mean = spreads.iter().sum::<f64>() / spreads.len() as f64;
    let total = mean * items.len() as f64 * (after - before);
    (u32::try_from(spreads.len()).unwrap_or(u32::MAX), total.round() as i64)
}

fn finish(items: usize, (sampled, delta_bytes): (u32, i64), outlook: Option<DiskOutlook>, warnings: Vec<String>) -> Impact {
    let ratio = |used: i64, cap: u64| (cap > 0).then(|| used.max(0) as f64 / cap as f64);
    let used = outlook.map(|o| i64::try_from(o.projected_used_bytes).unwrap_or(i64::MAX));
    Impact {
        items: u32::try_from(items).unwrap_or(u32::MAX),
        sampled,
        delta_bytes,
        utilization_before: outlook.zip(used).and_then(|(o, used)| ratio(used, o.capacity_bytes)),
        utilization_after: outlook.zip(used).and_then(|(o, used)| ratio(used.saturating_add(delta_bytes), o.capacity_bytes)),
        warnings,
    }
}

/// Raised maxima of the size change: larger releases become acceptable.
fn raised(app: App, live: &Live, desired: &Desired<'_>) -> Vec<String> {
    let before = live_table(app, live);
    let mut out = Vec::new();
    for target in desired.sizes.iter().flat_map(|sizes| &sizes.qualities) {
        if let Some(old) = before.get(&target.quality).filter(|old| target.max > **old + 0.05) {
            out.push(format!(
                "{}: max size rises from {old} to {} MB/min; larger releases are grabbed and upgrades may follow",
                target.quality, target.max
            ));
        }
    }
    out
}

/// An estimate for every profile and size change the library touches.
pub fn estimate(
    app: App,
    changes: &[Change],
    desired: &Desired<'_>,
    live: &Live,
    usage: &Usage,
    outlook: Option<DiskOutlook>,
) -> BTreeMap<String, Impact> {
    let (old_table, new_table) = (live_table(app, live), desired_table(app, live, desired));
    let mut out = BTreeMap::new();
    for change in changes.iter().filter(|c| c.action == Action::Update) {
        match change.kind {
            Kind::QualityProfile => {
                let Some(id) = change.arr_id else { continue };
                let live_profile = live.profiles.iter().find(|p| p.typed.id == id).map(|p| &p.typed);
                let wanted = desired.profiles.iter().find(|p| Some(p.trash_id.as_str()) == change.trash_id.as_deref());
                let (Some(live_profile), Some(wanted)) = (live_profile, wanted) else { continue };
                let items = usage.profiles.get(&id).map_or(&[][..], Vec::as_slice);
                let (Some(before), Some(after)) =
                    (reach(&old_table, &live_cutoff(live_profile)), reach(&new_table, &desired_cutoff(wanted)))
                else {
                    continue;
                };
                out.insert(change.id.clone(), finish(items.len(), shift(items, before, after), outlook, Vec::new()));
            }
            Kind::QualityDefinition => {
                let (mut items, mut sampled, mut delta) = (0, 0_u32, 0_i64);
                for profile in &live.profiles {
                    let held = usage.profiles.get(&profile.typed.id).map_or(&[][..], Vec::as_slice);
                    let cutoff = live_cutoff(&profile.typed);
                    if let (Some(before), Some(after)) = (reach(&old_table, &cutoff), reach(&new_table, &cutoff)) {
                        let (s, d) = shift(held, before, after);
                        items += held.len();
                        sampled = sampled.saturating_add(s);
                        delta = delta.saturating_add(d);
                    }
                }
                out.insert(change.id.clone(), finish(items, (sampled, delta), outlook, raised(app, live, desired)));
            }
            Kind::CustomFormat => {}
        }
    }
    out
}
