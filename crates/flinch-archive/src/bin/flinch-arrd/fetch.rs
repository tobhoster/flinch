//! Everything the daemon reads over HTTP: *arr inventory and disks, Plex watch
//! state, Tautulli history, and the connections it borrows from Maintainerr.
//! Every *arr read goes to each instance in turn ([`super::args`]); what one
//! instance says is its own (ids, tags, mounts) and is kept apart.

use super::Args;
use anyhow::{Context, Result};
use flinch_archive::arr::instances::Connection;
use flinch_archive::arr::{ArrMovie, ArrSeries};
use flinch_archive::body;
use flinch_archive::capacity::App;
use flinch_archive::plex::SonarrEpisodes;
use flinch_archive::rules::facts::InstanceTags;

/// Redirects are not followed (a key or token would travel with them), so one
/// answered here would otherwise read as an empty or unparseable body.
pub(super) fn refuse_redirect(status: reqwest::StatusCode) -> Result<()> {
    anyhow::ensure!(!status.is_redirection(), "answered HTTP {status}, a redirect FLINCH does not follow: configure the final URL");
    Ok(())
}

pub(super) async fn fetch_json(client: &reqwest::Client, url: &str, api_key: &str) -> Result<serde_json::Value> {
    let response = client.get(url).header("X-Api-Key", api_key).send().await.context("fetch failed; check url/key")?;
    let status = response.status();
    if !status.is_success() {
        // The body carries the reason ("database is locked"); a bare status
        // code sends the operator to the wrong app's logs.
        let body = body::read_text(response).await.unwrap_or_default();
        anyhow::bail!("{url}: HTTP {status}: {}", body.chars().take(200).collect::<String>().trim());
    }
    let body = body::read(response).await.context("arr response unreadable")?;
    serde_json::from_slice(&body).context("arr response was not JSON")
}

/// Every episode of one series (`/api/v3/episode`) in its Sonarr instance,
/// for confirming a season whose Plex episode count differs from Sonarr's
/// file count.
pub(super) async fn fetch_series_episodes(client: &reqwest::Client, args: &Args, instance: &str, series_id: u32) -> Result<SonarrEpisodes> {
    let arr = args.arr(App::Sonarr, instance).with_context(|| format!("no Sonarr instance named {instance:?}"))?;
    let url = format!("{}/api/v3/episode?seriesId={series_id}", arr.base);
    let rows = fetch_json(client, &url, &arr.key).await?;
    Ok(SonarrEpisodes::from_rows(rows.as_array().map(Vec::as_slice).unwrap_or_default()))
}

pub(super) struct Fetched {
    pub(super) movies: Vec<ArrMovie>,
    pub(super) series: Vec<ArrSeries>,
    /// Files every instance removed lately, from the cached history read.
    pub(super) removals: Vec<flinch_archive::outside::Removal>,
    /// Card id → download ids (torrent hashes) of its files, from the same read.
    pub(super) downloads: std::collections::BTreeMap<String, Vec<String>>,
    /// Each instance's tag id → label, for rules scoped by tag.
    pub(super) tags: Vec<InstanceTags>,
}

/// Parse a JSON array row by row: a malformed row is skipped and counted,
/// never allowed to fail the whole library — and with it the daemon loop.
fn parse_rows<T: serde::de::DeserializeOwned>(app: &str, value: serde_json::Value) -> Result<Vec<T>> {
    let rows: Vec<serde_json::Value> = serde_json::from_value(value).with_context(|| format!("{app} payload is not an array"))?;
    let rows = flinch_archive::arr::parse_rows(rows);
    if let Some(error) = &rows.first_error {
        eprintln!(
            "[flinch-arrd] {app}: {} of {} row(s) skipped as malformed (first: {error})",
            rows.skipped,
            rows.skipped + rows.parsed.len()
        );
    }
    Ok(rows.parsed)
}

/// One app's tag id → label (`GET /api/v3/tag`).
pub(super) type TagLabels = std::collections::HashMap<u32, String>;

/// Every tag of one app. `None` means the labels could not be read.
async fn tag_labels(client: &reqwest::Client, base: &str, key: &str) -> Option<TagLabels> {
    #[derive(serde::Deserialize)]
    struct Tag {
        id: u32,
        label: String,
    }
    match fetch_json(client, &format!("{base}/api/v3/tag"), key)
        .await
        .and_then(|value| serde_json::from_value::<Vec<Tag>>(value).context("tag payload shape"))
    {
        Ok(tags) => Some(tags.into_iter().map(|tag| (tag.id, tag.label)).collect()),
        Err(error) => {
            eprintln!("[flinch-arrd] tags unreadable at {base}: keep-tagged items there are held and tag rules see no tags: {error:#}");
            None
        }
    }
}

