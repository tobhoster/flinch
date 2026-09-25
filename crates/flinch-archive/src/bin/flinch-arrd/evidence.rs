//! Watch evidence for one cycle: the operator's export, Plex item state and
//! history, Tautulli history — joined to the library by catalogue identity,
//! with a health record of what could not be read.

use super::fetch::{fetch_series_episodes, maintainerr_tautulli_credentials};
use super::media::{episode_guids, fetch_plex, fetch_show_episodes, fetch_tautulli_history};
use super::{state_dir, Args};
use anyhow::{Context, Result};
use flinch_archive::arr::{ArrMovie, ArrSeries};
use flinch_archive::ids::PlexIds;
use flinch_archive::plex::{migration, EpisodeIds, PlayJoin, PlayKeys, PlexMetadata, Resolution, RowKey, Unconfirmed, WatchTarget};
use flinch_archive::tautulli::TautulliRow;
use flinch_archive::watch::{EvidenceHealth, WatchEntry, WatchSource};
use flinch_archive::ArchiveCard;
use std::collections::{BTreeSet, HashMap, HashSet};

/// Everything the cycle knows about who watched what.
pub(super) struct Evidence {
    /// The merged watch map the score and the policy read.
    pub(super) watch: HashMap<String, WatchEntry>,
    pub(super) watch_targets: Vec<WatchTarget>,
    pub(super) resolution: Resolution,
    pub(super) health: EvidenceHealth,
    pub(super) plex_history_rows: Vec<PlexMetadata>,
    pub(super) tautulli_rows: Vec<TautulliRow>,
    /// Card id → Plex placement, GUID-resolved only.
    pub(super) plex_ids: HashMap<String, PlexIds>,
    pub(super) play_keys: HashMap<String, PlayKeys>,
    /// Items the operator marked to keep in Plex: a label or collection named
    /// like the keep tag, on the movie, the show, or the season.
    pub(super) plex_keeps: BTreeSet<String>,
    /// Every ratingKey Plex listed this cycle; `None` unless Plex was read and
    /// listed completely — the only proof an item is gone from Plex.
    pub(super) plex_listed: Option<HashSet<String>>,
}

