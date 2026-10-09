//! Every library item as a [`MediaCandidate`]: its regret from the household's
//! plays, claims and re-download difficulty, and the one rule (if any) that
//! keeps it out of the solver.

use super::{Exclusion, MediaCandidate, Pin, PlannerConfig};
use crate::arr::{ArrMovie, ArrSeries};
use crate::card::{ArchiveCard, LibraryKind};
use crate::fit::plays::Play;
use crate::plan::knapsack::Sequence;
use crate::regret::{self, Claim, HazardModel, PlayHistory, Reacquisition, Regret, WatchFeatures};
use crate::signals::{MediaRef, Signals};
use crate::taste::Reading;
use crate::watch::{WatchEntry, COMPLETE};
use std::collections::{BTreeMap, HashMap, HashSet};

/// One item's plays: its own, then its show's (a movie's own), oldest first.
pub type Plays<'a> = (Vec<&'a Play>, Vec<&'a Play>);

/// What the builder reads, all keyed by card id.
#[derive(Clone, Copy)]
pub struct Library<'a> {
    pub cards: &'a [ArchiveCard],
    pub movies: &'a [ArrMovie],
    pub series: &'a [ArrSeries],
    pub watch: &'a HashMap<String, WatchEntry>,
    pub plays: &'a HashMap<String, Plays<'a>>,
    /// Card id → governed volume.
    pub located: &'a HashMap<String, String>,
    /// Cards Plex resolved by id: the only ones Maintainerr can act on.
    pub in_plex: &'a HashSet<String>,
    /// Cards already in a FLINCH collection.
    pub handed: &'a HashSet<String>,
    pub signals: &'a Signals,
    /// Card id → the household's taste for it and the titles it rests on
    /// ([`crate::taste`]); absent when no embedding or no outcome speaks for it.
    pub taste: &'a HashMap<String, Reading>,
    /// Card id → its theme, for cards in a theme the household seldom plays
    /// from ([`crate::themes`]): quality advice only, never regret.
    pub cold_themes: &'a HashMap<String, crate::themes::ColdTheme>,
    /// Why never-played items stay this run; `None` lets them compete.
    pub never_played: Option<Exclusion>,
    pub now: u64,
}

pub fn build(library: &Library, config: &PlannerConfig, model: &HazardModel) -> Vec<MediaCandidate> {
    let claims = claims(library, config);
    let none: Plays = (Vec::new(), Vec::new());
    let titles = if library.taste.is_empty() { HashMap::new() } else { titles(library) };
    let uhd: HashSet<String> = library
        .movies
        .iter()
        .filter(|movie| movie.quality().is_some_and(|quality| quality.contains("2160p")))
        .map(|movie| movie.card_id())
        .collect();
    library
        .cards
        .iter()
        .map(|card| {
            let (item, audience) = library.plays.get(&card.id).unwrap_or(&none);
            let season = card.kind == LibraryKind::Season;
            let watch = library.watch.get(&card.id);
            let features = WatchFeatures::read(
                &PlayHistory {
                    item,
                    audience,
                    episodes_total: card.episodes_total.filter(|_| season),
                    episodes_on_disk: card.episodes_on_disk.as_deref().filter(|_| season),
                    last_watched_days: card.last_watched_days,
                    added_days_ago: card.added_days_ago,
                    marked_complete: watch.is_some_and(|entry| entry.progress >= COMPLETE),
                },
                library.now,
            )
            .with_taste(library.taste.get(&card.id).map(|reading| reading.taste));
            // Taste sits only on a never-played title; name the titles it rests on.
            let like = library
                .taste
                .get(&card.id)
                .filter(|_| features.taste != 0.0)
                .and_then(|reading| reading.like.as_ref())
                .and_then(|like| like.note(|subject| titles.get(subject).copied()));
            let release = library.signals.releases.get(&card.id);
            let friction = Reacquisition {
                size_bytes: card.size_bytes,
                seeders: release.and_then(|release| release.seeders),
                usenet_out_of_retention: release.and_then(|release| release.usenet_out_of_retention).unwrap_or(false),
            }
            .friction();
            let household = regret::household(claims.get(card.id.as_str()).into_iter().flatten().copied());
            let regret = Regret::new(model.p_watch(&features), friction, household);
            let played = card.last_watched_days.is_some() || watch.is_some_and(|entry| entry.progress > 0.0);
            let pin = if card.is_favorite {
                Some(Pin::Favorite)
            } else if card.in_keep_collection {
                Some(Pin::KeepList)
            } else {
                None
            };
            let exclusion = pin.map(Exclusion::Pinned).or_else(|| {
                if !library.in_plex.contains(&card.id) {
                    Some(Exclusion::NotInPlex)
                } else if watch.is_none() {
                    Some(Exclusion::NoWatchEvidence)
                } else if !played {
                    library.never_played
                } else {
                    None
                }
            });
            MediaCandidate {
                id: card.id.clone(),
                title: card.title.clone(),
                size_bytes: card.size_bytes,
                volume: library.located.get(&card.id).cloned(),
                reason: regret::describe(&features, &regret, like.as_deref()),
                regret,
                age_days: card.added_days_ago,
                exclusion,
                sequence: season.then(|| sequence(card, played)).flatten(),
                handed: library.handed.contains(&card.id),
                announce: watch.is_none_or(|entry| entry.progress < COMPLETE),
                protect: pin.is_some() || features.partway,
                quality: crate::quality::advise(
                    &regret,
                    &crate::quality::Item {
                        size_bytes: card.size_bytes,
                        partway: features.partway,
                        pinned: pin.is_some(),
                        uhd: uhd.contains(&card.id),
                        cold_theme: library.cold_themes.get(&card.id),
                    },
                ),
                eviction_safety: crate::quality::eviction_safety(&regret),
            }
        })
        .collect()
}

