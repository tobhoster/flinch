//! The library as the UI sees it: one row per item — on disk or not — with the
//! decision, the reason in the operator's words, and the watch evidence.

use flinch_archive::arr::{ArrMovie, ArrSeries};
use flinch_archive::daemon::NeverPlayedHold;
use flinch_archive::govern::Governance;
use flinch_archive::maintainerr::{self as mx, OwnedState};
use flinch_archive::policy::ScoreVerdict;
use flinch_archive::score::ReclaimScore;
use flinch_archive::watch::WatchEntry;
use flinch_archive::{ArchiveCard, ArchivePolicy, ItemSnapshot, ReconcileOutput};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Everything one cycle decided, as the item view needs it.
#[derive(Clone, Copy)]
pub(super) struct ItemInputs<'a> {
    pub(super) cards: &'a [ArchiveCard],
    pub(super) scored: &'a [ReclaimScore],
    pub(super) movies: &'a [ArrMovie],
    pub(super) series: &'a [ArrSeries],
    pub(super) policy: &'a ArchivePolicy,
    pub(super) verdicts: &'a HashMap<String, ScoreVerdict>,
    pub(super) governance: &'a Governance,
    /// FLINCH's exclusions and memberships in Maintainerr as they stand after
    /// this cycle's sync; a dry run changes neither.
    pub(super) owned: &'a OwnedState,
    /// The titles that route each eviction, and where each titled collection
    /// leads (route and window) by id.
    pub(super) titles: &'a mx::CollectionTitles,
    pub(super) destinations: &'a BTreeMap<i64, mx::Destination>,
    pub(super) watch: &'a HashMap<String, WatchEntry>,
    pub(super) report: &'a ReconcileOutput,
    pub(super) plex_ids: &'a HashMap<String, flinch_archive::ids::PlexIds>,
    pub(super) play_keys: &'a HashMap<String, flinch_archive::plex::PlayKeys>,
    /// Never-played reclaim the settings or disk pressure would run now, and
    /// why it is held instead.
    pub(super) never_played_held: Option<NeverPlayedHold>,
}