pub(super) async fn gather(
    args: &Args,
    http: &reqwest::Client,
    settings: &flinch_archive::daemon::RuntimeSettings,
    movies: &[ArrMovie],
    series: &[ArrSeries],
    cards: &[ArchiveCard],
    cycle_now: u64,
) -> Result<Evidence> {
    let watch_path = args.watch_state.as_ref().ok_or_else(|| anyhow::anyhow!("--watch-state or FLINCH_WATCH_STATE is required"))?;
    let watch_text = std::fs::read_to_string(watch_path)?;
    let mut watch: HashMap<String, WatchEntry> =
        serde_json::from_str(&watch_text).with_context(|| format!("parsing {}", watch_path.display()))?;
    // Watch targets: every library item the media server could know about,
    // including ones with no file left on disk. Their watch state is exactly
    // what an operator checks ("I did watch that"), and reading it off cards
    // silently dropped it.
    // Identity travels with every target: catalogue ids from the *arrs (the
    // exact join to Plex GUIDs), and per season the show year and Sonarr's
    // episode-file count — Plex's leafCount must equal it or the season stays
    // unresolved (PX-04), since orderings can differ between the two.
    let external = flinch_archive::arr::external_ids(movies, series);
    let mut season_facts: HashMap<String, (Option<u32>, u32)> = HashMap::new();
    for series_item in series {
        for season in &series_item.seasons {
            season_facts.insert(
                format!("sonarr-{}-s{}", series_item.id, season.season_number),
                (series_item.year, season.statistics.episode_file_count),
            );
        }
    }
    let with_identity = |mut target: flinch_archive::plex::WatchTarget| {
        target.external = external.get(&target.id).cloned().unwrap_or_default();
        if let Some((year, files)) = season_facts.get(&target.id) {
            target.year = *year;
            target.episode_files = Some(*files);
        }
        target
    };
    let mut watch_targets: Vec<WatchTarget> = cards
        .iter()
        .map(|card| {
            let mut target = flinch_archive::plex::WatchTarget::from(card);
            // Arrival decides whether "no stream" is evidence or a blind spot.
            // `added_days_ago` is the file-arrival age (AR-01); an unknown
            // arrival reads as "just arrived", so it can never prove absence.
            target.added_epoch = Some(cycle_now.saturating_sub((card.added_days_ago * 86_400.0) as u64));
            with_identity(target)
        })
        .collect();
    for movie in movies.iter().filter(|movie| movie.to_card().is_none()) {
        watch_targets.push(with_identity(flinch_archive::plex::WatchTarget {
            id: format!("radarr-{}", movie.id),
            kind: flinch_archive::LibraryKind::Movie,
            title: movie.title.clone(),
            year: movie.year,
            show_title: None,
            season_index: None,
            episodes_total: None,
            episode_files: None,
            external: Default::default(),
            added_epoch: None,
            on_disk: false,
        }));
    }
    for series_item in series {
        let on_disk: std::collections::HashSet<u32> = series_item.to_cards().iter().filter_map(|c| c.season_index).collect();
        for season in series_item.seasons.iter().filter(|season| !on_disk.contains(&season.season_number)) {
            watch_targets.push(with_identity(flinch_archive::plex::WatchTarget {
                id: format!("sonarr-{}-s{}", series_item.id, season.season_number),
                kind: flinch_archive::LibraryKind::Season,
                title: series_item.title.clone(),
                year: series_item.year,
                show_title: Some(series_item.title.clone()),
                season_index: Some(season.season_number),
                episodes_total: Some(season.statistics.total_episode_count),
                episode_files: None,
                external: Default::default(),
                added_epoch: None,
                on_disk: false,
            }));
        }
    }

    // Plex is optional. Tautulli supplies the plays (and, once its coverage
    // is known, absence evidence) with a properly authenticated API, so the
    // stack runs on Tautulli + Maintainerr + Sonarr/Radarr alone. Settings
    // (the UI) or FLINCH_PLEX_URL / FLINCH_PLEX_TOKEN add item-level state.
    let env = (std::env::var("FLINCH_PLEX_URL").unwrap_or_default(), std::env::var("FLINCH_PLEX_TOKEN").unwrap_or_default());
    let PlexPair { url: plex_url, token: plex_token, settings_url_unpaired } =
        plex_pair((&settings.plex_url, &settings.plex_token), (&env.0, &env.1));
    if settings_url_unpaired {
        eprintln!("[flinch-arrd] plex: the Settings URL has no token of its own and is not used; enter the Plex token again in Settings");
    }
    if plex_url.is_empty() || plex_token.is_empty() {
        println!("[flinch-arrd] plex: disabled (Tautulli provides the watch evidence)");
    }
    // Tautulli: borrowed from Maintainerr's configuration, the same courtesy
    // as the Plex token — the operator configured it once.
    let (tautulli_url, tautulli_key) =
        match (std::env::var("FLINCH_TAUTULLI_URL").unwrap_or_default(), std::env::var("FLINCH_TAUTULLI_KEY").unwrap_or_default()) {
            (url, key) if !url.is_empty() && !key.is_empty() => (url, key),
            _ => maintainerr_tautulli_credentials(http, args).await.unwrap_or_default(),
        };
    let tautulli_url = with_scheme(&tautulli_url);

    // Fetch both sources, resolve identity by GUID, then derive evidence. A
    // source that failed or read only partly is recorded in `health`, which
    // gates absence evidence and never-played reclaim (TT-01, XC-03).
    let plex_configured = !plex_url.is_empty() && !plex_token.is_empty();
    let tautulli_configured = !tautulli_url.is_empty() && !tautulli_key.is_empty();
    let mut plex = if plex_configured {
        match fetch_plex(http, &plex_url, &plex_token, cycle_now, &settings.keep_tag).await {
            Ok(fetched) => Some(fetched),
            Err(error) => {
                eprintln!("[flinch-arrd] plex fetch failed, no plex evidence and no absence this cycle: {error:#}");
                None
            }
        }
    } else {
        None
    };
    let tautulli = if tautulli_configured {
        match fetch_tautulli_history(http, &tautulli_url, &tautulli_key, cycle_now).await {
            Ok(fetched) => Some(fetched),
            Err(error) => {
                eprintln!("[flinch-arrd] tautulli fetch failed: {error:#}");
                None
            }
        }
    } else {
        println!("[flinch-arrd] tautulli: not configured; Plex evidence only");
        None
    };
    // An empty library still resolves: nothing matches by GUID, and play joins
    // fall back to exact title+year for movies only.
    let empty = flinch_archive::plex::PlexLibrary::default();
    let library = plex.as_ref().map_or(&empty, |fetched| &fetched.library);
    let mut resolution = flinch_archive::plex::resolve(&watch_targets, library);
    // PX-04: a season whose Plex episode count differs from Sonarr's file count
    // resolves only when TVDB episode ids show it is the same season.
    let unconfirmed = resolution.unconfirmed_seasons().len();
    if unconfirmed > 0 {
        let ids = episode_ids(http, args, (&plex_url, &plex_token), resolution.unconfirmed_seasons(), series).await;
        let confirmed = resolution.confirm_seasons(&watch_targets, library, &ids);
        println!("[flinch-arrd] seasons whose episode counts differ: {confirmed} of {unconfirmed} confirmed by TVDB episode ids");
    }
    // Plays from before a library re-add carry ratingKeys Plex has replaced;
    // their plex:// GUIDs still name the item. A movie carries its GUID from
    // the listing; a season needs Plex's current filing of each episode GUID,
    // read only when such episode plays exist, and at most daily.
    let play_rows: Vec<RowKey> = plex
        .iter()
        .flat_map(|fetched| &fetched.history)
        .filter(|row| row.viewed_at.is_some())
        .filter_map(RowKey::plex)
        .chain(tautulli.iter().flat_map(|fetched| &fetched.rows).filter(|row| row.epoch().is_some()).filter_map(TautulliRow::key))
        .collect();
    let unjoined_episodes = migration::unjoined_episodes(library, &play_rows);
    let season_shows = resolution.season_shows();
    let episode_index = if plex.is_some() && unjoined_episodes > 0 && !season_shows.is_empty() {
        let index = episode_guids(http, &plex_url, &plex_token, &season_shows, cycle_now).await;
        Some((index.shows(), resolution.attach_episode_guids(&index)))
    } else {
        None
    };
    // Keep markers the operator set in Plex, as card ids.
    let plex_keeps = plex.as_ref().map(|fetched| resolution.marked(&fetched.keep_keys)).unwrap_or_default();
    if !plex_keeps.is_empty() {
        println!("[flinch-arrd] plex keep markers ('{}'): {} item(s) kept", settings.keep_tag.trim(), plex_keeps.len());
    }
    // TT-01: Tautulli's silence counts only where it keeps every stream: every
    // active user's, in every library section a resolved target lives in.
    let target_sections: std::collections::BTreeSet<u32> = resolution.plex_ids().values().filter_map(|ids| ids.section_id).collect();
    let keep_history = tautulli.as_ref().and_then(|fetched| fetched.keep_history.as_ref());
    let tautulli_keeps_all = keep_history.is_some_and(|keep| keep.covers(target_sections.iter().copied()));
    if let Some(keep) = keep_history.filter(|_| !tautulli_keeps_all) {
        let sections_off: Vec<&u32> = target_sections.iter().filter(|section| !keep.sections_on.contains(section)).collect();
        eprintln!(
            "[flinch-arrd] tautulli keeps no history for user(s) [{}] / library section(s) {sections_off:?}: its silence is not absence evidence",
            keep.users_off.join(", ")
        );
    }
    let health = flinch_archive::watch::EvidenceHealth {
        plex_configured,
        plex_items_ok: plex.as_ref().is_some_and(|fetched| fetched.items_complete),
        plex_history_complete: plex.as_ref().is_some_and(|fetched| fetched.history_complete),
        tautulli_configured,
        tautulli_complete: tautulli.as_ref().is_some_and(|fetched| fetched.complete) && tautulli_keeps_all,
        // Unknown (Plex down) is treated as shared: admin-only zeros prove nothing.
        multi_account: plex.as_ref().map_or(true, |fetched| fetched.multi_account),
        plex_settings_unpaired: settings_url_unpaired,
    };
    let plex_listed = plex.as_mut().filter(|fetched| fetched.items_complete).map(|fetched| std::mem::take(&mut fetched.listed));
    let plex_history_rows: Vec<flinch_archive::plex::PlexMetadata> = plex.map(|fetched| fetched.history).unwrap_or_default();
    let tautulli_rows: Vec<flinch_archive::tautulli::TautulliRow> = tautulli.map(|fetched| fetched.rows).unwrap_or_default();
    println!(
        "[flinch-arrd] identity: {} of {} targets resolved in plex ({} by GUID) · evidence: {}",
        resolution.len(),
        watch_targets.len(),
        resolution.plex_ids().len(),
        if health.problems().is_empty() { "complete".to_string() } else { health.problems().join("; ") },
    );
    if plex_configured && !plex_history_rows.is_empty() {
        // Persist the raw plays: the only outcome record this system has, and
        // fitting needs them after the fact.
        if let Err(error) =
            flinch_archive::persist::replace(state_dir().join("playback.json").as_path(), &serde_json::to_vec(&plex_history_rows)?)
        {
            eprintln!("[flinch-arrd] playback.json write failed: {error}");
        }
    }
    let mut entries = resolution.item_entries(&health);
    let from_history = flinch_archive::plex::history::history_entries(&watch_targets, &resolution, &plex_history_rows);
    flinch_archive::plex::history::merge_history(&mut entries, from_history);
    // Counted after the merge: a play in history that replaced the admin's
    // "nothing played" decides that item just as much as one item state lacked.
    let by_history = entries.values().filter(|entry| entry.source == WatchSource::PlexHistory).count();
    println!(
        "[flinch-arrd] watch evidence: {} of {} library items ({scanned} with files on disk): {} from item state, {by_history} from playback history",
        entries.len(),
        watch_targets.len(),
        entries.len() - by_history,
        scanned = cards.len(),
    );
    if plex_configured {
        // How much of what is on disk Plex vouches for, per kind.
        let count = |movie: bool, pick: &dyn Fn(&ArchiveCard) -> bool| {
            cards.iter().filter(|card| (card.kind == flinch_archive::LibraryKind::Movie) == movie && pick(card)).count()
        };
        let matched = |card: &ArchiveCard| entries.contains_key(&card.id);
        let on_disk = |card: &ArchiveCard| card.size_bytes > 0;
        println!(
            "[flinch-arrd] plex match rate: movies {}/{}, seasons {}/{}",
            count(true, &matched),
            count(true, &on_disk),
            count(false, &matched),
            count(false, &on_disk)
        );
    }
    watch.extend(entries);

    // Tautulli merges before the watch state reaches the cards, or its plays
    // would only reach the display and never the score.
    if !tautulli_rows.is_empty() {
        let plays = flinch_archive::tautulli::plays_by_target(&watch_targets, &resolution, &tautulli_rows);
        // Every absence gate lives in absence_by_target: GUID-resolved target,
        // no stream of any completeness, complete Tautulli, Plex read this
        // cycle, a known arrival after coverage began.
        let absence = flinch_archive::tautulli::absence_by_target(&watch_targets, &resolution, &tautulli_rows, &health, cycle_now);
        println!(
            "[flinch-arrd] tautulli: {} stream(s) · {} target(s) played · {} resolved target(s) never streamed",
            tautulli_rows.len(),
            plays.len(),
            absence.len()
        );
        if let Err(error) =
            flinch_archive::persist::replace(state_dir().join("tautulli.json").as_path(), &serde_json::to_vec(&tautulli_rows)?)
        {
            eprintln!("[flinch-arrd] tautulli.json write failed: {error}");
        }
        let mut from_tautulli = plays;
        from_tautulli.extend(absence);
        flinch_archive::plex::history::merge_history(&mut watch, from_tautulli);
    }
    let joins: Vec<PlayJoin> = watch_targets.iter().map(|target| resolution.join(target)).collect();
    let by_guid = migration::guid_joins(&joins, &play_rows);
    println!(
        "[flinch-arrd] plays joined by plex GUID (item re-added since): {} movie, {} episode · {unjoined_episodes} episode play(s) outside the library{}",
        by_guid.movies,
        by_guid.episodes,
        episode_index.map_or_else(String::new, |(shows, seasons)| format!(" · episode GUIDs of {shows} show(s) reach {seasons} season(s)")),
    );
    let plex_ids = resolution.plex_ids();
    let play_keys = resolution.play_keys();
    Ok(Evidence {
        watch,
        watch_targets,
        resolution,
        health,
        plex_history_rows,
        tautulli_rows,
        plex_ids,
        play_keys,
        plex_keeps,
        plex_listed,
    })
}

