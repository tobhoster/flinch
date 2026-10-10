//! The cycle's external signals ([`flinch_archive::signals`]): every Radarr
//! and Sonarr instance's imports (read at most every six hours, cached in `arr-imports.json`)
//! and queues, Seerr requests and watchlists, and per-card availability from
//! Prowlarr (a budgeted trickle of searches, cached in `releases.json`)
//! judged against SABnzbd's retention. Every source is best-effort: one that
//! is not configured or cannot be read leaves its field empty and adds one
//! sentence to the problems. Nothing here fails the cycle.

use super::fetch::fetch_json;
use super::Args;
use anyhow::{Context, Result};
use flinch_archive::arr::history::HistoryPage;
use flinch_archive::arr::instances::Connection;
use flinch_archive::signals::arr::{self, ImportCache, ImportRead};
use flinch_archive::signals::release::{self, ReleaseCache, Searched};
use flinch_archive::signals::{seerr, Queued, Signals, Watchlisted};
use flinch_archive::ArchiveCard;
use std::path::Path;

const QUEUE_PAGE_SIZE: usize = 500;
/// Queue pages read per instance before the rest is left out.
const QUEUE_PAGE_CAP: usize = 20;
/// Seerr request and user pages read before the rest is left out.
const SEERR_PAGE_CAP: usize = 50;
/// Watchlist pages read per user before the rest is left out.
const WATCHLIST_PAGE_CAP: u64 = 10;

/// Everything the external sources say this cycle about `cards`.
pub(super) async fn gather(http: &reqwest::Client, args: &Args, cards: &[ArchiveCard], now: u64) -> Signals {
    let mut signals = Signals::default();
    let state = super::state_dir();
    imports(http, args, &state.join("arr-imports.json"), now, &mut signals).await;
    for arr in &args.arrs {
        match queue(http, arr).await {
            Ok(queued) => signals.queue.extend(queued),
            Err(error) => {
                let sentence = format!("{} queue unreadable ({}): its pending downloads not counted", arr.label(), cause(&error));
                problem(&mut signals, sentence, &error, "");
            }
        }
    }
    requests_and_watchlists(http, args, &mut signals).await;
    releases(http, args, cards, &state.join("releases.json"), now, &mut signals).await;
    signals
}

/// Every instance's imports of the window, each read again once its cached
/// read is six hours old. A failed read keeps serving the last one.
async fn imports(http: &reqwest::Client, args: &Args, path: &Path, now: u64, signals: &mut Signals) {
    let mut cache: ImportCache = super::read_state(path);
    let mut read_any = false;
    for arr in &args.arrs {
        if cache.is_fresh(arr.app, &arr.name, now) {
            continue;
        }
        match fetch_json(http, &format!("{}{}", arr.base, arr::imports_path(arr.app, now)), &arr.key).await.and_then(array) {
            Ok(records) => {
                let read = ImportRead { read_at: now, imports: arr::parse_imports(arr.app, &arr.name, records) };
                cache.store(arr.app, &arr.name, read);
                read_any = true;
            }
            Err(error) => {
                let serving = if cache.read(arr.app, &arr.name).is_some() { "the last read serves" } else { "its imports not counted" };
                let sentence = format!("{} import history unreadable ({}): {serving}", arr.label(), cause(&error));
                problem(signals, sentence, &error, "");
            }
        }
    }
    // An instance no longer configured imports nothing into today's disks.
    let before = cache.extra.len();
    cache.extra.retain(|key, _| args.arrs.iter().any(|arr| arr.key() == *key));
    if read_any || cache.extra.len() != before {
        super::write_state(path, &cache);
    }
    signals.imports = cache.imports(now);
}

/// One instance's queue, every page up to the cap.
async fn queue(http: &reqwest::Client, arr: &Connection) -> Result<Vec<Queued>> {
    let mut records = Vec::new();
    for page in 1..=QUEUE_PAGE_CAP {
        let url = format!("{}{}", arr.base, arr::queue_path(arr.app, page, QUEUE_PAGE_SIZE));
        let body: HistoryPage = serde_json::from_value(fetch_json(http, &url, &arr.key).await?).context("queue page shape")?;
        let last = body.records.len() < QUEUE_PAGE_SIZE || (page * QUEUE_PAGE_SIZE) as u64 >= body.total_records;
        records.extend(body.records);
        if last {
            break;
        }
    }
    Ok(arr::parse_queue(arr.app, &arr.name, records))
}