/// Candidates from cards alone, as the offline CLI plans: every card on
/// `volume` and resolved in Plex, its watch evidence read from the card
/// itself, with no play log and no external signals.
pub fn offline(
    cards: &[ArchiveCard],
    volume: &str,
    never_played: Option<Exclusion>,
    now: u64,
    config: &PlannerConfig,
) -> Vec<MediaCandidate> {
    let watch: HashMap<String, WatchEntry> = cards
        .iter()
        .map(|card| {
            let progress = match (card.is_watched, card.episodes_watched, card.episodes_total) {
                (Some(true), _, _) => 1.0,
                (_, Some(watched), Some(total)) if total > 0 => watched as f32 / total as f32,
                _ => 0.0,
            };
            let last_watched_epoch = card.last_watched_days.map(|days| now.saturating_sub((f64::from(days) * 86_400.0) as u64));
            let entry = WatchEntry { id: card.id.clone(), last_watched_epoch, progress, source: Default::default() };
            (card.id.clone(), entry)
        })
        .collect();
    let located: HashMap<String, String> = cards.iter().map(|card| (card.id.clone(), volume.to_string())).collect();
    let in_plex: HashSet<String> = cards.iter().map(|card| card.id.clone()).collect();
    let library = Library {
        cards,
        movies: &[],
        series: &[],
        watch: &watch,
        plays: &HashMap::new(),
        located: &located,
        in_plex: &in_plex,
        handed: &HashSet::new(),
        signals: &Signals::default(),
        taste: &HashMap::new(),
        cold_themes: &HashMap::new(),
        never_played,
        now,
    };
    build(&library, config, &HazardModel::default())
}

/// A season's place in its show: the show is the card id before `-s<n>`.
fn sequence(card: &ArchiveCard, played: bool) -> Option<Sequence> {
    let (show, _) = card.id.rsplit_once("-s")?;
    Some(Sequence { group: show.to_string(), index: card.season_index?, played })
}

/// Subject ([`crate::embedding::subject_of`]) → title, for naming taste
/// neighbours: the *arr's title, else the card's (a season's show title).
fn titles<'a>(library: &Library<'a>) -> HashMap<String, &'a str> {
    let mut titles: HashMap<String, &'a str> = library
        .cards
        .iter()
        .map(|card| (crate::embedding::subject_of(&card.id).to_string(), card.show_title.as_deref().unwrap_or(&card.title)))
        .collect();
    titles.extend(library.movies.iter().map(|movie| (movie.card_id(), movie.title.as_str())));
    titles.extend(library.series.iter().map(|series| (crate::embedding::series_subject(series.id), series.title.as_str())));
    titles
}

/// Per card, each Seerr user's strongest claim: watchlisted, requested, or
/// both, weighted by the operator's per-user weight.
fn claims<'a>(library: &Library<'a>, config: &PlannerConfig) -> HashMap<&'a str, Vec<Claim>> {
    let known: HashSet<&'a str> = library.cards.iter().map(|card| card.id.as_str()).collect();
    // Users are one person whatever the case of their name, as weights are.
    let mut by_user: BTreeMap<(&str, String), (bool, bool)> = BTreeMap::new();
    let requests =
        library.signals.requests.iter().map(|request| (&request.media, request.seasons.as_slice(), request.requester.as_str(), false));
    let watchlists = library.signals.watchlists.iter().map(|entry| (&entry.media, &[][..], entry.user.as_str(), true));
    for (media, seasons, user, watchlisted) in requests.chain(watchlists) {
        for card in cards_of(library, &known, media, seasons) {
            let entry = by_user.entry((card, user.to_lowercase())).or_default();
            if watchlisted {
                entry.0 = true;
            } else {
                entry.1 = true;
            }
        }
    }
    let mut claims: HashMap<&str, Vec<Claim>> = HashMap::new();
    for ((card, user), (watchlisted, requested)) in by_user {
        claims.entry(card).or_default().push(Claim { weight: config.weight(&user), watchlisted, requested });
    }
    claims
}

/// The cards a Seerr media item names: a movie by TMDB id; a show's seasons
/// by TVDB or TMDB id, limited to `seasons` when any are listed.
fn cards_of<'a>(library: &Library<'a>, known: &HashSet<&'a str>, media: &MediaRef, seasons: &[u32]) -> Vec<&'a str> {
    let wanted = |id: String| known.get(id.as_str()).copied();
    match media {
        MediaRef::Movie { tmdb } => library
            .movies
            .iter()
            .filter(|movie| movie.tmdb_id.map(u64::from) == Some(*tmdb))
            .filter_map(|movie| wanted(movie.card_id()))
            .collect(),
        MediaRef::Show { tvdb, tmdb } => library
            .series
            .iter()
            .filter(|show| {
                (tvdb.is_some() && show.tvdb_id.map(u64::from) == *tvdb) || (tmdb.is_some() && show.tmdb_id.map(u64::from) == *tmdb)
            })
            .flat_map(|show| {
                show.seasons
                    .iter()
                    .filter(|season| seasons.is_empty() || seasons.contains(&season.season_number))
                    .filter_map(|season| wanted(show.season_card_id(season.season_number)))
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests;