/// Ids of the tags labelled with the operator's keep label (case-insensitive).
/// `None` means a keep label is set but the labels could not be read — the
/// caller then fails closed.
fn keep_tag_ids(labels: Option<&TagLabels>, keep_tag: &str) -> Option<Vec<u32>> {
    let keep_tag = keep_tag.trim();
    if keep_tag.is_empty() {
        return Some(Vec::new());
    }
    labels.map(|labels| labels.iter().filter(|(_, label)| label.eq_ignore_ascii_case(keep_tag)).map(|(id, _)| *id).collect())
}

/// A keep-tagged item is a hard guard. Unreadable tags hold every item of that
/// app for the cycle: a keep tag FLINCH cannot see must not be deleted past.
fn mark_keep(tags: &[u32], keep_ids: Option<&[u32]>) -> bool {
    keep_ids.map_or(true, |ids| tags.iter().any(|tag| ids.contains(tag)))
}

/// Newest file arrival per season (`/api/v3/episodefile?seriesId=`): each
/// season's dwell clock. ISO-8601 UTC strings order lexicographically.
async fn season_arrivals(
    client: &reqwest::Client,
    base: &str,
    key: &str,
    series_id: u32,
) -> Result<std::collections::HashMap<u32, String>> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct EpisodeFile {
        season_number: u32,
        #[serde(default)]
        date_added: Option<String>,
    }
    let url = format!("{base}/api/v3/episodefile?seriesId={series_id}");
    let files: Vec<EpisodeFile> = serde_json::from_value(fetch_json(client, &url, key).await?).context("episodefile payload shape")?;
    let mut newest: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    for file in files {
        let Some(date) = file.date_added else { continue };
        let slot = newest.entry(file.season_number).or_default();
        if date > *slot {
            *slot = date;
        }
    }
    Ok(newest)
}

/// Every instance's library. One unreadable library fails the read: an
/// instance whose items vanished would read as deleted, never as unknown.
pub(super) async fn fetch_inventory(client: &reqwest::Client, args: &Args, keep_tag: &str) -> Result<Fetched> {
    let (mut movies, mut series, mut tags) = (Vec::new(), Vec::new(), Vec::new());
    for arr in &args.arrs {
        let path = match arr.app {
            App::Radarr => "movie",
            App::Sonarr => "series",
        };
        let url = format!("{}/api/v3/{path}", arr.base);
        let (rows, labels) = tokio::join!(fetch_json(client, &url, &arr.key), tag_labels(client, &arr.base, &arr.key));
        let rows = rows.with_context(|| format!("{} library", arr.key()))?;
        let keep = keep_tag_ids(labels.as_ref(), keep_tag);
        match arr.app {
            App::Radarr => {
                let mut read: Vec<ArrMovie> = parse_rows(&arr.key(), rows)?;
                for movie in &mut read {
                    movie.instance.clone_from(&arr.name);
                    movie.keep = mark_keep(&movie.tags, keep.as_deref());
                }
                movies.append(&mut read);
            }
            App::Sonarr => {
                let mut read: Vec<ArrSeries> = parse_rows(&arr.key(), rows)?;
                for show in &mut read {
                    show.instance.clone_from(&arr.name);
                    show.keep = mark_keep(&show.tags, keep.as_deref());
                    file_dates(client, arr, show).await;
                }
                series.append(&mut read);
            }
        }
        tags.push((arr.app, arr.name.clone(), labels));
    }
    // Dwell runs from when the household got each item, not from its current
    // file: needs today's files and their dates, so it comes last.
    let super::history::Attached { removals, downloads } = super::history::attach(client, args, &mut movies, &mut series).await;
    Ok(Fetched { movies, series, removals, downloads, tags })
}