/// A base URL as operators type it: `plex:32400` means `http://plex:32400`.
/// Without a scheme the HTTP client takes the host for one and refuses every
/// request, so a Settings value typed as host:port silently cost all evidence.
fn with_scheme(url: &str) -> String {
    let url = url.trim();
    if url.is_empty() || url.contains("://") {
        url.to_string()
    } else {
        format!("http://{url}")
    }
}

/// The Plex connection one cycle uses.
#[derive(Debug, PartialEq, Eq)]
struct PlexPair {
    url: String,
    token: String,
    /// Settings name a URL but no token for it: that URL is not used.
    settings_url_unpaired: bool,
}

/// URL and token are one credential: a token sent to a URL it was not entered
/// with can reach a server that should never see it. Settings win when they
/// hold both; otherwise both come from the environment. A Settings URL is
/// never paired with the environment's token.
fn plex_pair((settings_url, settings_token): (&str, &str), (env_url, env_token): (&str, &str)) -> PlexPair {
    let (settings_url, settings_token) = (settings_url.trim(), settings_token.trim());
    let (url, token) = if !settings_url.is_empty() && !settings_token.is_empty() {
        (settings_url, settings_token)
    } else {
        (env_url.trim(), env_token.trim())
    };
    PlexPair {
        url: with_scheme(url),
        token: token.to_string(),
        settings_url_unpaired: !settings_url.is_empty() && settings_token.is_empty(),
    }
}

