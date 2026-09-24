//! Watch evidence over HTTP: the Plex library and history, Tautulli history.
//!
//! Every listing is paged to its end and says whether it got there: a silent
//! under-read turns "we did not look" into "nobody watched", and the daemon
//! may only arm the never-played rule on a record it read in full.

use super::fetch::refuse_redirect;
use anyhow::Context;
use flinch_archive::plex::{EpisodeGuids, PlexContainer, PlexEnvelope, PlexEpisodes, PlexLibrary, PlexMetadata};
use flinch_archive::tautulli::{self, TautulliRow};
use std::collections::{BTreeSet, HashSet};

/// Rows per page, for both servers.
const PAGE: usize = 500;
/// A listing longer than this many pages is reported incomplete rather than
/// read forever.
const MAX_PAGES: usize = 400;
/// How far back play history is needed: the fitter's oldest cut date plus its
/// horizon. History past this may stop being read without being "incomplete".
const HISTORY_HORIZON_SECS: u64 = ((flinch_archive::fit::MAX_CUT_DAYS + flinch_archive::fit::DEFAULT_HORIZON_DAYS) as u64) * 86_400;

/// One cycle's read of Plex.
pub(super) struct PlexFetch {
    pub library: PlexLibrary,
    pub history: Vec<PlexMetadata>,
    /// Every movie/show section listed to its end.
    pub items_complete: bool,
    /// History paged to its end or past [`HISTORY_HORIZON_SECS`].
    pub history_complete: bool,
    /// More than one account can play on this server (or it could not be told).
    pub multi_account: bool,
    /// ratingKeys the operator marked to keep: items labelled with the keep
    /// tag, and members of a collection with that title.
    pub keep_keys: HashSet<String>,
    /// Every movie, show and season ratingKey the sections listed: what Plex
    /// holds, as proof an item is gone only when `items_complete`.
    pub listed: HashSet<String>,
}

struct PlexClient<'a> {
    http: &'a reqwest::Client,
    base: &'a str,
    token: &'a str,
}

