//! Taste: how readily someone here plays titles like one nobody has played.
//!
//! The hazard reads an item's own plays, so every download nobody has opened
//! looks alike: "never played, N days on disk". What the household did with
//! the *rest* of the library says more. EmbeddingGemma 2 places every title in
//! one space from its content alone — story, genres, people, studio
//! ([`crate::embedding`]) — and the panel already records, per title, whether
//! it was played in the horizon after each cut, and by whom. Taste is a
//! nearest-neighbour classifier over those outcomes: the played share of the
//! most similar titles, smoothed toward the overall played rate, and reported
//! as a log-odds shift from that rate (0 = no evidence either way).
//!
//! A household is several people, and the household-wide share blurs them: a
//! comedy can sit among horror nobody else touches while the one person who
//! watches comedies plays every one. So taste is read per active viewer, each
//! from their own outcomes against their own baseline, and the household's
//! taste is the warmest of them — someone here would watch it. Only when no
//! active viewer has played enough titles to point anywhere does the
//! household-wide record speak.
//!
//! It is the zero-prior `taste` feature of the hazard ([`crate::regret`]): it
//! moves no decision until the daily held-out fit gives it a weight and that
//! fit beats the priors.
//!
//! Leak-freedom: a panel row asks with the outcomes whose window had closed by
//! its own cut (`cut + horizon <= as_of`), never with its own show's, and with
//! the viewers active before that cut. The daemon asks with the outcomes the
//! adopted fit was made from, which travel with it in `hazard.json`.

use crate::card::ArchiveCard;
use crate::embedding::{subject_of, VectorStore};
use crate::fit::panel::Example;
use crate::fit::plays::{Play, Viewer};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Neighbours that vote.
pub const NEIGHBOURS: usize = 20;
/// Titles kept per query, nearest first: the first [`NEIGHBOURS`] with a closed
/// outcome are almost always among them, at every cut.
const SHORTLIST: usize = 200;
/// Pseudo-neighbours voting the audience's overall rate: a title whose few
/// neighbours were all played is pulled back toward what that audience does
/// with anything.
pub const PRIOR_WEIGHT: f64 = 2.0;
/// Rates are kept off 0 and 1 before they become log-odds.
const RATE_BOUNDS: (f64, f64) = (0.01, 0.99);
/// A viewer is part of the household at a date with a play in the year before
/// it: an account nobody has used for a year no longer says what gets watched.
pub const ACTIVE_SECS: u64 = 365 * 86_400;
/// Distinct titles a viewer must have played among the closed outcomes before
/// their own taste is read: fewer point nowhere in particular.
pub const MIN_VIEWER_TITLES: usize = 5;
/// Neighbours an explanation names.
const NAMED: usize = 2;

/// Closed outcomes: rows, and rows played within their horizon.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcomes {
    pub played: u32,
    pub total: u32,
}

impl Outcomes {
    fn add(&mut self, played: bool) {
        self.played += u32::from(played);
        self.total += 1;
    }

    fn rate(self) -> Option<f64> {
        (self.total > 0).then(|| f64::from(self.played) / f64::from(self.total))
    }
}

/// One audience's closed outcomes — the household's, or one viewer's — overall
/// and per subject (a movie, or a show for all its seasons).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tally {
    pub overall: Outcomes,
    pub subjects: BTreeMap<String, Outcomes>,
}

impl Tally {
    fn add(&mut self, subject: &str, played: bool) {
        self.overall.add(played);
        match self.subjects.get_mut(subject) {
            Some(outcomes) => outcomes.add(played),
            None => {
                let mut outcomes = Outcomes::default();
                outcomes.add(played);
                self.subjects.insert(subject.to_string(), outcomes);
            }
        }
    }

    fn titles_played(&self) -> usize {
        self.subjects.values().filter(|outcomes| outcomes.played > 0).count()
    }
}

