//! The duplicate finder's reads and the resolver's removals
//! ([`flinch_archive::dupes`]).
//!
//! Finding runs before the hand-off, because a confirmed choice settles a
//! several-copies hold there. Acting runs after the plan, once quality advice
//! (for the recommendation) and pins and partway viewers (which keep every
//! copy) are known. Reads, all live in a dry run too:
//! - Plex `Media` come with the section listings the evidence already read.
//! - Radarr `GET /api/v3/moviefile?movieId=` for movies Plex holds more than
//!   once, so a Plex copy can be matched to the file Radarr tracks.
//! - Radarr and Sonarr `GET /api/v3/rootfolder` (`unmappedFolders`: folders
//!   under a root no item owns) and `GET /api/v3/manualimport?folder=` (the
//!   video files in one, with sizes); https://radarr.video/docs/api/,
//!   https://sonarr.tv/docs/api/.

use super::arr_write::ArrWriter;
use super::{state_dir, Args};
use flinch_archive::arr::ArrMovie;
use flinch_archive::capacity::App;
use flinch_archive::dupes::decisions::{self, Acted, ActedLog, Decisions, Outcome};
use flinch_archive::dupes::group::{self, MovieIn, PlexVersion};
use flinch_archive::dupes::pick::Advice;
use flinch_archive::dupes::remove::{self, ArrInstance, Clients};
use flinch_archive::dupes::{ArrFile, DupesConfig, DupesStatus, Group, UnownedFolder};
use flinch_archive::maintainerr::SyncItem;
use flinch_archive::plan::MediaCandidate;
use flinch_archive::plex::{PlayKeys, PlexMetadata};
use std::collections::{HashMap, HashSet};

/// What finding read.
pub(super) struct Found {
    groups: Vec<Group>,
    unowned: Vec<UnownedFolder>,
    problems: Vec<String>,
}

/// Plex's movie inputs.
pub(super) struct Plex<'a> {
    pub(super) content: &'a HashMap<String, PlexMetadata>,
    pub(super) play_keys: &'a HashMap<String, PlayKeys>,
    pub(super) history: &'a [PlexMetadata],
}

/// Find this cycle's duplicates, with the operator's choices attached, and
/// settle the several-copies hold of every item with a confirmed choice;
/// `None` while the finder is off.
pub(super) async fn find(
    http: &reqwest::Client,
    args: &Args,
    config: &DupesConfig,
    movies: &[ArrMovie],
    plex: Plex<'_>,
    sync_items: &mut [SyncItem],
) -> Option<Found> {
    if !config.enabled {
        return None;
    }
    let mut problems = Vec::new();
    let mut plays: HashMap<&str, u32> = HashMap::new();
    for row in plex.history.iter().filter(|row| !row.rating_key.is_empty()) {
        *plays.entry(row.rating_key.as_str()).or_default() += 1;
    }
    let versions = |key: &String| {
        plex.content.get(key).map(|row| PlexVersion::of(row, plays.get(key.as_str()).copied().unwrap_or(0))).unwrap_or_default()
    };
    // Each movie's files are its own instance's.
    let radarr = |movie: &ArrMovie| args.arr(App::Radarr, &movie.instance).and_then(|arr| ArrWriter::new(http, arr, true));
    let mut covered: HashSet<&str> = HashSet::new();
    let mut inputs = Vec::new();
    for movie in movies {
        let card = movie.card_id();
        let Some(PlayKeys::Movie { rating_keys, .. }) = plex.play_keys.get(&card) else { continue };
        covered.extend(rating_keys.iter().map(String::as_str));
        let copies: Vec<PlexVersion> = rating_keys.iter().flat_map(&versions).collect();
        if copies.len() < 2 {
            continue;
        }
        let arr = match radarr(movie) {
            Some(radarr) => arr_files(&radarr, movie).await.unwrap_or_else(|error| {
                problems.push(format!("{} files of {}: {error:#}", radarr.name(), movie.title));
                Vec::new()
            }),
            None => Vec::new(),
        };
        inputs.push(MovieIn { card_id: Some(card), title: movie.title.clone(), year: movie.year, tmdb: movie.tmdb_id, plex: copies, arr });
    }
    // Movies only Plex holds: their copies merge by tmdb across sections. A
    // row of a movie Radarr has but the GUID join did not reach is left out:
    // without Radarr's files beside it, a Plex removal could hit Radarr's.
    let radarr_tmdb: HashSet<u32> = movies.iter().filter_map(|movie| movie.tmdb_id).collect();
    let plex_only = |row: &&PlexMetadata| {
        row.media_type == "movie"
            && !covered.contains(row.rating_key.as_str())
            && !row.external_ids().tmdb.is_some_and(|tmdb| radarr_tmdb.contains(&tmdb))
    };
    for row in plex.content.values().filter(plex_only) {
        inputs.push(MovieIn {
            card_id: None,
            title: row.title.clone(),
            year: row.year,
            tmdb: row.external_ids().tmdb,
            plex: versions(&row.rating_key),
            arr: Vec::new(),
        });
    }
    let mut groups = group::group(inputs, config.prefer, &HashMap::new());
    match Decisions::read(&state_dir()) {
        Ok(decisions) => decisions.attach(&mut groups),
        Err(error) => problems.push(error.to_string()),
    }
    let settled = decisions::settle(sync_items, &groups);
    if settled > 0 {
        println!("[flinch-arrd] dupes: {settled} item(s) with several Plex copies settled by the operator's choice");
    }
    let unowned = unowned(http, args, config, &mut problems).await;
    Some(Found { groups, unowned, problems })
}