impl PlexClient<'_> {
    async fn get(&self, path: &str) -> anyhow::Result<PlexContainer> {
        // The token travels in the header only: a URL lands in proxy and
        // server access logs, and in errors that quote it.
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .header("X-Plex-Token", self.token)
            .header("Accept", "application/json")
            .send()
            .await
            .context("plex request failed")?;
        let status = response.status();
        refuse_redirect(status).context("plex")?;
        let body = response.text().await.context("plex body read failed")?;
        serde_json::from_str::<PlexEnvelope>(&body).map(|envelope| envelope.container).map_err(|error| {
            anyhow::anyhow!("plex {status} body did not parse: {error}; first bytes: {:.140}", body.chars().take(140).collect::<String>())
        })
    }

    /// Every row of a listing, and whether the end was reached. With `until`,
    /// a page whose oldest `viewedAt` is before it also ends the listing.
    async fn get_all(&self, path: &str, until: Option<u64>) -> (Vec<PlexMetadata>, bool) {
        let separator = if path.contains('?') { '&' } else { '?' };
        let mut rows: Vec<PlexMetadata> = Vec::new();
        for _ in 0..MAX_PAGES {
            let url = format!("{path}{separator}X-Plex-Container-Start={}&X-Plex-Container-Size={PAGE}", rows.len());
            let page = match self.get(&url).await {
                Ok(page) => page,
                Err(error) => {
                    eprintln!("[flinch-arrd] plex {path} stopped after {} row(s): {error:#}", rows.len());
                    return (rows, false);
                }
            };
            let read = page.metadata.len();
            let past_horizon = until.is_some_and(|until| page.metadata.iter().filter_map(|row| row.viewed_at).any(|at| at < until));
            rows.extend(page.metadata);
            match page.total_size {
                _ if past_horizon => return (rows, true),
                Some(total) if rows.len() as u64 >= total => return (rows, true),
                Some(total) if read == 0 => {
                    eprintln!("[flinch-arrd] plex {path}: {} of {total} row(s), then an empty page", rows.len());
                    return (rows, false);
                }
                None if read < PAGE => return (rows, true),
                _ => {}
            }
        }
        eprintln!("[flinch-arrd] plex {path}: stopped at the {MAX_PAGES}-page cap with {} row(s)", rows.len());
        (rows, false)
    }

    /// A section listing of one Plex type, with every row stamped with its section.
    async fn section(&self, key: &str, plex_type: u8) -> (Vec<PlexMetadata>, bool) {
        self.stamped(&format!("/library/sections/{key}/all?type={plex_type}&includeGuids=1"), key).await
    }

    /// One show's seasons with their episode counts. The section's own season
    /// listing (`type=3`) leaves `leafCount` and `viewedLeafCount` out (seen
    /// live), and a season without them can be neither matched nor measured.
    async fn seasons(&self, section: &str, show: &str) -> (Vec<PlexMetadata>, bool) {
        let (mut rows, complete) = self.stamped(&format!("/library/metadata/{show}/children?excludeAllLeaves=1"), section).await;
        rows.retain(|row| row.media_type == "season");
        (rows, complete)
    }

    /// Every row of a listing, stamped with `section` where Plex left it out.
    async fn stamped(&self, path: &str, section: &str) -> (Vec<PlexMetadata>, bool) {
        let (mut rows, complete) = self.get_all(path, None).await;
        let section = section.parse().ok();
        for row in &mut rows {
            if row.library_section_id.is_none() {
                row.library_section_id = section;
            }
        }
        (rows, complete)
    }

    /// Every row of a listing, or an error when it could not be read to its end.
    async fn listing(&self, path: &str) -> anyhow::Result<Vec<PlexMetadata>> {
        let (rows, complete) = self.get_all(path, None).await;
        anyhow::ensure!(complete, "plex {path} could not be read to its end");
        Ok(rows)
    }

    /// ratingKeys the operator marked to keep in one section: movies and shows
    /// carrying the label `tag`, and members of a collection titled `tag` (a
    /// member episode keeps its season). Names compare case-insensitively.
    async fn keep_keys(&self, section: &str, tag: &str) -> anyhow::Result<Vec<String>> {
        let named = |title: &str| title.trim().eq_ignore_ascii_case(tag);
        let mut keys = Vec::new();
        let labels = self.get(&format!("/library/sections/{section}/label")).await?;
        for label in labels.directory.iter().filter(|label| named(&label.title) && !label.fast_key.is_empty()) {
            // A keep label that cannot be followed is a keep FLINCH cannot
            // see: the error fails this Plex read, so nothing is handed over.
            let path = server_path(&label.fast_key)
                .with_context(|| format!("keep label {:?} points at {:?}, not a path on this Plex server", label.title, label.fast_key))?;
            keys.extend(self.listing(path).await?.into_iter().map(|row| row.rating_key));
        }
        let collections = self.listing(&format!("/library/sections/{section}/all?type=18")).await?;
        for collection in collections.iter().filter(|row| named(&row.title)) {
            for member in self.listing(&format!("/library/collections/{}/children", collection.rating_key)).await? {
                keys.push(match member.media_type.as_str() {
                    "episode" => member.parent_rating_key.unwrap_or(member.rating_key),
                    _ => member.rating_key,
                });
            }
        }
        keys.retain(|key| !key.is_empty());
        Ok(keys)
    }
}

/// A path Plex answered with, fit to append to the base URL: absolute on this
/// server. Appended, `@host/…` would read as user-info and send the token to
/// `host`, and `//host/…` names another host to any client that resolves it.
fn server_path(path: &str) -> Option<&str> {
    (path.starts_with('/') && !path.starts_with("//")).then_some(path)
}

