//! The household's dated play record, correlated with library items.
//!
//! One correlation for both sides of the model: the fitter builds its panel
//! from it, and the daemon derives the same play-based household signals from
//! it at inference. Both persisted sources feed it — the media server's history
//! (`playback.json`) and Tautulli's streams (`tautulli.json`) — and rows join
//! items through a [`PlayJoin`]: by Plex ratingKey where the item resolved, or
//! by `plex://` GUID where Plex re-added it since, by exact title and year for
//! an unresolved movie, and not at all otherwise.

use crate::card::LibraryKind;
use crate::plex::{PlayJoin, PlayKeys, PlexMetadata, RowKey};
use crate::tautulli::TautulliRow;
use std::collections::{HashMap, HashSet};

/// Plays of the same episode (or movie) closer together than this are one
/// viewing: a paused stream resumed, or one play reported by both sources.
pub const SAME_VIEWING_SECS: u64 = 86_400;

/// Who played something, scoped to the source that said so.
///
/// A Plex account id and a Tautulli user are different names for the same
/// people, so viewers are only ever counted within one namespace.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Viewer {
    PlexAccount(u64),
    TautulliUser(String),
}

/// One play of a library item.
#[derive(Debug, Clone, PartialEq)]
pub struct Play {
    pub epoch: u64,
    /// Episode number within its season; `None` for a movie or an unnumbered row.
    pub episode: Option<u32>,
    /// Who played it, when the source says.
    pub viewer: Option<Viewer>,
    /// Most of the runtime was watched. A stream that stopped early is still a
    /// play — the household touched the item — but completes nothing.
    pub complete: bool,
}

/// Every play both sources recorded, indexed by the keys joins look up.
#[derive(Debug, Default)]
pub struct PlayLog {
    rows: Vec<(RowKey, Play)>,
    by_rating_key: HashMap<String, Vec<usize>>,
    by_parent: HashMap<String, Vec<usize>>,
    by_grandparent: HashMap<String, Vec<usize>>,
    by_guid: HashMap<String, Vec<usize>>,
    by_title_year: HashMap<(String, u32), Vec<usize>>,
}

impl PlayLog {
    /// Collect plays from media-server history rows and Tautulli streams. A row
    /// without a timestamp, or that is neither a movie nor an episode, is not a play.
    pub fn new(plex: &[PlexMetadata], tautulli: &[TautulliRow]) -> Self {
        let mut log = Self::default();
        for row in plex {
            let (Some(epoch), Some(key)) = (row.viewed_at, RowKey::plex(row)) else { continue };
            let viewer = row.account_id.map(Viewer::PlexAccount);
            // Plex writes a history row when an item is scrobbled as watched.
            log.push(key, Play { epoch, episode: row.index.filter(|_| row.media_type.eq_ignore_ascii_case("episode")), viewer, complete: true });
        }
        for row in tautulli {
            let (Some(epoch), Some(key)) = (row.epoch(), row.key()) else { continue };
            let viewer = row.viewer().map(|id| Viewer::TautulliUser(id.to_string()));
            log.push(key, Play { epoch, episode: row.media_index.parse().ok(), viewer, complete: row.is_watch() });
        }
        log
    }

    fn push(&mut self, key: RowKey, play: Play) {
        let index = self.rows.len();
        let add = |map: &mut HashMap<String, Vec<usize>>, key: &Option<String>| {
            if let Some(key) = key {
                map.entry(key.clone()).or_default().push(index);
            }
        };
        add(&mut self.by_rating_key, &key.rating_key);
        add(&mut self.by_guid, &key.guid);
        add(&mut self.by_parent, &key.parent_rating_key);
        add(&mut self.by_grandparent, &key.grandparent_rating_key);
        if let Some(year) = key.year {
            self.by_title_year.entry((key.title.clone(), year)).or_default().push(index);
        }
        self.rows.push((key, play));
    }

    /// Row indices a join could match; the join itself decides.
    fn candidates(&self, join: &PlayJoin) -> Vec<usize> {
        let lookup = |map: &HashMap<String, Vec<usize>>, keys: &[String]| -> Vec<usize> {
            keys.iter().filter_map(|key| map.get(key)).flatten().copied().collect()
        };
        let mut rows = match join {
            PlayJoin::Keys(PlayKeys::Movie { rating_keys, plex_guids }) => {
                let mut rows = lookup(&self.by_rating_key, rating_keys);
                rows.extend(lookup(&self.by_guid, plex_guids));
                rows
            }
            PlayJoin::Keys(PlayKeys::Season { show_rating_keys, season_rating_keys, episode_guids, .. }) => {
                let mut rows = lookup(&self.by_parent, season_rating_keys);
                rows.extend(lookup(&self.by_grandparent, show_rating_keys));
                rows.extend(lookup(&self.by_guid, episode_guids));
                rows
            }
            PlayJoin::MovieTitleYear { title, year } => self.by_title_year.get(&(title.clone(), *year)).cloned().unwrap_or_default(),
            PlayJoin::Unresolved => Vec::new(),
        };
        rows.sort_unstable();
        rows.dedup();
        rows
    }