/// TVDB episode ids for the seasons whose episode counts disagreed: each Plex
/// show's episodes, and each affected series' Sonarr episodes. A read that
/// fails leaves its seasons unconfirmed.
async fn episode_ids(
    http: &reqwest::Client,
    args: &Args,
    (plex_url, plex_token): (&str, &str),
    unconfirmed: &[Unconfirmed],
    series: &[ArrSeries],
) -> EpisodeIds {
    let mut ids = EpisodeIds::default();
    let shows: BTreeSet<&str> = unconfirmed.iter().flat_map(|season| season.show_rating_keys.iter().map(String::as_str)).collect();
    for show in shows {
        match fetch_show_episodes(http, plex_url, plex_token, show).await {
            Ok(episodes) => {
                ids.plex.insert(show.to_string(), episodes);
            }
            Err(error) => eprintln!("[flinch-arrd] plex episodes of show {show} unreadable: {error:#}"),
        }
    }
    let waiting: HashSet<&str> = unconfirmed.iter().map(|season| season.target_id.as_str()).collect();
    for series_item in series {
        let targets: Vec<String> = series_item
            .seasons
            .iter()
            .map(|season| format!("sonarr-{}-s{}", series_item.id, season.season_number))
            .filter(|id| waiting.contains(id.as_str()))
            .collect();
        if targets.is_empty() {
            continue;
        }
        match fetch_series_episodes(http, args, series_item.id).await {
            Ok(episodes) => ids.sonarr.extend(targets.into_iter().map(|id| (id, episodes.clone()))),
            Err(error) => eprintln!("[flinch-arrd] sonarr episodes of {} unreadable: {error:#}", series_item.title),
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::{plex_pair, with_scheme, PlexPair};

    #[rstest::rstest]
    #[case::typed_as_host_and_port("plex.media.svc.cluster.local:32400", "http://plex.media.svc.cluster.local:32400")]
    #[case::scheme_kept_and_trimmed(" https://tautulli:8181 ", "https://tautulli:8181")]
    // Empty must stay empty: it is how "not configured" is told apart.
    #[case::unset_stays_unset("", "")]
    fn a_base_url_without_a_scheme_is_read_as_http(#[case] typed: &str, #[case] used: &str) {
        assert_eq!(with_scheme(typed), used);
    }

    const ENV: (&str, &str) = ("plex-env:32400", "env-token");

    fn pair(url: &str, token: &str, settings_url_unpaired: bool) -> PlexPair {
        PlexPair { url: url.to_string(), token: token.to_string(), settings_url_unpaired }
    }

    #[rstest::rstest]
    #[case::settings_hold_both(("plex-ui:32400", "ui-token"), ENV, pair("http://plex-ui:32400", "ui-token", false))]
    #[case::settings_empty(("", ""), ENV, pair("http://plex-env:32400", "env-token", false))]
    // The env token must never go to the URL typed in Settings.
    #[case::settings_url_without_its_token(("plex-ui:32400", " "), ENV, pair("http://plex-env:32400", "env-token", true))]
    #[case::settings_token_without_a_url(("", "ui-token"), ENV, pair("http://plex-env:32400", "env-token", false))]
    #[case::settings_url_without_token_and_no_env(("plex-ui:32400", ""), ("", ""), pair("", "", true))]
    fn the_plex_url_and_token_come_from_one_source(#[case] settings: (&str, &str), #[case] env: (&str, &str), #[case] used: PlexPair) {
        assert_eq!(plex_pair(settings, env), used);
    }
}
