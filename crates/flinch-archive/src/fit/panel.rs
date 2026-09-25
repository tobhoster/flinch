//! The panel: every cut date × every item that was on disk at that cut, asked
//! exactly as the daemon would have asked it then.
//!
//! Each row carries the card and household context inference would have scored
//! at the cut, rebuilt from plays *before* the cut only, and a label from plays
//! in the horizon *after* it. Play state, recency, rewatches, viewers, sibling
//! engagement and play-log provenance are exact. A few facts have no history to
//! replay and use their current value — the documented approximation: size,
//! the newest-season flag, the sibling count, and provenance from an item-state
//! source (a live Plex query or an export). They barely move over a season, and
//! pretending to know their history would be inventing data. Series status is
//! only counted once the final episode had aired by the cut, so it never leaks.
//! Taste is a post-pass over the finished panel: each row reads the household's
//! genre play-rates counted from rows whose outcome had closed by its own cut.

use super::plays::PlayEvidence;
use super::FitItem;
use crate::card::{ArchiveCard, LibraryKind, SeasonState};
use crate::presence;
use crate::score::{self, HouseholdContext};
use crate::taste::{self, GenreRates, ItemGenres};
use crate::tautulli;
use crate::watch::WatchSource;
use std::collections::HashMap;

/// Which questions the panel asks.
#[derive(Debug, Clone, Copy)]
pub struct PanelSpec<'a> {
    /// The panel's "today", unix seconds. Pin it to reproduce a panel later.
    pub now: u64,
    /// Cut dates, in days before `now`.
    pub cuts_days: &'a [f32],
    pub horizon_days: f32,
    /// Oldest stream Tautulli holds: its silence only counts after this.
    pub tautulli_coverage_start: Option<u64>,
}

/// One panel row: an "as of" question and its observed answer.
#[derive(Debug, Clone)]
pub struct Example {
    pub item_id: String,
    /// When the question was asked, in days before the panel's `now`.
    pub cut_days: f32,
    pub cut_unix: u64,
    /// What inference would have scored at the cut.
    pub card: ArchiveCard,
    pub ctx: HouseholdContext,
    /// `score::features(card, ctx)` by name: what training fits weights over.
    pub values: HashMap<&'static str, f32>,
    /// 1.0 = nothing played it during the horizon, so reclaiming would have been safe.
    pub label: f32,
}

/// Build the panel: every cut date × every item that was on disk at that cut.
pub fn build_dataset(items: &[FitItem], spec: &PanelSpec<'_>) -> Vec<Example> {
    let horizon = (spec.horizon_days * 86_400.0) as u64;
    let shows = by_show(items);
    let mut examples = Vec::new();
    for days in spec.cuts_days {
        let cut = spec.now.saturating_sub((*days * 86_400.0) as u64);
        // Only fully observed outcomes are labels. A window that runs past `now`
        // has not happened yet, and counting it as "nothing played it" would
        // teach the model that recent items are safe.
        if cut + horizon > spec.now {
            continue;
        }
        for item in items {
            // Only items that existed at the cut can be judged at it.
            let Some(arrival) = arrival_by(item, cut, spec.now) else {
                continue;
            };
            let show = item.show_title.as_deref().and_then(|title| shows.get(title)).map_or(&[][..], Vec::as_slice);
            let card = card_as_of(item, cut, arrival);
            let ctx = context_as_of(item, show, cut, spec);
            let values = score::features(&card, ctx).into_iter().map(|feature| (feature.name, feature.value)).collect();
            examples.push(Example {
                item_id: item.id.clone(),
                cut_days: *days,
                cut_unix: cut,
                card,
                ctx,
                values,
                // The guard's question: did nothing play it during the horizon?
                label: if item.played_between(cut, cut + horizon) { 0.0 } else { 1.0 },
            });
        }
    }
    attach_taste(&mut examples, items, horizon);
    examples
}

/// Set each row's taste from the genre rates as of its cut ([`taste::taste`]),
/// and add the feature it implies. Rates are counted once per distinct cut.
fn attach_taste(examples: &mut [Example], items: &[FitItem], horizon_secs: u64) {
    let genres = ItemGenres::of(items);
    let mut cuts: Vec<u64> = examples.iter().map(|row| row.cut_unix).collect();
    cuts.sort_unstable();
    cuts.dedup();
    let rates: HashMap<u64, GenreRates> =
        cuts.into_iter().map(|cut| (cut, GenreRates::as_of(examples, &genres, horizon_secs, cut))).collect();
    for row in examples.iter_mut() {
        row.ctx.taste = rates.get(&row.cut_unix).and_then(|rates| taste::taste(rates, &row.card, genres.of_id(&row.item_id)));
        if row.ctx.taste.is_some() {
            row.values = score::features(&row.card, row.ctx).into_iter().map(|feature| (feature.name, feature.value)).collect();
        }
    }
}

fn by_show(items: &[FitItem]) -> HashMap<&str, Vec<&FitItem>> {
    let mut shows: HashMap<&str, Vec<&FitItem>> = HashMap::new();
    for item in items {
        if let Some(show) = item.show_title.as_deref() {
            shows.entry(show).or_default().push(item);
        }
    }
    shows
}