/// The files `movie`'s Radarr instance tracks for it.
async fn arr_files(radarr: &ArrWriter<'_>, movie: &ArrMovie) -> anyhow::Result<Vec<ArrFile>> {
    let files = radarr.get(&format!("/api/v3/moviefile?movieId={}", movie.id)).await?;
    let instance = flinch_archive::ids::instance_key(App::Radarr, radarr.instance());
    Ok(files
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|file| {
            Some(ArrFile {
                instance: instance.clone(),
                movie_id: movie.id,
                file_id: u32::try_from(file["id"].as_u64()?).ok()?,
                path: file["path"].as_str()?.to_string(),
                bytes: file["size"].as_u64().unwrap_or(0),
                quality: file["quality"]["quality"]["name"].as_str().map(str::to_string),
            })
        })
        .collect())
}

/// Folders under every instance's roots that no item owns, largest first.
async fn unowned(http: &reqwest::Client, args: &Args, config: &DupesConfig, problems: &mut Vec<String>) -> Vec<UnownedFolder> {
    let min_bytes = u64::from(config.unowned_min_gib) << 30;
    let mut found = Vec::new();
    for arr in &args.arrs {
        let Some(reader) = ArrWriter::new(http, arr, true) else { continue };
        let roots = match reader.get("/api/v3/rootfolder").await {
            Ok(roots) => roots,
            Err(error) => {
                problems.push(format!("{} root folders: {error:#}", reader.name()));
                continue;
            }
        };
        let folders: Vec<String> = roots
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|root| root["unmappedFolders"].as_array().cloned().unwrap_or_default())
            .filter_map(|folder| folder["path"].as_str().map(str::to_string))
            .take(config.unowned_max_folders as usize)
            .collect();
        for path in folders {
            let query = format!("/api/v3/manualimport?folder={}&filterExistingFiles=false", encode(&path));
            match reader.get(&query).await {
                Ok(files) => {
                    let files: Vec<u64> = files.as_array().into_iter().flatten().map(|file| file["size"].as_u64().unwrap_or(0)).collect();
                    let bytes = files.iter().sum();
                    if bytes >= min_bytes {
                        found.push(UnownedFolder { app: reader.name().to_string(), path, bytes, files: files.len() as u32 });
                    }
                }
                Err(error) => problems.push(format!("{} files in {path}: {error:#}", reader.name())),
            }
        }
    }
    found.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    found
}