    fn select(&self, join: &PlayJoin, keep: impl Fn(&RowKey) -> bool) -> Vec<&Play> {
        let mut plays: Vec<&Play> =
            self.candidates(join).into_iter().map(|index| &self.rows[index]).filter(|(key, _)| keep(key)).map(|(_, play)| play).collect();
        plays.sort_by_key(|play| play.epoch);
        plays
    }

    /// Plays of exactly this item, oldest first.
    pub fn item_plays(&self, join: &PlayJoin) -> Vec<&Play> {
        self.select(join, |key| join.matches(key))
    }

    /// Plays that speak for the item's audience, oldest first: the movie's own,
    /// or every season of its show — a household picks shows up as a whole.
    pub fn audience_plays(&self, join: &PlayJoin) -> Vec<&Play> {
        self.select(join, |key| join.matches_audience(key))
    }

    /// The play-derived household signals for one item as of `as_of`: the
    /// daemon asks with `as_of = now`, the fitter with each cut date.
    pub fn evidence(&self, join: &PlayJoin, kind: LibraryKind, as_of: u64) -> PlayEvidence {
        PlayEvidence::as_of(kind, self.item_plays(join), self.audience_plays(join), as_of)
    }
}

/// Play-derived household signals, as of one date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlayEvidence {
    /// The movie, or one episode of the season, was watched to the end on
    /// occasions more than [`SAME_VIEWING_SECS`] apart: watched again, not resumed.
    pub rewatched: bool,
    /// Distinct viewers who finished something of the item's audience plays,
    /// counted per source and taking the larger count, so one person seen by
    /// both is one viewer.
    pub viewers: u32,
}