pub(super) fn build_items(inputs: ItemInputs) -> Vec<ItemSnapshot> {
    let ItemInputs {
        cards,
        scored,
        movies,
        series,
        policy,
        verdicts,
        governance,
        owned,
        titles,
        destinations,
        watch,
        report,
        plex_ids,
        play_keys,
        never_played_held,
    } = inputs;
    // Publish the passive view the web UI renders (single source: this
    // daemon). The UI holds no keys and never talks to the arrs.
    // Snapshot for the UI: EVERYTHING in the libraries, not just what the
    // policy considered. A view that hides half the library is a lie; items
    // with nothing on disk get an explicit status instead of silence.
    let deleted: HashSet<&str> = report.deleted_ids.iter().map(String::as_str).collect();

    let card_row = |card: &flinch_archive::ArchiveCard,
                    movie: Option<&flinch_archive::arr::ArrMovie>,
                    show: Option<&flinch_archive::arr::ArrSeries>| {
        let is_delete = deleted.contains(card.id.as_str());
        let membership = owned.scheduled.get(&card.id);
        let poster = movie
            .and_then(|m| flinch_archive::arr::poster_url(&m.images))
            .or_else(|| show.and_then(|s| flinch_archive::arr::poster_url(&s.images)));
        let year = movie.and_then(|m| m.year).or_else(|| show.and_then(|s| s.year));
        flinch_archive::ItemSnapshot {
            id: card.id.clone(),
            title: card.title.clone(),
            kind: if card.kind == flinch_archive::LibraryKind::Movie { "movie" } else { "season" }.to_string(),
            size_bytes: card.size_bytes,
            decision: if is_delete { "delete" } else { "keep" }.to_string(),
            // The policy's own decision in plain words: the same verdict-aware
            // `decide` reconcile used, so the stated reason cannot drift from
            // the action. Never-played items also say why they are not
            // reclaimed, because a 78% score next to "kept" needs an answer.
            reason: {
                let verdict = verdicts.get(&card.id).copied();
                let decision = flinch_archive::policy::decide(card, &policy, verdict);
                let never_played = matches!(decision, flinch_archive::policy::Reason::KeepBecauseNeverWatchedIsSoleCopy)
                    || (matches!(decision, flinch_archive::policy::Reason::KeepBecauseNotCompleted)
                        && card.season_state == Some(flinch_archive::card::SeasonState::Empty));
                let permitted = flinch_archive::policy::reclaims_bytes(&decision) > 0;
                // Same comparison the plan gates with, so a NaN never "clears".
                let below_floor = verdict.filter(|v| permitted && !(v.p_safe >= policy.score_floor));
                // No watch entry at all is missing evidence, not "never played":
                // saying so would send the operator hunting for plays that exist.
                let no_evidence = matches!(decision, flinch_archive::policy::Reason::KeepBecauseNoWatchEvidence);
                // A movie somebody started and nobody finished has a play date,
                // which the row shows: "never played" would be the same false lead.
                let unplayed = if card.is_watched == Some(false) && card.last_watched_days.is_some() {
                    "Started, never finished"
                } else {
                    "Never played"
                };
                match (never_played, policy.unwatched_reclaim.enabled) {
                    _ if no_evidence => "No watch evidence (no usable Plex or Tautulli record this run), so it is held".to_string(),
                    (true, false) => match never_played_held {
                        Some(hold) => format!("{unplayed}; never-played reclaim is held {}", hold.until()),
                        None => format!("{unplayed}; never-played reclaim is off"),
                    },
                    (true, true) => format!("{unplayed}, outside the never-played reclaim terms"),
                    (false, _) if is_delete => decision.describe(),
                    (false, _) => match below_floor {
                        Some(v) => format!("Below the score floor: P(safe) {:.0}% < {:.0}%", v.p_safe * 100.0, policy.score_floor * 100.0),
                        None if permitted => governance.held_reason(&card.id),
                        None => decision.describe(),
                    },
                }
            },
            delete_probability: if is_delete { 1.0 } else { 0.0 },
            // Truthful, not aspirational: a FLINCH exclusion exists right now
            // (it is released only when the item is actually scheduled).
            protected: owned.protected.contains_key(&card.id),
            poster_url: poster,
            year,
            quality: movie.and_then(|m| m.quality()),
            season_label: card.season_index.map(|n| format!("S{n}")),
            episodes: card.episodes_total.filter(|n| *n > 0),
            age_days: Some(card.added_days_ago),
            last_watched_days: card.last_watched_days,
            // Display reads the SAME merged map the model scores: a UI that
            // says "unknown" while the model scores 79% off a stale export
            // is a lie in a safety system.
            watched_fraction: watch.get(&card.id).map(|entry| entry.progress),
            watch_source: watch.get(&card.id).map(|entry| entry.source.label().to_string()),
            p_safe: None,
            forecast: None,
            reasons: Vec::new(),
            hard_guard: None,
            title_slug: movie.and_then(|m| m.title_slug.clone()).or_else(|| show.and_then(|s| s.title_slug.clone())),
            volume: governance.volume_for(&card.id),
            series_status: show.and_then(|s| s.status.clone()),
            last_aired_epoch: show.and_then(|s| s.last_aired_epoch()),
            plex: plex_ids.get(&card.id).cloned(),
            play_keys: play_keys.get(&card.id).cloned(),
            genres: movie.map(|m| m.genres.clone()).or_else(|| show.map(|s| s.genres.clone())).unwrap_or_default(),
            on_disk: presence(card, movie, show),
            inflow: None,
            // Where it sits once handed over; until then, where it is headed.
            // With the Leaving Soon title blank nothing is announced (see
            // `NeverPlayedHold`), so no item reads as headed there.
            route: membership
                .and_then(|entry| destinations.get(&entry.collection_id))
                .map(|destination| destination.route)
                .or_else(|| is_delete.then(|| titles.route(report.announced_ids.contains(&card.id)))),
            // A membership stands whatever this cycle decided: in a dry run a
            // kept item still leaves on its collection's schedule.
            handed_at: membership.map(|entry| entry.added_at),
            leaves_at: membership.and_then(|entry| leaves_at(entry, destinations)),
        }
    };

    let mut items: Vec<flinch_archive::ItemSnapshot> = cards
        .iter()
        .zip(scored.iter())
        .map(|(card, score)| {
            let movie = movies.iter().find(|m| format!("radarr-{}", m.id) == card.id);
            let show = series.iter().find(|s| card.id.starts_with(&format!("sonarr-{}-", s.id)));
            let mut row = card_row(card, movie, show);
            row.p_safe = Some(score.p_safe);
            row.forecast = Some(score.forecast);
            row.reasons = score.top_reasons(3).iter().map(|signal| signal.detail.clone()).collect();
            row.hard_guard = score.hard_guard.map(|guard| guard.to_string());
            row.inflow = flinch_archive::inflow::advise(card, score);
            row
        })
        .collect();

    // Movies and seasons with NO files never became cards; they still belong
    // in the library view — including their watch evidence, which is exactly
    // what the operator checks ("I did watch that").
    let snapshot_now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    for movie in movies {
        if movie.to_card().is_none() {
            let id = format!("radarr-{}", movie.id);
            let entry = watch.get(&id);
            items.push(flinch_archive::ItemSnapshot {
                id: id.clone(),
                title: movie.title.clone(),
                kind: "movie".to_string(),
                size_bytes: movie.size_on_disk,
                decision: "none".to_string(),
                reason: "Nothing on disk".to_string(),
                delete_probability: 0.0,
                protected: false,
                poster_url: flinch_archive::arr::poster_url(&movie.images),
                year: movie.year,
                quality: movie.quality(),
                season_label: None,
                episodes: None,
                age_days: None,
                last_watched_days: entry.and_then(|e| flinch_archive::plex::age_days(e.last_watched_epoch, snapshot_now)),
                watched_fraction: entry.map(|e| e.progress),
                watch_source: entry.map(|e| e.source.label().to_string()),
                p_safe: None,
                forecast: None,
                reasons: vec!["no file on disk — nothing to reclaim".to_string()],
                hard_guard: None,
                title_slug: movie.title_slug.clone(),
                volume: None,
                series_status: None,
                last_aired_epoch: None,
                plex: None,
                play_keys: None,
                genres: Vec::new(),
                on_disk: Vec::new(),
                inflow: None,
                route: None,
                handed_at: None,
                leaves_at: None,
            });
        }
    }
    for series_item in series {
        let on_disk: std::collections::HashSet<u32> = series_item.to_cards().iter().filter_map(|c| c.season_index).collect();
        for season in &series_item.seasons {
            if on_disk.contains(&season.season_number) {
                continue;
            }
            let id = format!("sonarr-{}-s{}", series_item.id, season.season_number);
            let entry = watch.get(&id);
            items.push(flinch_archive::ItemSnapshot {
                id: id.clone(),
                title: format!("{} S{}", series_item.title, season.season_number),
                kind: "season".to_string(),
                size_bytes: 0,
                decision: "none".to_string(),
                reason: "Nothing on disk".to_string(),
                delete_probability: 0.0,
                protected: false,
                poster_url: flinch_archive::arr::poster_url(&series_item.images),
                year: series_item.year,
                quality: None,
                season_label: Some(format!("S{}", season.season_number)),
                episodes: None,
                age_days: None,
                last_watched_days: entry.and_then(|e| flinch_archive::plex::age_days(e.last_watched_epoch, snapshot_now)),
                watched_fraction: entry.map(|e| e.progress),
                watch_source: entry.map(|e| e.source.label().to_string()),
                p_safe: None,
                forecast: None,
                reasons: vec!["no file on disk — nothing to reclaim".to_string()],
                hard_guard: None,
                title_slug: series_item.title_slug.clone(),
                volume: None,
                series_status: series_item.status.clone(),
                last_aired_epoch: series_item.last_aired_epoch(),
                plex: None,
                play_keys: None,
                genres: Vec::new(),
                on_disk: Vec::new(),
                inflow: None,
                route: None,
                handed_at: None,
                leaves_at: None,
            });
        }
    }
    items
}

