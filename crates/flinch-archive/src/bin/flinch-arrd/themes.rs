//! Themes, once a cycle: the taste vectors are clustered again when one of
//! them changed or a day has passed (`themes.json`), and the library's bytes
//! and plays are read by theme, for the storage view and quality advice.
//!
//! Nothing here fails the cycle or makes anything more deletable: an
//! unwritable `themes.json` is recomputed next cycle, and without vectors
//! there are no themes, no cold ones, and no `themes` block in the status.

use super::{read_state, state_dir, write_state};
use flinch_archive::arr::{ArrMovie, ArrSeries};
use flinch_archive::embedding::VectorStore;
use flinch_archive::plan::candidates::Plays;
use flinch_archive::themes::{self, ColdTheme, Holding, Member, Themes, ThemesStatus};
use flinch_archive::{ArchiveCard, EvictionPlan};
use std::collections::{HashMap, HashSet};

pub(super) struct Themed {
    pub(super) themes: Themes,
    /// Card id → its theme, for cards in a seldom-played one.
    pub(super) cold: HashMap<String, ColdTheme>,
}

/// The themes of every movie and show with a vector, recomputed when stale,
/// and the cards that sit in a cold one.
pub(super) fn refresh(
    vectors: &VectorStore,
    (movies, series): (&[ArrMovie], &[ArrSeries]),
    cards: &[ArchiveCard],
    plays: &HashMap<String, Plays<'_>>,
    now: u64,
) -> Themed {
    let subjects: Vec<(String, &[String])> = movies
        .iter()
        .map(|movie| (movie.card_id(), movie.genres.as_slice()))
        .chain(series.iter().map(|show| (show.subject(), show.genres.as_slice())))
        .collect();
    let members: Vec<Member> =
        subjects.iter().filter_map(|(subject, genres)| Some(Member { subject, vector: vectors.vector(subject)?, genres })).collect();
    let path = state_dir().join(themes::THEMES_FILE);
    let mut current: Themes = read_state(&path);
    if !current.is_current(themes::fingerprint(&members), now) {
        current = Themes::compute(&members, now);
        write_state(&path, &current);
        println!(
            "[flinch-arrd] themes: {} theme(s) over {} of {} titles with a vector",
            current.names.len(),
            current.assignments.len(),
            members.len(),
        );
    }
    let status = themes::status(&current, &holdings(cards, plays, &HashSet::new()), now);
    let cold = themes::cold_cards(&current, &status, cards.iter().map(|card| card.id.as_str()));
    Themed { themes: current, cold }
}

/// Storage by theme with this cycle's planned evictions; `None` without themes.
pub(super) fn status(
    themed: &Themed,
    cards: &[ArchiveCard],
    plays: &HashMap<String, Plays<'_>>,
    plan: &EvictionPlan,
    now: u64,
) -> Option<ThemesStatus> {
    if themed.themes.names.is_empty() {
        return None;
    }
    let planned: HashSet<&str> = plan.items.iter().map(|item| item.id.as_str()).collect();
    Some(themes::status(&themed.themes, &holdings(cards, plays, &planned), now))
}

/// Every card as a holding: its bytes, the newest play of it or (a season)
/// of its show from the play log, and whether `planned` evicts it.
fn holdings<'a>(cards: &'a [ArchiveCard], plays: &HashMap<String, Plays<'_>>, planned: &HashSet<&str>) -> Vec<Holding<'a>> {
    cards
        .iter()
        .map(|card| Holding {
            card_id: &card.id,
            bytes: card.size_bytes,
            last_play: plays.get(&card.id).and_then(|(_, audience)| audience.iter().map(|play| play.epoch).max()),
            planned: planned.contains(card.id.as_str()),
        })
        .collect()
}