/// Read the Plex library (with GUIDs), every season container, server-wide
/// play history, the account list and the keep markers under `keep_tag`. A
/// failure to list the sections or to read the keep markers is an error: a
/// keep FLINCH could not see must not let an item go. Anything else is
/// reported through the completeness flags.
pub(super) async fn fetch_plex(http: &reqwest::Client, base_url: &str, token: &str, now: u64, keep_tag: &str) -> anyhow::Result<PlexFetch> {
    let plex = PlexClient { http, base: base_url.trim_end_matches('/'), token };
    let sections = plex.get("/library/sections").await?;
    let (mut movies, mut shows, mut seasons) = (Vec::new(), Vec::new(), Vec::new());
    let mut items_complete = true;
    let mut keep_keys = HashSet::new();
    let keep_tag = keep_tag.trim();
    for section in &sections.directory {
        // Plex types: 1 movie, 2 show; seasons are read per show.
        match section.kind.as_str() {
            "movie" => {
                let (rows, complete) = plex.section(&section.key, 1).await;
                movies.extend(rows);
                items_complete &= complete;
            }
            "show" => {
                let (rows, complete) = plex.section(&section.key, 2).await;
                items_complete &= complete;
                for show in rows.iter().filter(|show| !show.rating_key.is_empty()) {
                    let (children, complete) = plex.seasons(&section.key, &show.rating_key).await;
                    seasons.extend(children);
                    items_complete &= complete;
                }
                shows.extend(rows);
            }
            _ => continue,
        }
        if !keep_tag.is_empty() {
            let marked = plex.keep_keys(&section.key, keep_tag).await.with_context(|| format!("keep markers in plex section {}", section.key))?;
            keep_keys.extend(marked);
        }
    }

    let (history, history_complete) =
        plex.get_all("/status/sessions/history/all?sort=viewedAt:desc", Some(now.saturating_sub(HISTORY_HORIZON_SECS))).await;
    let history_accounts = history.iter().filter_map(|row| row.account_id).collect::<std::collections::HashSet<_>>().len();
    let multi_account = match plex.get("/accounts").await {
        Ok(accounts) => accounts.account.iter().filter(|account| account.id > 0).count() > 1 || history_accounts > 1,
        Err(error) => {
            // Unknown is treated as shared: the admin's silence then proves nothing.
            eprintln!("[flinch-arrd] plex accounts unreadable, treating the server as shared: {error:#}");
            true
        }
    };
    println!(
        "[flinch-arrd] plex: {} movie row(s), {} show(s), {} season(s){} · history {} row(s){} · {}",
        movies.len(),
        shows.len(),
        seasons.len(),
        if items_complete { "" } else { " (INCOMPLETE)" },
        history.len(),
        if history_complete { "" } else { " (INCOMPLETE)" },
        if multi_account { "several accounts" } else { "one account" },
    );
    let listed: HashSet<String> =
        movies.iter().chain(&shows).chain(&seasons).map(|row| row.rating_key.clone()).filter(|key| !key.is_empty()).collect();
    Ok(PlexFetch {
        library: PlexLibrary::new(&movies, &shows, &seasons),
        history,
        items_complete,
        history_complete,
        multi_account,
        keep_keys,
        listed,
    })
}

/// A show's episodes with their TVDB ids (`allLeaves`), for confirming a season
/// whose episode count differs from Sonarr's file count.
pub(super) async fn fetch_show_episodes(http: &reqwest::Client, base_url: &str, token: &str, show_rating_key: &str) -> anyhow::Result<PlexEpisodes> {
    let plex = PlexClient { http, base: base_url.trim_end_matches('/'), token };
    let rows = plex.listing(&format!("/library/metadata/{show_rating_key}/allLeaves?includeGuids=1")).await?;
    Ok(PlexEpisodes::from_rows(&rows))
}

/// Where the episode GUID index is cached between cycles.
const EPISODE_GUIDS_FILE: &str = "episode-guids.json";

/// Where Plex files each episode of `shows` now, for plays recorded under a
/// ratingKey it has since replaced. The cached index is reused while it is
/// under a day old; otherwise every show's `allLeaves` is read and cached, so
/// the cost is at most one read per show per day. A show that cannot be read
/// is left out until the next read, its migrated plays unjoined; the failures
/// are logged on one line and never fail the cycle.
pub(super) async fn episode_guids(http: &reqwest::Client, base_url: &str, token: &str, shows: &BTreeSet<&str>, now: u64) -> EpisodeGuids {
    let path = super::state_dir().join(EPISODE_GUIDS_FILE);
    let cached = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice::<EpisodeGuids>(&bytes).ok());
    if let Some(cached) = cached.filter(|index| index.is_fresh(now)) {
        return cached;
    }
    let plex = PlexClient { http, base: base_url.trim_end_matches('/'), token };
    let mut index = EpisodeGuids::new(now);
    let mut failed = Vec::new();
    for show in shows {
        match plex.listing(&format!("/library/metadata/{show}/allLeaves")).await {
            Ok(rows) => index.add_show(show, &rows),
            Err(error) => failed.push((*show, error)),
        }
    }
    if let Some((show, error)) = failed.first() {
        eprintln!(
            "[flinch-arrd] plex episode GUIDs: {} of {} show(s) unreadable, their earlier plays stay unjoined for up to a day (show {show}: {error:#})",
            failed.len(),
            shows.len()
        );
    }
    let written = serde_json::to_vec(&index)
        .map_err(std::io::Error::other)
        .and_then(|bytes| flinch_archive::persist::replace(&path, &bytes));
    if let Err(error) = written {
        eprintln!("[flinch-arrd] {EPISODE_GUIDS_FILE} write failed: {error}");
    }
    index
}