/// Seerr's requests, then every user's watchlist. A user whose watchlist
/// cannot be read is left out; all of them together make one problem.
async fn requests_and_watchlists(http: &reqwest::Client, args: &Args, signals: &mut Signals) {
    let Some((base, key)) = configured(&args.seerr_url, &args.seerr_key) else {
        signals.problems.push("Seerr not configured: requests and watchlists not counted".to_owned());
        return;
    };
    match seerr_rows(http, base, key, seerr::requests_path).await {
        Ok(records) => {
            signals.requests = seerr::parse_requests(records);
            signals.requests_read = true;
        }
        Err(error) => {
            let sentence = format!("Seerr unreachable ({}): requests and watchlists not counted", cause(&error));
            problem(signals, sentence, &error, "");
            return;
        }
    }
    let users = match seerr_rows(http, base, key, seerr::users_path).await {
        Ok(records) => {
            signals.contacts = seerr::parse_contacts(records.clone());
            seerr::parse_users(records)
        }
        Err(error) => {
            let sentence = format!("Seerr users unreadable ({}): watchlists not counted", cause(&error));
            problem(signals, sentence, &error, "");
            return;
        }
    };
    let (mut failed, mut first_error) = (0, None);
    for user in &users {
        match watchlist(http, base, key, user).await {
            Ok(items) => signals.watchlists.extend(items),
            Err(error) => {
                failed += 1;
                first_error.get_or_insert(error);
            }
        }
    }
    if let Some(error) = first_error {
        let sentence =
            format!("Seerr watchlists unreadable for {failed} of {} user(s) ({}): theirs not counted", users.len(), cause(&error));
        problem(signals, sentence, &error, "");
    }
}

/// Every row of a paged Seerr list, up to the cap.
async fn seerr_rows(http: &reqwest::Client, base: &str, key: &str, path: fn(usize) -> String) -> Result<Vec<serde_json::Value>> {
    let mut rows = Vec::new();
    for page in 0..SEERR_PAGE_CAP {
        let skip = page * seerr::TAKE;
        let body: seerr::Page =
            serde_json::from_value(fetch_json(http, &format!("{base}{}", path(skip)), key).await?).context("Seerr page shape")?;
        let last = body.is_last(skip);
        rows.extend(body.results);
        if last {
            break;
        }
    }
    Ok(rows)
}

/// One user's watchlist, every page up to the cap.
async fn watchlist(http: &reqwest::Client, base: &str, key: &str, user: &seerr::User) -> Result<Vec<Watchlisted>> {
    let mut items = Vec::new();
    for page in 1..=WATCHLIST_PAGE_CAP {
        let url = format!("{base}{}", seerr::watchlist_path(user.member, page));
        let body: seerr::WatchlistPage = serde_json::from_value(fetch_json(http, &url, key).await?).context("watchlist page shape")?;
        items.extend(seerr::parse_watchlist(&user.name, body.results));
        if page >= body.total_pages {
            break;
        }
    }
    Ok(items)
}

/// Each card's availability: the cached searches, after this cycle's budget
/// of searches for the cards never searched or searched longest ago. One
/// failed search ends the cycle's searching.
async fn releases(http: &reqwest::Client, args: &Args, cards: &[ArchiveCard], path: &Path, now: u64, signals: &mut Signals) {
    let retention = sab_retention(http, args, signals).await;
    let Some((base, key)) = configured(&args.prowlarr_url, &args.prowlarr_key) else {
        signals.problems.push("Prowlarr not configured: re-download difficulty uses size only".to_owned());
        return;
    };
    let mut cache: ReleaseCache = super::read_state(path);
    // An empty card list is a failed inventory more often than an empty library.
    if !cards.is_empty() {
        cache.retain_cards(cards);
    }
    let mut searched_any = false;
    for card in cache.due(cards, now) {
        let Some((query, kind)) = release::search_query(card) else { continue };
        match search(http, base, key, &query, kind).await {
            Ok(results) => {
                let season = card.season_index.filter(|_| card.kind == flinch_archive::LibraryKind::Season);
                cache.insert(card.id.clone(), Searched::from_results(results, now, season));
                searched_any = true;
            }
            Err(error) => {
                let sentence = format!("Prowlarr search failed ({}): availability uses earlier searches only", cause(&error));
                problem(signals, sentence, &error, "");
                break;
            }
        }
    }
    if searched_any {
        super::write_state(path, &cache);
    }
    signals.releases = cache.releases(retention);
    signals.smallest_release = cache.smallest();
    signals.largest_release = cache.largest();
}

