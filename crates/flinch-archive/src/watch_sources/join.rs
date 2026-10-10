//! Join a source's plays to the library by catalogue id and turn them into
//! evidence.
//!
//! A movie joins by its TMDB or IMDb id, an episode by its show's TVDB, TMDB
//! or IMDb id plus its season number; an id two targets share joins neither,
//! and a play whose ids name two different targets joins none. An episode
//! numbered past the season's episode count does not join: the source orders
//! that show's seasons differently from Sonarr, and the play would land on the
//! wrong season.

use super::{Played, SourceKind, SourcePlay, SourceRead};
use crate::card::LibraryKind;
use crate::fit::plays::Play;
use crate::ids::ExternalIds;
use crate::jellyfin::CardPlays;
use crate::plex::season::{EpisodePlays, UNVERIFIED};
use crate::plex::WatchTarget;
use crate::tautulli::{absence_is_evidence, MAX_SILENCE_SECS};
use crate::watch::{WatchEntry, WatchSource};
use std::collections::{HashMap, HashSet};

/// What one source's read says about the library.
#[derive(Debug, Default)]
pub struct SourceEvidence {
    /// Card id → watch state: played items, and (Tracearr, complete read
    /// only) items nobody played since its record began.
    pub entries: HashMap<String, WatchEntry>,
    /// Card id → its dated plays.
    pub plays: HashMap<String, CardPlays>,
    /// Targets at least one play joined.
    pub joined: usize,
    /// Targets claimed never played.
    pub never_played: usize,
}

/// `kind` decides the provenance; `retention_days` (0 = everything kept)
/// bounds how far back Tracearr's silence counts.
pub fn evidence(targets: &[WatchTarget], read: &SourceRead, kind: SourceKind, retention_days: u32, now: u64) -> SourceEvidence {
    let index = Index::new(targets);
    let mut item: HashMap<usize, Vec<&SourcePlay>> = HashMap::new();
    let mut audience: HashMap<usize, Vec<&SourcePlay>> = HashMap::new();
    for play in &read.plays {
        match &play.played {
            Played::Movie(ids) => {
                if let Some(at) = index.movie(ids) {
                    item.entry(at).or_default().push(play);
                    audience.entry(at).or_default().push(play);
                }
            }
            Played::Episode { show, season, episode } => {
                if let Some(at) = index.season(show, *season) {
                    let fits = targets[at].episodes_total.is_none_or(|total| episode.is_none_or(|number| number <= total));
                    if fits {
                        item.entry(at).or_default().push(play);
                    }
                }
                for at in index.show(show) {
                    audience.entry(at).or_default().push(play);
                }
            }
        }
    }
    let source = match kind {
        SourceKind::Tracearr => WatchSource::Tracearr,
        SourceKind::Trakt => WatchSource::Trakt,
    };
    let mut out = SourceEvidence { joined: item.len(), ..SourceEvidence::default() };
    for (&at, plays) in &item {
        let target = &targets[at];
        out.entries.insert(target.id.clone(), entry(target, plays, source));
    }
    for at in item.keys().chain(audience.keys()).copied().collect::<HashSet<usize>>() {
        let card = CardPlays { item: dated(item.get(&at)), audience: dated(audience.get(&at)) };
        out.plays.insert(targets[at].id.clone(), card);
    }
    if kind == SourceKind::Tracearr && read.complete {
        if let Some(start) = coverage_start(&read.epochs, retention_days, now) {
            for target in targets.iter().enumerate().filter(|(at, _)| !item.contains_key(at)).map(|(_, target)| target) {
                let added = target.added_epoch.filter(|added| absence_is_evidence(*added, start, now));
                if added.is_some() && target.on_disk && index.knows(target) {
                    let entry =
                        WatchEntry { id: target.id.clone(), last_watched_epoch: None, progress: 0.0, source: WatchSource::TracearrAbsence };
                    out.entries.insert(target.id.clone(), entry);
                    out.never_played += 1;
                }
            }
        }
    }
    out
}

fn dated(plays: Option<&Vec<&SourcePlay>>) -> Vec<Play> {
    let mut out: Vec<Play> = plays
        .into_iter()
        .flatten()
        .map(|play| Play {
            epoch: play.epoch,
            episode: match play.played {
                Played::Episode { episode, .. } => episode,
                Played::Movie(_) => None,
            },
            viewer: Some(play.viewer.clone()),
            fraction: play.fraction,
        })
        .collect();
    out.sort_by_key(|play| play.epoch);
    out
}