impl PlayEvidence {
    /// Reduce plays to signals using only finished plays strictly before `as_of`.
    pub fn as_of<'p>(
        kind: LibraryKind,
        item_plays: impl IntoIterator<Item = &'p Play>,
        audience_plays: impl IntoIterator<Item = &'p Play>,
        as_of: u64,
    ) -> Self {
        // Earliest and latest play per viewing unit: the movie itself, or one
        // numbered episode. An unnumbered episode row cannot be told apart from
        // its neighbours, so it cannot prove a rewatch.
        let mut span: HashMap<Option<u32>, (u64, u64)> = HashMap::new();
        for play in item_plays.into_iter().filter(|play| play.complete && play.epoch < as_of) {
            let unit = match kind {
                LibraryKind::Movie => None,
                LibraryKind::Season => match play.episode {
                    Some(episode) => Some(episode),
                    None => continue,
                },
            };
            let entry = span.entry(unit).or_insert((play.epoch, play.epoch));
            entry.0 = entry.0.min(play.epoch);
            entry.1 = entry.1.max(play.epoch);
        }
        let rewatched = span.values().any(|(first, last)| last - first > SAME_VIEWING_SECS);

        let mut plex: HashSet<u64> = HashSet::new();
        let mut tautulli: HashSet<&str> = HashSet::new();
        for play in audience_plays.into_iter().filter(|play| play.complete && play.epoch < as_of) {
            match &play.viewer {
                Some(Viewer::PlexAccount(id)) => {
                    plex.insert(*id);
                }
                Some(Viewer::TautulliUser(name)) => {
                    tautulli.insert(name.as_str());
                }
                None => {}
            }
        }
        let viewers = plex.len().max(tautulli.len()) as u32;
        Self { rewatched, viewers }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plex(json: &str) -> PlexMetadata {
        serde_json::from_str(json).expect("history row fixture")
    }

    fn tautulli(json: &str) -> TautulliRow {
        serde_json::from_str(json).expect("tautulli row fixture")
    }

    fn andor_s1() -> PlayJoin {
        PlayJoin::Keys(PlayKeys::Season {
            show_rating_keys: vec!["70".into()],
            season_rating_keys: vec!["71".into()],
            season: 1,
            episode_guids: Vec::new(),
        })
    }

    fn epochs(plays: Vec<&Play>) -> Vec<u64> {
        plays.into_iter().map(|play| play.epoch).collect()
    }

    #[test]
    fn plays_join_by_rating_key_and_never_by_title() {
        let log = PlayLog::new(
            &[
                plex(r#"{"type":"episode","ratingKey":"701","parentRatingKey":"71","grandparentRatingKey":"70","parentIndex":1,"index":1,"grandparentTitle":"Andor","viewedAt":100}"#),
                plex(r#"{"type":"episode","ratingKey":"721","parentRatingKey":"72","grandparentRatingKey":"70","parentIndex":2,"index":1,"grandparentTitle":"Andor","viewedAt":200}"#),
                // Same show title, another show: a different ratingKey.
                plex(r#"{"type":"episode","ratingKey":"901","parentRatingKey":"91","grandparentRatingKey":"90","parentIndex":1,"index":1,"grandparentTitle":"Andor","viewedAt":300}"#),
                plex(r#"{"type":"movie","ratingKey":"9","title":"Heat","year":1995,"viewedAt":400}"#),
                // A row with no play timestamp is not a play.
                plex(r#"{"type":"episode","ratingKey":"702","parentRatingKey":"71","grandparentRatingKey":"70","parentIndex":1,"index":2}"#),
            ],
            &[
                tautulli(r#"{"media_type":"episode","rating_key":"703","grandparent_rating_key":"70","parent_media_index":"1","media_index":"3","date":"150","percent_complete":"100"}"#),
                // Stopped at 20%: still a play, not a finished one.
                tautulli(r#"{"media_type":"movie","rating_key":"9","title":"Heat","date":"500","percent_complete":"20"}"#),
            ],
        );
        assert_eq!(epochs(log.item_plays(&andor_s1())), vec![100, 150], "both sources, oldest first, only show 70");
        assert_eq!(epochs(log.audience_plays(&andor_s1())), vec![100, 150, 200], "every season of show 70 speaks for it");
        let heat = PlayJoin::Keys(PlayKeys::Movie { rating_keys: vec!["9".into()], plex_guids: Vec::new() });
        let plays = log.item_plays(&heat);
        assert_eq!(epochs(plays.clone()), vec![400, 500]);
        assert!(!plays[1].complete, "the abandoned stream is a play that completes nothing");
    }

    #[test]
    fn an_unresolved_movie_matches_only_its_exact_title_and_year() {
        let log = PlayLog::new(
            &[
                plex(r#"{"type":"movie","ratingKey":"1","title":"Superman","year":1978,"viewedAt":100}"#),
                plex(r#"{"type":"movie","ratingKey":"2","title":"Superman","viewedAt":200}"#),
            ],
            &[],
        );
        let remake = PlayJoin::fallback(LibraryKind::Movie, "Superman", Some(2025));
        assert!(log.item_plays(&remake).is_empty(), "the 1978 play and a yearless row are not the 2025 film's");
        let original = PlayJoin::fallback(LibraryKind::Movie, "Superman", Some(1978));
        assert_eq!(epochs(log.item_plays(&original)), vec![100]);
        assert_eq!(PlayJoin::fallback(LibraryKind::Season, "Andor", Some(2022)), PlayJoin::Unresolved, "no title fallback for seasons");
    }

    fn play(epoch: u64, episode: Option<u32>, viewer: Option<Viewer>) -> Play {
        Play { epoch, episode, viewer, complete: true }
    }

    const DAY: u64 = 86_400;

    #[test]
    fn a_rewatch_is_the_same_episode_finished_on_separate_days_before_the_date() {
        let resumed = [play(10 * DAY, Some(1), None), play(10 * DAY + 3_600, Some(1), None)];
        assert!(!PlayEvidence::as_of(LibraryKind::Season, &resumed, &resumed, 20 * DAY).rewatched, "a resumed stream is one viewing");

        let binge = [play(10 * DAY, Some(1), None), play(12 * DAY, Some(2), None)];
        assert!(!PlayEvidence::as_of(LibraryKind::Season, &binge, &binge, 20 * DAY).rewatched, "two episodes are not a rewatch");

        let again = [play(10 * DAY, Some(1), None), play(40 * DAY, Some(1), None)];
        assert!(PlayEvidence::as_of(LibraryKind::Season, &again, &again, 50 * DAY).rewatched);
        assert!(
            !PlayEvidence::as_of(LibraryKind::Season, &again, &again, 40 * DAY).rewatched,
            "a play on the as-of date itself is the future, not evidence"
        );
        let sampled = [play(10 * DAY, Some(1), None), Play { complete: false, ..play(40 * DAY, Some(1), None) }];
        assert!(!PlayEvidence::as_of(LibraryKind::Season, &sampled, &sampled, 50 * DAY).rewatched, "starting it again is not watching it again");

        let movie_twice = [play(10 * DAY, None, None), play(40 * DAY, None, None)];
        assert!(PlayEvidence::as_of(LibraryKind::Movie, &movie_twice, &movie_twice, 50 * DAY).rewatched);
        assert!(
            !PlayEvidence::as_of(LibraryKind::Season, &movie_twice, &movie_twice, 50 * DAY).rewatched,
            "unnumbered episode rows cannot prove the same episode twice"
        );
    }

    #[test]
    fn viewers_are_counted_within_one_source_never_summed_across_them() {
        let alice_everywhere = [
            play(DAY, Some(1), Some(Viewer::PlexAccount(1))),
            play(DAY, Some(1), Some(Viewer::TautulliUser("alice".into()))),
        ];
        assert_eq!(PlayEvidence::as_of(LibraryKind::Season, &[], &alice_everywhere, 2 * DAY).viewers, 1);

        let household = [
            play(DAY, Some(1), Some(Viewer::TautulliUser("alice".into()))),
            play(DAY, Some(2), Some(Viewer::TautulliUser("bob".into()))),
            play(DAY, Some(3), Some(Viewer::TautulliUser("bob".into()))),
            play(DAY, Some(4), None),
            play(9 * DAY, Some(5), Some(Viewer::TautulliUser("carol".into()))),
        ];
        let evidence = PlayEvidence::as_of(LibraryKind::Season, &[], &household, 5 * DAY);
        assert_eq!(evidence.viewers, 2, "carol played after the date; the unattributed row names nobody");
    }
}