/// One cycle's read of Tautulli.
pub(super) struct TautulliFetch {
    pub rows: Vec<TautulliRow>,
    /// Every page was read and Tautulli is still recording.
    pub complete: bool,
    /// Its `keep_history` switches; `None` when they could not be read, which
    /// vouches for nothing.
    pub keep_history: Option<tautulli::KeepHistory>,
}

/// Page Tautulli's stream history to its end (`recordsFiltered`), newest first,
/// ungrouped so every stream is its own row.
pub(super) async fn fetch_tautulli_history(http: &reqwest::Client, base: &str, key: &str, now: u64) -> anyhow::Result<TautulliFetch> {
    let base = base.trim_end_matches('/');
    let mut rows: Vec<TautulliRow> = Vec::new();
    let mut paged_to_end = false;
    for _ in 0..MAX_PAGES {
        let url = format!(
            "{base}/api/v2?apikey={key}&cmd=get_history&grouping=0&order_column=date&order_dir=desc&start={}&length={PAGE}",
            rows.len()
        );
        let page = async {
            // The URL carries the API key: keep it out of every error.
            let response = http.get(&url).send().await.map_err(reqwest::Error::without_url).context("tautulli request failed")?;
            let status = response.status();
            refuse_redirect(status).context("tautulli")?;
            let body = response.text().await.map_err(reqwest::Error::without_url).context("tautulli body read failed")?;
            tautulli::parse_history_page(&body).ok_or_else(|| {
                // Silence here cost an afternoon: say what came back instead.
                anyhow::anyhow!("tautulli {status} did not answer a history page; first bytes: {:.160}", body.chars().take(160).collect::<String>())
            })
        }
        .await;
        let page = match page {
            Ok(page) => page,
            Err(error) if rows.is_empty() => return Err(error),
            Err(error) => {
                eprintln!("[flinch-arrd] tautulli history stopped after {} row(s): {error:#}", rows.len());
                break;
            }
        };
        let read = page.rows.len();
        rows.extend(page.rows);
        match page.records_filtered {
            Some(total) if rows.len() as u64 >= total => {
                paged_to_end = true;
                break;
            }
            Some(total) if read == 0 => {
                eprintln!("[flinch-arrd] tautulli history: {} of {total} row(s), then an empty page", rows.len());
                break;
            }
            // The old unpaged shape answers everything at once.
            None => {
                paged_to_end = read < PAGE;
                break;
            }
            Some(_) => {}
        }
    }
    let recording = tautulli::coverage(&rows).is_some_and(|coverage| coverage.recording_at(now));
    if !recording {
        eprintln!("[flinch-arrd] tautulli: no stream in the last {} days — not treated as recording", tautulli::MAX_SILENCE_SECS / 86_400);
    }
    let keep_history = match fetch_keep_history(http, base, key).await {
        Ok(keep_history) => Some(keep_history),
        Err(error) => {
            eprintln!("[flinch-arrd] tautulli user/library settings unreadable, so its silence is not absence evidence: {error:#}");
            None
        }
    };
    Ok(TautulliFetch { complete: paged_to_end && recording, rows, keep_history })
}

/// Tautulli's `keep_history` switch for every user and library section. One
/// page of up to 1000 libraries; a longer table reads as unknown.
async fn fetch_keep_history(http: &reqwest::Client, base: &str, key: &str) -> anyhow::Result<tautulli::KeepHistory> {
    let get = |command: &'static str| async move {
        let url = format!("{base}/api/v2?apikey={key}&cmd={command}");
        // The URL carries the API key: keep it out of every error.
        let response = http.get(&url).send().await.map_err(reqwest::Error::without_url)?;
        refuse_redirect(response.status())?;
        let response = response.error_for_status().map_err(reqwest::Error::without_url)?;
        anyhow::Ok(response.text().await.map_err(reqwest::Error::without_url)?)
    };
    let (users, libraries) = tokio::join!(get("get_users"), get("get_libraries_table&length=1000"));
    let (users, libraries) = (users.context("get_users")?, libraries.context("get_libraries_table")?);
    tautulli::KeepHistory::parse(&users, &libraries).context("answered without its user and library switches")
}

#[cfg(test)]
mod tests {
    use super::server_path;

    #[rstest::rstest]
    #[case::a_path_on_this_server("/library/sections/1/all?label=7", true)]
    #[case::user_info_naming_another_host("@evil.example/library", false)]
    #[case::a_scheme_relative_url("//evil.example/library", false)]
    #[case::an_absolute_url("http://evil.example/library", false)]
    fn only_a_path_on_this_server_is_followed(#[case] path: &str, #[case] followed: bool) {
        assert_eq!(server_path(path).is_some(), followed);
    }
}