/// When a FLINCH membership leaves: its hand-off plus the window of the
/// collection it sits in. Without a window Maintainerr acts on its next run,
/// at a time FLINCH cannot know.
fn leaves_at(entry: &mx::ScheduledEntry, destinations: &BTreeMap<i64, mx::Destination>) -> Option<u64> {
    let days = destinations.get(&entry.collection_id)?.window_days.filter(|days| *days > 0)?;
    Some(entry.added_at.saturating_add(u64::try_from(days).ok()?.saturating_mul(86_400)))
}

/// The presence spans the card was dated from: its movie's, or its season's.
fn presence(card: &ArchiveCard, movie: Option<&ArrMovie>, show: Option<&ArrSeries>) -> Vec<flinch_archive::presence::Span> {
    match (movie, show) {
        (Some(movie), _) => movie.on_disk.clone(),
        (None, Some(show)) => {
            let season = show.seasons.iter().find(|season| Some(season.season_number) == card.season_index);
            season.map(|season| season.on_disk.clone()).unwrap_or_default()
        }
        (None, None) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flinch_archive::capacity::{Latch, LibraryVolumes, OnDisk};
    use flinch_archive::daemon::RuntimeSettings;
    use flinch_archive::policy::UnwatchedReclaim;
    use flinch_archive::score::{HouseholdContext, ScoreWeights};
    use flinch_archive::watch::WatchSource;
    use std::collections::BTreeSet;

    /// The reason the row of a movie nobody finished gives, for one played
    /// `played` days ago or never, with never-played reclaim `enabled` and `hold`.
    fn reason(played: Option<f32>, enabled: bool, hold: Option<NeverPlayedHold>) -> String {
        let card = ArchiveCard {
            id: "radarr-1".to_string(),
            is_watched: Some(false),
            last_watched_days: played,
            ..flinch_archive::golden::golden_movie()
        };
        row_reason(card, enabled, hold)
    }

    /// The reason `card`'s row gives, with never-played reclaim `enabled` and
    /// `hold`.
    fn row_reason(card: ArchiveCard, enabled: bool, hold: Option<NeverPlayedHold>) -> String {
        let settings = RuntimeSettings::default();
        let mut policy =
            ArchivePolicy { unwatched_reclaim: UnwatchedReclaim { enabled, ..Default::default() }, ..ArchivePolicy::default() };
        let live = HouseholdContext { watch_source: Some(WatchSource::Plex), ..Default::default() };
        let scored = [flinch_archive::score::score(&card, live, &ScoreWeights::default(), settings.score_temperature)];
        let governance = flinch_archive::govern::govern(
            LibraryVolumes::default(),
            HashMap::new(),
            |_| false,
            &settings,
            &Latch::default(),
            OnDisk::default(),
            &mut policy,
        );
        let report = flinch_archive::reconcile(&[], &[], &HashMap::new(), &BTreeSet::new(), &policy, &HashMap::new(), &governance.goal);
        let items = build_items(ItemInputs {
            cards: std::slice::from_ref(&card),
            scored: &scored,
            movies: &[],
            series: &[],
            policy: &policy,
            verdicts: &HashMap::new(),
            governance: &governance,
            owned: &OwnedState::default(),
            titles: &settings.collection_titles(),
            destinations: &BTreeMap::new(),
            watch: &HashMap::new(),
            report: &report,
            plex_ids: &HashMap::new(),
            play_keys: &HashMap::new(),
            never_played_held: hold,
        });
        items[0].reason.clone()
    }

    #[rstest::rstest]
    #[case::off(false, None, "; never-played reclaim is off")]
    #[case::held(false, Some(NeverPlayedHold::IncompleteEvidence), "; never-played reclaim is held until the watch evidence is complete")]
    #[case::held_untitled(
        false,
        Some(NeverPlayedHold::LeavingSoonUntitled),
        "; never-played reclaim is held until a Leaving Soon collection is named"
    )]
    #[case::armed(true, None, ", outside the never-played reclaim terms")]
    fn a_started_movie_is_never_called_never_played(#[case] enabled: bool, #[case] hold: Option<NeverPlayedHold>, #[case] why: &str) {
        // The row's Watched column shows the play. Why it is not reclaimed
        // reads as before: the Overview buckets on it.
        assert_eq!(reason(Some(200.0), enabled, hold), format!("Started, never finished{why}"));
        assert_eq!(reason(None, enabled, hold), format!("Never played{why}"));
    }

    #[rstest::rstest]
    #[case::off(false)]
    #[case::armed(true)]
    fn an_item_with_no_watch_evidence_is_not_said_to_be_missing_from_plex(#[case] enabled: bool) {
        // Plex may well list it: on a shared server the only record can be one
        // account's zero, dropped as not evidence. The Overview buckets on the
        // "No watch evidence" prefix.
        let card =
            ArchiveCard { id: "radarr-1".to_string(), is_watched: None, last_watched_days: None, ..flinch_archive::golden::golden_movie() };
        assert_eq!(row_reason(card, enabled, None), "No watch evidence (no usable Plex or Tautulli record this run), so it is held");
    }
}