/// When the item that was on disk at `cut` arrived there, or `None` if it was
/// not on disk then.
///
/// With presence history, it was on disk iff a span covers the cut, and it
/// arrived when that span began: the start the daemon dates its live card
/// from, so both compute the same dwell for the same date.
///
/// Without it: its recorded arrival, or an earlier play. A library migration
/// re-imports files and resets arrival dates (seen live: 12 of 41 items played
/// before their recorded arrival), which silently dropped their whole history
/// from the panel. A play before the cut proves the item was there, and uses
/// nothing from after it.
fn arrival_by(item: &FitItem, cut: u64, now: u64) -> Option<u64> {
    if !item.on_disk.is_empty() {
        return presence::covering(&item.on_disk, cut).map(|span| span.from);
    }
    let recorded = item.added_epoch(now);
    let first_play = item.plays.iter().map(|play| play.epoch).filter(|epoch| *epoch < cut).min();
    let arrival = first_play.map_or(recorded, |played| played.min(recorded));
    (arrival <= cut).then_some(arrival)
}

/// An item's card reconstructed as of `cut`, for an item that had arrived by then.
fn card_as_of(item: &FitItem, cut: u64, arrival: u64) -> ArchiveCard {
    let played = item.plays_before(cut) > 0;
    let episodes = item.episodes_played_before(cut);
    let season_state = match item.kind {
        LibraryKind::Season => Some(match item.episodes_total.filter(|total| *total > 0) {
            Some(total) if episodes >= total => SeasonState::Completed,
            _ if played => SeasonState::Partial,
            _ => SeasonState::Empty,
        }),
        LibraryKind::Movie => None,
    };
    ArchiveCard {
        id: item.id.clone(),
        title: item.title.clone(),
        kind: item.kind,
        size_bytes: item.size_bytes,
        // Dwell as the daemon saw it at the cut, from the arrival the cut can
        // prove — not today's age, and not a migration's reset date.
        added_days_ago: cut.saturating_sub(arrival) as f32 / 86_400.0,
        last_watched_days: item.last_play_before(cut).map(|epoch| cut.saturating_sub(epoch) as f32 / 86_400.0),
        in_keep_collection: false,
        is_favorite: false,
        duplicate_count: 0,
        series_type: None,
        season_state,
        season_index: item.season_index,
        is_newest_season: Some(item.is_newest_season),
        episodes_total: item.episodes_total,
        episodes_watched: Some(episodes),
        is_watched: match item.kind {
            // Watched means finished, as the daemon reads Tautulli progress; a
            // stream that stopped early still sets recency above.
            LibraryKind::Movie => Some(item.finished_before(cut)),
            LibraryKind::Season => None,
        },
        rewatch_score: None,
        movie_year: None,
        show_title: item.show_title.clone(),
    }
}

/// Household context as of `cut`: what the *other* seasons of this show were
/// doing by then, what the household's plays say, and where the evidence came
/// from. Taste is added once the whole panel exists ([`attach_taste`]).
fn context_as_of(item: &FitItem, show: &[&FitItem], cut: u64, spec: &PanelSpec<'_>) -> HouseholdContext {
    let others = show.iter().filter(|other| other.id != item.id);
    let sibling_season_played = others.clone().any(|other| other.plays_before(cut) > 0);
    let sibling_season_completed =
        others.into_iter().any(|other| other.episodes_total.is_some_and(|total| total > 0 && other.episodes_played_before(cut) >= total));
    // The same reduction the daemon applies to its play log, at the cut.
    let evidence = PlayEvidence::as_of(item.kind, &item.plays, &item.audience_plays, cut);
    HouseholdContext {
        sibling_season_played,
        sibling_season_completed,
        siblings: show.len() as u32,
        watch_source: source_as_of(item, cut, spec),
        rewatched: evidence.rewatched,
        viewers: evidence.viewers,
        series_ended: score::series_ended_as_of(item.series_status.as_deref(), item.last_aired_epoch, cut),
        taste: None,
    }
}

/// Watch-evidence provenance as of `cut`.
///
/// Item-state sources describe the item as a whole and cannot be replayed, so
/// they keep their current value. Play-log sources can be replayed exactly: the
/// item had play evidence at the cut iff it had a play before it, and Tautulli's
/// silence counted only for a GUID-resolved item with no stream of any kind
/// before the cut, where [`tautulli::absence_is_evidence`] held at the cut —
/// the conditions under which the daemon claims it.
fn source_as_of(item: &FitItem, cut: u64, spec: &PanelSpec<'_>) -> Option<WatchSource> {
    let played = item.plays_before(cut) > 0;
    match item.watch_source {
        Some(source @ (WatchSource::Plex | WatchSource::PlexShow | WatchSource::Export)) => Some(source),
        Some(WatchSource::Tautulli) if played => Some(WatchSource::Tautulli),
        Some(WatchSource::Tautulli | WatchSource::TautulliAbsence | WatchSource::PlexHistory) | None => {
            let silent_while_watched = item.guid_resolved
                && spec.tautulli_coverage_start.is_some_and(|start| tautulli::absence_is_evidence(item.added_epoch(spec.now), start, cut));
            if played {
                Some(WatchSource::PlexHistory)
            } else if silent_while_watched {
                Some(WatchSource::TautulliAbsence)
            } else {
                None
            }
        }
    }
}
