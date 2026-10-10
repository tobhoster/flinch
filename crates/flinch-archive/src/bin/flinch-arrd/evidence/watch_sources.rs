//! Tracearr and Trakt as watch sources for one cycle: read each configured
//! source, join its plays by catalogue id, and record whether every account
//! was read to the end. Read-only: nothing is ever written to either service.

use super::super::state_dir;
use flinch_archive::jellyfin::CardPlays;
use flinch_archive::plex::WatchTarget;
use flinch_archive::viewers::IgnoredViewers;
use flinch_archive::watch::WatchEntry;
use flinch_archive::watch_sources::{
    env_secret, SourceError, SourceKind, SourceRead, SourceStatus, TracearrClient, TraktClient, WatchSourceConfig, WatchSourcesStatus,
    PLAYS_FILE, STATUS_FILE,
};
use std::collections::HashMap;

/// What the cycle learned from every configured source.
#[derive(Default)]
pub(in super::super) struct Sources {
    pub(in super::super) configured: bool,
    /// Every source, every account, read to the end.
    pub(in super::super) complete: bool,
    pub(in super::super) entries: HashMap<String, WatchEntry>,
    pub(in super::super) plays: HashMap<String, CardPlays>,
}

pub(super) async fn gather(configs: &[WatchSourceConfig], targets: &[WatchTarget], ignored: &IgnoredViewers, now: u64) -> Sources {
    let dir = state_dir();
    if configs.is_empty() {
        // A source the operator removed must not keep feeding the fitter or
        // the status page.
        for file in [PLAYS_FILE, STATUS_FILE] {
            if let Err(error) = std::fs::remove_file(dir.join(file)) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("[flinch-arrd] {file} removal failed: {error}");
                }
            }
        }
        return Sources::default();
    }
    let mut out = Sources { configured: true, complete: true, ..Sources::default() };
    let mut status = WatchSourcesStatus::default();
    for config in configs {
        let label = config.display();
        let mut read = match read(config).await {
            Ok(read) => read,
            Err(problem) => SourceRead { problems: vec![problem], ..SourceRead::default() },
        };
        // An ignored viewer's plays count as no play; coverage stays as read.
        let set_aside = ignored.source(&mut read);
        if set_aside > 0 {
            println!("[flinch-arrd] {label}: {set_aside} play(s) by ignored viewers set aside");
        }
        for problem in &read.problems {
            eprintln!("[flinch-arrd] {label}: {problem}");
        }
        let found = flinch_archive::watch_sources::evidence(targets, &read, config.kind, config.retention_days, now);
        println!(
            "[flinch-arrd] {label}: {} account(s) · {} play(s){} · {} target(s) joined by catalogue id · {} never played",
            read.accounts,
            read.plays.len(),
            if read.complete { "" } else { " (incomplete: no absence claimed, never-played reclaim held)" },
            found.joined,
            found.never_played,
        );
        out.complete &= read.complete;
        flinch_archive::plex::history::merge_history(&mut out.entries, found.entries);
        for (id, plays) in found.plays {
            let merged = out.plays.entry(id).or_default();
            merged.item.extend(plays.item);
            merged.audience.extend(plays.audience);
        }
        status.sources.push(SourceStatus {
            source: label,
            kind: config.kind,
            complete: read.complete,
            accounts: read.accounts,
            plays: read.plays.len(),
            joined: found.joined,
            never_played: found.never_played,
            problems: read.problems,
        });
    }
    for plays in out.plays.values_mut() {
        plays.item.sort_by_key(|play| play.epoch);
        plays.audience.sort_by_key(|play| play.epoch);
    }
    // The fitter reads the same plays the daemon used; publish reads the status.
    persist(&dir, PLAYS_FILE, serde_json::to_vec(&out.plays));
    persist(&dir, STATUS_FILE, serde_json::to_vec(&status));
    out
}

fn persist(dir: &std::path::Path, file: &str, bytes: serde_json::Result<Vec<u8>>) {
    match bytes {
        Ok(bytes) => {
            if let Err(error) = flinch_archive::persist::replace(dir.join(file).as_path(), &bytes) {
                eprintln!("[flinch-arrd] {file} write failed: {error}");
            }
        }
        Err(error) => eprintln!("[flinch-arrd] {file} encode failed: {error}"),
    }
}

/// One source's read; `Err` is the one-line reason nothing was read (never
/// a URL or a secret).
async fn read(config: &WatchSourceConfig) -> Result<SourceRead, String> {
    let token = env_secret(&config.token_env).ok_or_else(|| format!("{} is unset: not read", config.token_env.trim()))?;
    let failed = |error: SourceError| format!("read failed: {error}");
    match config.kind {
        SourceKind::Tracearr => {
            TracearrClient::new(&super::with_scheme(&config.base()), &token).map_err(failed)?.read().await.map_err(failed)
        }
        SourceKind::Trakt => {
            let client_id =
                env_secret(&config.client_id_env).ok_or_else(|| format!("{} is unset: not read", config.client_id_env.trim()))?;
            let viewer = match config.name.trim() {
                "" => config.token_env.trim(),
                name => name,
            };
            TraktClient::new(&super::with_scheme(&config.base()), &token, &client_id).map_err(failed)?.read(viewer).await.map_err(failed)
        }
    }
}
