//! Which items a rule speaks for: every listed condition must hold (a list
//! holds when any entry matches), each answered yes, no, or unknown.

use super::{Event, Facts};
use crate::plan::MediaCandidate;
use serde::{Deserialize, Serialize};

const GIB: f64 = 1_073_741_824.0;
const DAY: f64 = 86_400.0;

/// A three-valued answer: a condition the facts cannot answer is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Truth {
    Yes,
    No,
    Unknown,
}

impl Truth {
    pub(super) fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::No, _) | (_, Self::No) => Self::No,
            (Self::Yes, Self::Yes) => Self::Yes,
            _ => Self::Unknown,
        }
    }

    fn of(known: Option<bool>) -> Self {
        match known {
            Some(true) => Self::Yes,
            Some(false) => Self::No,
            None => Self::Unknown,
        }
    }
}

/// A movie, or one season of a show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Movie,
    Season,
}

/// Inclusive bounds; an absent bound is open.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Range {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

impl Range {
    pub fn is_any(&self) -> bool {
        self.min.is_none() && self.max.is_none()
    }

    fn holds(&self, value: f64) -> bool {
        self.min.is_none_or(|min| value >= min) && self.max.is_none_or(|max| value <= max)
    }

    fn validate(&self, field: &str, ceiling: f64) -> Result<(), String> {
        let bounds = [self.min, self.max];
        if bounds.iter().flatten().any(|bound| !(bound.is_finite() && (0.0..=ceiling).contains(bound))) {
            return Err(format!("{field} bounds must be 0 to {ceiling}"));
        }
        match (self.min, self.max) {
            (Some(min), Some(max)) if min > max => Err(format!("{field} min must not exceed its max")),
            _ => Ok(()),
        }
    }
}

/// The items a rule covers. Empty matches everything (refused for evict rules).
/// Names (tags, requesters, themes, genres) match case-insensitively.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scope {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    /// FLINCH card ids (`radarr-7`, `sonarr-12-s3`): one title, as an
    /// approved household removal names it ([`crate::requests`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
    /// *arr root folders: the item's folder is one of them or below one.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub root_folders: Vec<String>,
    /// Governed volumes, by the key the capacity page shows.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub disks: Vec<String>,
    /// Radarr/Sonarr tag labels.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Plex library section ids.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub plex_sections: Vec<u32>,
    /// Seerr display names of whoever requested it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub requesters: Vec<String>,
    /// Theme names ([`crate::themes`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub themes: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub genres: Vec<String>,
    /// Part of the *arr quality name: `2160p` matches `Bluray-2160p`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub qualities: Vec<String>,
    #[serde(skip_serializing_if = "Range::is_any")]
    pub size_gib: Range,
    /// Days since it was added.
    #[serde(skip_serializing_if = "Range::is_any")]
    pub age_days: Range,
    /// Anyone has played it (`false`: never played).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub played: Option<bool>,
    /// Days since its last play; a never-played item is outside every range.
    #[serde(skip_serializing_if = "Range::is_any")]
    pub last_played_days: Range,
    /// The model's probability someone plays it, 0..1.
    #[serde(skip_serializing_if = "Range::is_any")]
    pub p_watch: Range,
}

/// Up to this many entries per list; one rule is not a library index.
const MAX_ENTRIES: usize = 100;

impl Scope {
    pub fn is_everything(&self) -> bool {
        *self == Self::default()
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        let lists: [(&str, &[String]); 8] = [
            ("ids", &self.ids),
            ("root_folders", &self.root_folders),
            ("disks", &self.disks),
            ("tags", &self.tags),
            ("requesters", &self.requesters),
            ("themes", &self.themes),
            ("genres", &self.genres),
            ("qualities", &self.qualities),
        ];
        for (field, entries) in lists {
            if entries.len() > MAX_ENTRIES || entries.iter().any(|entry| entry.trim().is_empty()) {
                return Err(format!("{field} takes up to {MAX_ENTRIES} non-blank entries"));
            }
        }
        if self.plex_sections.len() > MAX_ENTRIES {
            return Err(format!("plex_sections takes up to {MAX_ENTRIES} entries"));
        }
        self.size_gib.validate("size_gib", 1_000_000.0)?;
        self.age_days.validate("age_days", 36_500.0)?;
        self.last_played_days.validate("last_played_days", 36_500.0)?;
        self.p_watch.validate("p_watch", 1.0)
    }