/// Prowlarr's results for one query across every indexer.
async fn search(http: &reqwest::Client, base: &str, key: &str, query: &str, kind: &str) -> Result<Vec<serde_json::Value>> {
    let url = reqwest::Url::parse_with_params(&format!("{base}/api/v1/search"), [("query", query), ("type", kind), ("limit", "100")])
        .context("Prowlarr url")?;
    array(fetch_json(http, url.as_str(), key).await?)
}

/// The usenet servers' retention in days (0 = unlimited); `None` leaves
/// usenet availability unjudged.
async fn sab_retention(http: &reqwest::Client, args: &Args, signals: &mut Signals) -> Option<u32> {
    let Some((base, key)) = configured(&args.sabnzbd_url, &args.sabnzbd_key) else {
        signals.problems.push("SABnzbd not configured: usenet retention unknown".to_owned());
        return None;
    };
    // SABnzbd takes its key in the query only, so the url is never logged as is.
    let params = [("mode", "get_config"), ("section", "servers"), ("output", "json"), ("apikey", key)];
    let config = match reqwest::Url::parse_with_params(&format!("{base}/api"), params).context("SABnzbd url") {
        Ok(url) => fetch_json(http, url.as_str(), key).await,
        Err(error) => Err(error),
    };
    let config = match config {
        Ok(config) => config,
        Err(error) => {
            let sentence = format!("SABnzbd unreachable ({}): usenet retention unknown", cause(&error));
            problem(signals, sentence, &error, key);
            return None;
        }
    };
    if let Some(refusal) = config.get("error").and_then(serde_json::Value::as_str) {
        signals.problems.push(format!("SABnzbd refused the request ({refusal}): usenet retention unknown"));
        return None;
    }
    let retention = release::parse_retention(&config);
    if retention.is_none() {
        signals.problems.push("SABnzbd has no enabled server with a retention: usenet retention unknown".to_owned());
    }
    retention
}

/// The trimmed base url and key, when both are set.
fn configured<'a>(url: &'a str, key: &'a str) -> Option<(&'a str, &'a str)> {
    let url = url.trim().trim_end_matches('/');
    (!url.is_empty() && !key.is_empty()).then_some((url, key))
}

fn array(value: serde_json::Value) -> Result<Vec<serde_json::Value>> {
    serde_json::from_value(value).context("answer is not an array")
}

/// Why a read failed, in a few words: the HTTP status `fetch_json` reports,
/// no answer at all, or an answer FLINCH could not read.
fn cause(error: &anyhow::Error) -> String {
    let status = error.to_string().split_once(": HTTP ").and_then(|(_, rest)| rest.get(..3).map(str::to_owned));
    match status.filter(|code| code.bytes().all(|byte| byte.is_ascii_digit())) {
        Some(code) => format!("HTTP {code}"),
        None if error.chain().any(|source| source.is::<reqwest::Error>()) => "no answer".to_owned(),
        None => "unexpected answer".to_owned(),
    }
}

/// Record a degraded source: the sentence for the operator, the detail for
/// the log, with `secret` (a key that travels in a url) masked.
fn problem(signals: &mut Signals, sentence: String, error: &anyhow::Error, secret: &str) {
    let mut detail = format!("{error:#}");
    if !secret.is_empty() {
        detail = detail.replace(secret, "<key>");
    }
    eprintln!("[flinch-arrd] {sentence}: {detail}");
    signals.problems.push(sentence);
}
