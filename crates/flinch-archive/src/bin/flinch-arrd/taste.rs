//! The taste signal in the daemon: every card nobody has played gets the
//! household's play rate for its genres, from the rates the daily refit
//! counted and stored in `fit.json`. No network and no model server: the
//! rates are this household's own closed outcomes.

use super::model::Scoring;
use flinch_archive::arr::{ArrMovie, ArrSeries};
use flinch_archive::fit::adopt;
use flinch_archive::score::HouseholdContext;
use flinch_archive::taste;
use flinch_archive::ArchiveCard;
use std::collections::HashMap;
use std::path::Path;

/// Set `ctx.taste` for every card nobody has played: not in its card's watch
/// state and not in the play log. One log line says how many.
pub(super) fn fill(state_dir: &Path, inputs: &Scoring<'_>, contexts: &mut [HouseholdContext]) {
    let rates = adopt::read_status(state_dir).map(|status| status.taste).unwrap_or_default();
    let genres = Genres::of(inputs.movies, inputs.series);
    let mut tasted = 0;
    for (card, ctx) in inputs.cards.iter().zip(contexts.iter_mut()) {
        let played = inputs.play_log.item_plays(inputs.join(card)).iter().any(|play| play.epoch < inputs.now);
        ctx.taste = if played { None } else { taste::taste(&rates, card, genres.of_card(card)) };
        tasted += usize::from(ctx.taste.is_some());
    }
    println!(
        "[flinch-arrd] taste: {tasted} unplayed item(s) scored by genre, from {} closed outcome(s) in {} genre(s)",
        rates.overall.total,
        rates.genres.len()
    );
}

/// Genre names by card: a movie's own, a season's show's.
struct Genres<'a> {
    movies: HashMap<String, &'a [String]>,
    shows: HashMap<&'a str, &'a [String]>,
}

impl<'a> Genres<'a> {
    fn of(movies: &'a [ArrMovie], series: &'a [ArrSeries]) -> Self {
        Self {
            movies: movies.iter().map(|movie| (format!("radarr-{}", movie.id), movie.genres.as_slice())).collect(),
            shows: series.iter().map(|show| (show.title.as_str(), show.genres.as_slice())).collect(),
        }
    }

    fn of_card(&self, card: &ArchiveCard) -> &'a [String] {
        let genres = match card.show_title.as_deref() {
            Some(show) => self.shows.get(show),
            None => self.movies.get(&card.id),
        };
        genres.copied().unwrap_or(&[])
    }
}