    /// Whether every condition holds for this item.
    pub(super) fn test(&self, candidate: &MediaCandidate, facts: &Facts) -> Truth {
        let played = facts.played;
        [
            self.kind.map_or(Truth::Yes, |kind| Truth::of(facts.kind.map(|known| known == kind))),
            listed(&self.ids, Some(self.ids.contains(&candidate.id))),
            listed(&self.root_folders, facts.path.as_deref().map(|path| self.root_folders.iter().any(|root| under(path, root)))),
            listed(&self.disks, candidate.volume.as_deref().map(|volume| self.disks.iter().any(|disk| disk == volume))),
            listed(&self.tags, facts.tags.as_deref().map(|tags| any_named(&self.tags, tags))),
            listed_ids(&self.plex_sections, facts.plex_section),
            listed(
                &self.requesters,
                facts.requests.as_deref().map(|requests| requests.iter().any(|request| named(&self.requesters, &request.requester))),
            ),
            listed(&self.themes, facts.theme.as_deref().map(|theme| named(&self.themes, theme))),
            listed(&self.genres, facts.genres.as_deref().filter(|genres| !genres.is_empty()).map(|genres| any_named(&self.genres, genres))),
            listed(&self.qualities, facts.quality.as_deref().map(|quality| self.qualities.iter().any(|part| contains(quality, part)))),
            ranged(&self.size_gib, Some(candidate.size_bytes as f64 / GIB)),
            ranged(&self.age_days, Some(f64::from(candidate.age_days))),
            self.played.map_or(Truth::Yes, |wanted| Truth::of(played.map(|played| played == wanted))),
            match played {
                _ if self.last_played_days.is_any() => Truth::Yes,
                Some(false) => Truth::No,
                _ => ranged(&self.last_played_days, facts.last_played_days.map(f64::from)),
            },
            ranged(&self.p_watch, Some(candidate.regret.p_watch)),
        ]
        .into_iter()
        .fold(Truth::Yes, Truth::and)
    }
}

/// Whether `event` happened fewer than `days` ago. Requests count only those
/// by the scope's requesters when it names any.
pub(super) fn within(event: Event, days: u32, scope: &Scope, candidate: &MediaCandidate, facts: &Facts, now: u64) -> Truth {
    let window = f64::from(days);
    match event {
        Event::Added => Truth::of(Some(f64::from(candidate.age_days) < window)),
        Event::LastPlayed => match facts.played {
            Some(false) => Truth::No,
            _ => Truth::of(facts.last_played_days.map(|ago| f64::from(ago) < window)),
        },
        Event::Requested => {
            let Some(requests) = facts.requests.as_deref() else { return Truth::Unknown };
            let mut truth = Truth::No;
            for request in requests.iter().filter(|request| scope.requesters.is_empty() || named(&scope.requesters, &request.requester)) {
                match request.at {
                    Some(at) if (now.saturating_sub(at) as f64) < window * DAY => return Truth::Yes,
                    Some(_) => {}
                    None => truth = Truth::Unknown,
                }
            }
            truth
        }
    }
}

fn listed<T>(wanted: &[T], hit: Option<bool>) -> Truth {
    if wanted.is_empty() {
        Truth::Yes
    } else {
        Truth::of(hit)
    }
}

fn listed_ids(wanted: &[u32], have: Option<u32>) -> Truth {
    listed(wanted, have.map(|id| wanted.contains(&id)))
}

fn ranged(range: &Range, value: Option<f64>) -> Truth {
    if range.is_any() {
        Truth::Yes
    } else {
        Truth::of(value.map(|value| range.holds(value)))
    }
}

fn named(wanted: &[String], name: &str) -> bool {
    wanted.iter().any(|entry| entry.trim().eq_ignore_ascii_case(name.trim()))
}

fn any_named(wanted: &[String], names: &[String]) -> bool {
    names.iter().any(|name| named(wanted, name))
}

fn contains(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.trim().to_lowercase())
}

/// `path` is `root` or inside it, by whole path components.
fn under(path: &str, root: &str) -> bool {
    let (path, root) = (path.trim_end_matches('/'), root.trim().trim_end_matches('/'));
    path.strip_prefix(root).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}