fn entry(target: &WatchTarget, plays: &[&SourcePlay], source: WatchSource) -> WatchEntry {
    let finished = |play: &SourcePlay| play.fraction >= crate::tautulli::FINISHED_FRACTION;
    let progress = match target.kind {
        LibraryKind::Movie if plays.iter().any(|play| finished(play)) => 1.0,
        // Touched, unfinished: never "never played" and never complete.
        LibraryKind::Movie => plays.iter().map(|play| play.fraction).fold(0.01f32, f32::max).min(UNVERIFIED),
        LibraryKind::Season => {
            let mut episodes = EpisodePlays::default();
            for play in plays {
                if let Played::Episode { episode, .. } = play.played {
                    episodes.record(episode, finished(play));
                }
            }
            episodes.progress(target)
        }
    };
    WatchEntry { id: target.id.clone(), last_watched_epoch: plays.iter().map(|play| play.epoch).max(), progress, source }
}

/// Where the record's latest unbroken run began (no gap over
/// [`MAX_SILENCE_SECS`]), moved forward to the retention window; `None` when
/// the source is not recording now, so its silence proves nothing.
pub(super) fn coverage_start(epochs: &[u64], retention_days: u32, now: u64) -> Option<u64> {
    let mut epochs = epochs.to_vec();
    epochs.sort_unstable();
    let end = *epochs.last()?;
    if now.saturating_sub(end) > MAX_SILENCE_SECS {
        return None;
    }
    let mut start = end;
    for epoch in epochs.iter().rev().skip(1) {
        if start - epoch > MAX_SILENCE_SECS {
            break;
        }
        start = *epoch;
    }
    match retention_days {
        0 => Some(start),
        days => Some(start.max(now.saturating_sub(u64::from(days) * 86_400))),
    }
}

fn keys(ids: &ExternalIds) -> Vec<String> {
    let mut keys = Vec::new();
    keys.extend(ids.tvdb.map(|id| format!("tvdb:{id}")));
    keys.extend(ids.tmdb.map(|id| format!("tmdb:{id}")));
    keys.extend(ids.imdb.as_deref().map(str::trim).filter(|id| !id.is_empty()).map(|id| format!("imdb:{}", id.to_ascii_lowercase())));
    keys
}

/// Catalogue ids → target index; `None` marks an id two targets share.
struct Index {
    movies: HashMap<String, Option<usize>>,
    seasons: HashMap<(String, u32), Option<usize>>,
    shows: HashMap<String, HashSet<usize>>,
}

impl Index {
    fn new(targets: &[WatchTarget]) -> Self {
        let mut index = Self { movies: HashMap::new(), seasons: HashMap::new(), shows: HashMap::new() };
        let claim = |slot: &mut Option<usize>, at: usize| {
            if *slot != Some(at) {
                *slot = None;
            }
        };
        for (at, target) in targets.iter().enumerate() {
            for key in keys(&target.external) {
                match (target.kind, target.season_index) {
                    (LibraryKind::Movie, _) => {
                        index.movies.entry(key).and_modify(|slot| claim(slot, at)).or_insert(Some(at));
                    }
                    (LibraryKind::Season, Some(season)) => {
                        index.shows.entry(key.clone()).or_default().insert(at);
                        index.seasons.entry((key, season)).and_modify(|slot| claim(slot, at)).or_insert(Some(at));
                    }
                    (LibraryKind::Season, None) => {}
                }
            }
        }
        index
    }

    /// The one target every known id of the play names, or none.
    fn unique(found: impl Iterator<Item = Option<usize>>) -> Option<usize> {
        let mut only = None;
        for slot in found {
            let at = slot?;
            if only.is_some_and(|seen| seen != at) {
                return None;
            }
            only = Some(at);
        }
        only
    }

    fn movie(&self, ids: &ExternalIds) -> Option<usize> {
        Self::unique(keys(ids).into_iter().filter_map(|key| self.movies.get(&key).copied()))
    }

    fn season(&self, show: &ExternalIds, season: u32) -> Option<usize> {
        Self::unique(keys(show).into_iter().filter_map(|key| self.seasons.get(&(key, season)).copied()))
    }

    fn show(&self, show: &ExternalIds) -> HashSet<usize> {
        keys(show).iter().filter_map(|key| self.shows.get(key)).flatten().copied().collect()
    }

    /// A target a play could have joined: one with a catalogue id no other
    /// target shares. Only such a target's silence means anything.
    fn knows(&self, target: &WatchTarget) -> bool {
        let ids = keys(&target.external);
        !ids.is_empty()
            && ids.into_iter().all(|key| match (target.kind, target.season_index) {
                (LibraryKind::Movie, _) => self.movies.get(&key).copied().flatten().is_some(),
                (LibraryKind::Season, Some(season)) => self.seasons.get(&(key, season)).copied().flatten().is_some(),
                (LibraryKind::Season, None) => false,
            })
    }
}
