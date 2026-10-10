//! Rolling retention for shows: whether a season is among the newest of a
//! continuing show, or is its first. Seasons are ranked among the show's
//! seasons on disk, specials (season 0) aside: a season Sonarr lists but
//! holds no file of does not use up a place.
//!
//! A movie and a special are never matched; a show whose status Sonarr did
//! not give is unknown, so a rule keeps it (missing facts keep).

use super::scope::Truth;
use super::{Facts, Kind};

/// The season is among the newest `seasons` of a continuing show.
pub(super) fn latest(seasons: u32, facts: &Facts) -> Truth {
    regular_season(facts).and(truth(facts.continuing)).and(truth(facts.newest_rank.map(|rank| rank <= seasons)))
}

/// The season is its show's first regular one.
pub(super) fn first(facts: &Facts) -> Truth {
    regular_season(facts).and(truth(facts.first_season))
}

/// A season that is not a special; unknown without the item's kind.
fn regular_season(facts: &Facts) -> Truth {
    match facts.kind {
        Some(Kind::Season) if facts.season == Some(0) => Truth::No,
        Some(Kind::Season) => Truth::Yes,
        Some(Kind::Movie) => Truth::No,
        None => Truth::Unknown,
    }
}

fn truth(known: Option<bool>) -> Truth {
    match known {
        Some(true) => Truth::Yes,
        Some(false) => Truth::No,
        None => Truth::Unknown,
    }
}

#[cfg(test)]
mod tests;