/// Each season's newest file date, for a show with files; unreadable dates
/// leave its seasons reading as fresh.
async fn file_dates(client: &reqwest::Client, arr: &Connection, show: &mut ArrSeries) {
    if !show.seasons.iter().any(|season| season.statistics.episode_file_count > 0) {
        return;
    }
    match season_arrivals(client, &arr.base, &arr.key, show.id).await {
        Ok(mut newest) => {
            for season in &mut show.seasons {
                season.files_added = newest.remove(&season.season_number);
            }
        }
        Err(error) => {
            eprintln!("[flinch-arrd] {}: file dates for {:?} unreadable, its seasons read as fresh: {error:#}", arr.key(), show.title)
        }
    }
}
/// Does a value look like a redacted secret rather than a usable one?
pub(super) fn looks_masked(value: &str) -> bool {
    value.contains("...") || value.len() < 12
}
/// Maintainerr's Tautulli connection, if it has one configured.
pub(super) async fn maintainerr_tautulli_credentials(http: &reqwest::Client, args: &Args) -> Option<(String, String)> {
    let settings = maintainerr_settings(http, args).await.ok()?;
    let url = settings.get("tautulli_url")?.as_str()?.to_string();
    let key = settings.get("tautulli_api_key")?.as_str()?.to_string();
    if url.is_empty() || key.is_empty() {
        return None;
    }
    // Maintainerr's API returns secrets masked (`765...7c1`). Borrowing that
    // looks like it worked and then silently yields nothing — say so instead.
    if looks_masked(&key) {
        println!("[flinch-arrd] tautulli html: Maintainerr returns a masked key; set FLINCH_TAUTULLI_KEY to use Tautulli");
        return None;
    }
    println!("[flinch-arrd] tautulli credentials: borrowed from Maintainerr's settings");
    Some((url, key))
}
/// Read the Plex host/port/token Maintainerr already uses.
///
/// Borrowing beats asking: the operator configured this connection once, and
/// FLINCH only reads it. The token never leaves the process — it is used for
/// the Plex calls in this run and never written to the state volume.
/// Maintainerr's own settings document — the one place both borrowed connections
/// (Plex, Tautulli) come from, so there is a single definition of how to ask.
pub(super) async fn maintainerr_settings(http: &reqwest::Client, args: &Args) -> anyhow::Result<serde_json::Value> {
    let url = format!("{}/api/settings", args.maintainerr_url.trim_end_matches('/'));
    let response = http
        .get(&url)
        .bearer_auth(args.maintainerr_key.trim())
        .header("Accept", "application/json")
        .send()
        .await
        .context("maintainerr settings request failed")?;
    refuse_redirect(response.status()).context("maintainerr settings")?;
    let response = response.error_for_status().context("maintainerr refused the settings request")?;
    let body = body::read(response).await.context("maintainerr settings response unreadable")?;
    serde_json::from_slice(&body).context("maintainerr settings response was not JSON")
}
/// Whether Maintainerr has Seerr configured; `None` when its settings could
/// not be read, so a missing answer never produces a warning.
pub(super) async fn maintainerr_seerr_configured(http: &reqwest::Client, args: &Args) -> Option<bool> {
    let settings = maintainerr_settings(http, args).await.ok()?;
    Some(settings.get("seerr_url").and_then(|url| url.as_str()).is_some_and(|url| !url.trim().is_empty()))
}
/// Each instance's view of its disks: every mount (`/api/v3/diskspace`) and
/// where its library lives (`/api/v3/rootfolder`). An instance that refuses
/// is logged and left out — its items then sit on no governed volume and are
/// never evicted.
pub(super) async fn fetch_disks(client: &reqwest::Client, args: &Args) -> Vec<flinch_archive::capacity::AppDisks> {
    let mut found = Vec::new();
    for arr in &args.arrs {
        match instance_disks(client, arr).await {
            Ok(disks) => found.push(disks),
            Err(error) => eprintln!("[flinch-arrd] {} disks unavailable, its items stay ungoverned: {error:#}", arr.key()),
        }
    }
    found
}

async fn instance_disks(client: &reqwest::Client, arr: &Connection) -> Result<flinch_archive::capacity::AppDisks> {
    use flinch_archive::arr::{ArrDiskSpace, ArrMediaManagement, ArrRootFolder};
    use flinch_archive::capacity::{AppDisks, RecycleBin};
    let base = &arr.base;
    let (disk_url, root_url, media_url) =
        (format!("{base}/api/v3/diskspace"), format!("{base}/api/v3/rootfolder"), format!("{base}/api/v3/config/mediamanagement"));
    let (disks, roots, media) = tokio::join!(
        fetch_json(client, &disk_url, &arr.key),
        fetch_json(client, &root_url, &arr.key),
        fetch_json(client, &media_url, &arr.key),
    );
    let diskspace: Vec<ArrDiskSpace> = serde_json::from_value(disks?).context("diskspace payload shape")?;
    let roots: Vec<ArrRootFolder> = serde_json::from_value(roots?).context("rootfolder payload shape")?;
    // The recycle bin only decides how long evicted bytes are credited;
    // unreadable settings fall back to the longer default, the safe side.
    let recycle = match media.and_then(|value| serde_json::from_value::<ArrMediaManagement>(value).context("mediamanagement payload shape"))
    {
        Ok(settings) => RecycleBin::from_settings(&settings.recycle_bin, settings.recycle_bin_cleanup_days),
        Err(error) => {
            eprintln!("[flinch-arrd] {} recycle-bin settings unreadable, assuming {} days: {error:#}", arr.key(), RecycleBin::DEFAULT_DAYS);
            RecycleBin::Unknown
        }
    };
    let (accessible, unreachable): (Vec<ArrRootFolder>, Vec<ArrRootFolder>) =
        roots.into_iter().partition(|root| root.accessible != Some(false));
    for root in &unreachable {
        eprintln!("[flinch-arrd] {} root folder {} is not accessible — its items stay ungoverned", arr.key(), root.path);
    }
    Ok(AppDisks {
        app: arr.app,
        instance: arr.name.clone(),
        diskspace: diskspace.iter().map(Into::into).collect(),
        root_folders: accessible.into_iter().map(Into::into).collect(),
        recycle,
    })
}