/// The closed outcomes as of one date.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// The latest date whose outcomes these counts include, unix seconds.
    pub as_of: u64,
    /// Every closed row; a title counts as played when anyone played it.
    pub household: Tally,
    /// Each viewer active at `as_of` who played at least
    /// [`MIN_VIEWER_TITLES`] titles, by [`viewer_key`]: the closed rows whose
    /// cut they were active at, a title counting as played when they played it.
    pub viewers: BTreeMap<String, Tally>,
}

impl Record {
    /// Count every panel row whose horizon had closed by `as_of`.
    pub fn as_of(dataset: &[Example], activity: &Activity, horizon_secs: u64, as_of: u64) -> Self {
        let present = activity.present_at(as_of);
        let mut household = Tally::default();
        let mut viewers = vec![Tally::default(); present.len()];
        let mut active_by_cut: HashMap<u64, Vec<bool>> = HashMap::new();
        for row in dataset.iter().filter(|row| row.cut_unix.saturating_add(horizon_secs) <= as_of) {
            let subject = subject_of(&row.item_id);
            household.add(subject, row.label >= 0.5);
            let active =
                active_by_cut.entry(row.cut_unix).or_insert_with(|| present.iter().map(|member| member.active_at(row.cut_unix)).collect());
            for ((member, tally), _) in present.iter().zip(viewers.iter_mut()).zip(active.iter()).filter(|(_, active)| **active) {
                tally.add(subject, row.played_by.contains(&member.viewer));
            }
        }
        let viewers = present
            .into_iter()
            .zip(viewers)
            .filter(|(_, tally)| tally.titles_played() >= MIN_VIEWER_TITLES)
            .map(|(member, tally)| (member.key.clone(), tally))
            .collect();
        Self { as_of, household, viewers }
    }
}

/// How a viewer is named in a [`Record`]: the source's namespace and its id.
pub fn viewer_key(viewer: &Viewer) -> String {
    match viewer {
        Viewer::PlexAccount(id) => format!("plex:{id}"),
        Viewer::TautulliUser(user) => format!("tautulli:{user}"),
        Viewer::JellyfinUser(user) => format!("jellyfin:{user}"),
        Viewer::TracearrUser(user) => format!("tracearr:{user}"),
        Viewer::TraktUser(user) => format!("trakt:{user}"),
    }
}

/// When each named viewer played anything: who is part of the household at a
/// date.
#[derive(Debug, Clone, Default)]
pub struct Activity {
    /// Sorted by key.
    members: Vec<Member>,
}

#[derive(Debug, Clone)]
struct Member {
    key: String,
    viewer: Viewer,
    /// Ascending.
    epochs: Vec<u64>,
}

impl Member {
    /// Played something in the [`ACTIVE_SECS`] before `at`.
    fn active_at(&self, at: u64) -> bool {
        let first = self.epochs.partition_point(|epoch| *epoch < at.saturating_sub(ACTIVE_SECS));
        self.epochs.get(first).is_some_and(|epoch| *epoch < at)
    }
}

impl Activity {
    /// From every play of every item; a play that names no viewer names nobody.
    pub fn new<'p>(plays: impl IntoIterator<Item = &'p Play>) -> Self {
        let mut by_viewer: HashMap<&Viewer, Vec<u64>> = HashMap::new();
        for play in plays {
            if let Some(viewer) = &play.viewer {
                by_viewer.entry(viewer).or_default().push(play.epoch);
            }
        }
        let mut members: Vec<Member> = by_viewer
            .into_iter()
            .map(|(viewer, mut epochs)| {
                epochs.sort_unstable();
                Member { key: viewer_key(viewer), viewer: viewer.clone(), epochs }
            })
            .collect();
        members.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        Self { members }
    }

    fn present_at(&self, at: u64) -> Vec<&Member> {
        self.members.iter().filter(|member| member.active_at(at)).collect()
    }
}

/// The titles that may vote, with their vectors.
pub struct Pool<'v> {
    members: Vec<(String, &'v [f32])>,
}