/// Percent-encode a query value (RFC 3986 unreserved characters stay).
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (byte as char).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// Recommend with quality advice, act on confirmed choices when the
/// operator turned acting on, and build `status.dupes`; `None` while the
/// finder is off.
pub(super) async fn act(
    http: &reqwest::Client,
    args: &Args,
    settings: &flinch_archive::daemon::RuntimeSettings,
    found: Option<Found>,
    candidates: &[MediaCandidate],
    dry_run: bool,
    now: u64,
) -> Option<DupesStatus> {
    let config = &settings.dupes;
    let Found { mut groups, unowned, problems } = found?;
    let advice: HashMap<String, Advice> =
        candidates.iter().map(|candidate| (candidate.id.clone(), Advice::of(&candidate.quality.action))).collect();
    group::advise(&mut groups, config.prefer, &advice);
    let protected: HashSet<&str> = candidates.iter().filter(|candidate| candidate.protect).map(|candidate| candidate.id.as_str()).collect();
    let mut log = ActedLog::read(&state_dir());
    if config.act {
        // Named as their files are: `radarr`, `radarr@4k`.
        let keys: Vec<String> = args.arrs_of(App::Radarr).map(|arr| arr.key()).collect();
        let arrs: Vec<ArrInstance> =
            args.arrs_of(App::Radarr).zip(&keys).map(|(arr, key)| ArrInstance { name: key, base: &arr.base, key: &arr.key }).collect();
        let plex = (!settings.plex_url.trim().is_empty() && !settings.plex_token.is_empty())
            .then_some((settings.plex_url.trim(), settings.plex_token.as_str()));
        let clients = Clients { http, plex, arrs: &arrs, dry_run };
        let mut budget = config.max_per_run;
        for group in &mut groups {
            resolve(&clients, group, &protected, &mut log, &mut budget, now).await;
        }
        if let Err(error) = log.write(&state_dir()) {
            eprintln!("[flinch-arrd] dupes-acted.json write failed: {error}");
        }
    }
    Some(DupesStatus { act: config.act, dry_run, groups, unowned, acted: log.entries, problems })
}

/// Remove `group`'s redundant copies, within the cycle's budget.
async fn resolve(clients: &Clients<'_>, group: &mut Group, protected: &HashSet<&str>, log: &mut ActedLog, budget: &mut u32, now: u64) {
    let Some(decision) = group.confirmed().cloned() else { return };
    let is_protected = group.card_id.as_deref().is_some_and(|card| protected.contains(card));
    let removals = match remove::plan(group, is_protected) {
        Ok(removals) => removals.into_iter().map(|(copy, removal)| (copy.clone(), removal)).collect::<Vec<_>>(),
        Err(held) => {
            group.held = Some(held);
            return;
        }
    };
    let Some(keep) = group.copy(&decision.keep).cloned() else { return };
    for (copy, removal) in removals {
        if log.settled_since(&copy.id, decision.decided_at_unix) {
            continue;
        }
        if *budget == 0 {
            group.held.get_or_insert_with(|| "this cycle's removal cap is reached; the next cycle continues".to_string());
            return;
        }
        *budget -= 1;
        let (outcome, detail) = match clients.remove(&keep, &removal).await {
            Ok(true) => (Outcome::Removed, String::new()),
            Ok(false) => (Outcome::Simulated, "dry run: printed, not sent".to_string()),
            Err(error) => (Outcome::Failed, error.to_string()),
        };
        println!("[flinch-arrd] dupes: {} copy {} of {}: {outcome:?} {detail}", group.title, copy.id, group.id);
        let failed = outcome == Outcome::Failed;
        log.push(Acted {
            group: group.id.clone(),
            copy: copy.id.clone(),
            title: group.title.clone(),
            bytes: copy.bytes,
            outcome,
            detail,
            at_unix: now,
        });
        if failed {
            group.held = Some("a removal failed (see the list below); choose again to retry".to_string());
            return;
        }
    }
}