impl<'v> Pool<'v> {
    /// Every named subject the store holds a vector for, once.
    pub fn new<'s>(vectors: &'v VectorStore, subjects: impl IntoIterator<Item = &'s str>) -> Self {
        let mut names: Vec<&str> = subjects.into_iter().collect();
        names.sort_unstable();
        names.dedup();
        let members = names.into_iter().filter_map(|subject| Some((subject.to_string(), vectors.vector(subject)?))).collect();
        Self { members }
    }

    /// The pool ranked by resemblance to `subject`, nearest first, without
    /// `subject` itself (and so without any season of the same show); `None`
    /// when the store has no vector for it.
    pub fn shortlist(&self, vectors: &VectorStore, subject: &str) -> Option<Shortlist<'_>> {
        let query = vectors.vector(subject)?;
        let mut ranked: Vec<(&str, f32)> = self
            .members
            .iter()
            .filter(|(member, vector)| member != subject && vector.len() == query.len())
            .map(|(member, vector)| (member.as_str(), dot(query, vector)))
            .filter(|(_, similarity)| similarity.is_finite())
            .collect();
        let nearest_first = |a: &(&str, f32), b: &(&str, f32)| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0));
        if ranked.len() > SHORTLIST {
            ranked.select_nth_unstable_by(SHORTLIST, nearest_first);
            ranked.truncate(SHORTLIST);
        }
        ranked.sort_unstable_by(nearest_first);
        Some(Shortlist(ranked))
    }
}

/// One title's nearest titles, nearest first, with their cosine similarity.
pub struct Shortlist<'p>(Vec<(&'p str, f32)>);

/// The nearest titles behind a taste that went the way it leans, nearest
/// first: played ones for a warm taste, ones nobody here played for a cold one.
#[derive(Debug, Clone, PartialEq)]
pub struct Likeness {
    pub subjects: Vec<String>,
    pub played: bool,
}

impl Likeness {
    /// `like A, B (played here)`, naming the nearest titles `title` knows;
    /// `None` when it knows none of them.
    pub fn note<'t>(&self, title: impl Fn(&str) -> Option<&'t str>) -> Option<String> {
        let names: Vec<&str> = self.subjects.iter().filter_map(|subject| title(subject)).take(NAMED).collect();
        let here = if self.played { "played here" } else { "unplayed here" };
        (!names.is_empty()).then(|| format!("like {} ({here})", names.join(", ")))
    }
}

/// Taste for one title, and the titles it rests on.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub taste: f64,
    /// `None` at a taste of 0 or when no neighbour went its way.
    pub like: Option<Likeness>,
}

impl Shortlist<'_> {
    /// The household's taste for the title, as of `record` (see
    /// [`Shortlist::read`]). `None` before any outcome has closed.
    pub fn taste(&self, record: &Record) -> Option<f64> {
        self.speaker(record).map(|(taste, _)| taste)
    }

    /// The household's taste for the title and the neighbours behind it.
    pub fn read(&self, record: &Record) -> Option<Reading> {
        let (taste, tally) = self.speaker(record)?;
        Some(Reading { taste, like: self.likeness(record, taste, tally) })
    }

    /// Who speaks for the household, and their taste: the warmest viewer in
    /// the record, or the household as a whole when the record has none.
    fn speaker<'r>(&self, record: &'r Record) -> Option<(f64, &'r Tally)> {
        let warmest = record.viewers.values().filter_map(|tally| Some((self.lean(tally)?, tally))).max_by(|a, b| a.0.total_cmp(&b.0));
        warmest.or_else(|| Some((self.lean(&record.household)?, &record.household)))
    }

    /// One audience's taste: the similarity-weighted played share of the
    /// title's nearest [`NEIGHBOURS`] with a closed outcome in `tally`,
    /// smoothed by [`PRIOR_WEIGHT`] toward its overall rate, as log-odds minus
    /// the overall log-odds.
    fn lean(&self, tally: &Tally) -> Option<f64> {
        let overall = tally.overall.rate()?;
        let (mut weighted, mut weight) = (0.0, 0.0);
        for (_, rate, similarity) in voters(&self.0, tally) {
            let vote = f64::from(similarity.max(0.0));
            weighted += vote * rate;
            weight += vote;
        }
        let rate = (weighted + PRIOR_WEIGHT * overall) / (weight + PRIOR_WEIGHT);
        Some(logit(rate) - logit(overall))
    }

    /// The voters of `tally` that went the way `taste` leans: played by the
    /// speaker when warm, played by nobody here when cold.
    fn likeness(&self, record: &Record, taste: f64, tally: &Tally) -> Option<Likeness> {
        if taste == 0.0 {
            return None;
        }
        let played = taste > 0.0;
        let agrees = |subject: &str| {
            let outcomes = if played { tally.subjects.get(subject) } else { record.household.subjects.get(subject) };
            outcomes.is_some_and(|outcomes| (outcomes.played > 0) == played)
        };
        let subjects: Vec<String> =
            voters(&self.0, tally).map(|(subject, ..)| subject).filter(|subject| agrees(subject)).map(str::to_string).collect();
        (!subjects.is_empty()).then_some(Likeness { subjects, played })
    }
}

/// The nearest [`NEIGHBOURS`] with a closed outcome in `tally`: subject,
/// played share, similarity.
fn voters<'a>(nearest: &'a [(&'a str, f32)], tally: &'a Tally) -> impl Iterator<Item = (&'a str, f64, f32)> + 'a {
    nearest.iter().filter_map(|(subject, similarity)| Some((*subject, tally.subjects.get(*subject)?.rate()?, *similarity))).take(NEIGHBOURS)
}

/// Fill the taste of every never-played panel row, each with the outcomes
/// closed by its own cut. A store without vectors leaves every row at 0.
pub fn fill(dataset: &mut [Example], vectors: &VectorStore, activity: &Activity, horizon_secs: u64) {
    if vectors.is_empty() {
        return;
    }
    let mut records: BTreeMap<u64, Record> = BTreeMap::new();
    for row in dataset.iter().filter(|row| row.features.never_played) {
        records.entry(row.cut_unix).or_insert_with(|| Record::as_of(dataset, activity, horizon_secs, row.cut_unix));
    }
    let subjects: Vec<String> = dataset.iter().map(|row| subject_of(&row.item_id).to_string()).collect();
    let pool = Pool::new(vectors, subjects.iter().map(String::as_str));
    let mut shortlists: HashMap<&str, Option<Shortlist>> = HashMap::new();
    for (row, subject) in dataset.iter_mut().zip(&subjects).filter(|(row, _)| row.features.never_played) {
        let shortlist = shortlists.entry(subject.as_str()).or_insert_with(|| pool.shortlist(vectors, subject));
        let taste = shortlist.as_ref().zip(records.get(&row.cut_unix)).and_then(|(shortlist, record)| shortlist.taste(record));
        row.features = row.features.with_taste(taste);
    }
}

/// Card id → taste and the neighbours behind it, for every card with no
/// recorded play, from the outcomes the adopted fit learned from. Seasons
/// share their show's answer.
pub fn read_cards(cards: &[ArchiveCard], vectors: &VectorStore, record: &Record) -> HashMap<String, Reading> {
    let pool = Pool::new(vectors, record.household.subjects.keys().map(String::as_str));
    let mut by_subject: HashMap<&str, Option<Reading>> = HashMap::new();
    let mut out = HashMap::new();
    for card in cards.iter().filter(|card| card.last_watched_days.is_none()) {
        let subject = subject_of(&card.id);
        let reading = by_subject.entry(subject).or_insert_with(|| pool.shortlist(vectors, subject)?.read(record));
        if let Some(reading) = reading {
            out.insert(card.id.clone(), reading.clone());
        }
    }
    out
}

/// Card id → taste, as [`read_cards`] without the neighbours.
pub fn for_cards(cards: &[ArchiveCard], vectors: &VectorStore, record: &Record) -> HashMap<String, f64> {
    read_cards(cards, vectors, record).into_iter().map(|(id, reading)| (id, reading.taste)).collect()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn logit(rate: f64) -> f64 {
    let rate = rate.clamp(RATE_BOUNDS.0, RATE_BOUNDS.1);
    (rate / (1.0 - rate)).ln()
}

#[cfg(test)]
mod tests;
