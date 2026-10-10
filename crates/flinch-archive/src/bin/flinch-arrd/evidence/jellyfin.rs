//! Jellyfin/Emby as a watch source for one cycle: read every user, join by
//! catalogue id, and record whether the whole record was read.

use super::super::state_dir;
use flinch_archive::jellyfin::{CardPlays, JellyfinClient, JellyfinConfig, JellyfinEvidence, PLAYS_FILE};
use flinch_archive::plan::candidates::Plays;
use flinch_archive::plex::WatchTarget;
use flinch_archive::viewers::IgnoredViewers;

/// What the cycle learned from Jellyfin.
#[derive(Default)]
pub(in super::super) struct Jellyfin {
    pub(in super::super) configured: bool,
    /// Every user and every user's whole library was read.
    pub(in super::super) complete: bool,
    pub(in super::super) evidence: JellyfinEvidence,
}

pub(super) async fn gather(config: &JellyfinConfig, targets: &[WatchTarget], ignored: &IgnoredViewers) -> Jellyfin {
    if !config.enabled() {
        return Jellyfin::default();
    }
    let kind = config.kind.label();
    let Some(key) = config.api_key() else {
        eprintln!("[flinch-arrd] {kind}: a URL is set but no API key (token or api_key_env): not read, evidence incomplete");
        return Jellyfin { configured: true, ..Jellyfin::default() };
    };
    let read = match JellyfinClient::new(&super::with_scheme(config.url.trim()), &key, config.kind) {
        Ok(client) => client.read().await,
        Err(error) => Err(error),
    };
    let mut read = match read {
        Ok(read) => read,
        Err(error) => {
            eprintln!("[flinch-arrd] {kind}: read failed, no {kind} evidence this cycle: {error}");
            return Jellyfin { configured: true, ..Jellyfin::default() };
        }
    };
    for problem in &read.problems {
        eprintln!("[flinch-arrd] {kind}: {problem}");
    }
    // An ignored user's state counts as no play; the read stays as complete.
    let set_aside = ignored.jellyfin(&mut read);
    if set_aside > 0 {
        println!("[flinch-arrd] {kind}: {set_aside} ignored viewer(s) set aside");
    }
    let evidence = flinch_archive::jellyfin::evidence(targets, &read);
    println!(
        "[flinch-arrd] {kind}: {} user(s) read{} · {} of {} targets joined by catalogue id · {} with watch state",
        read.users.len(),
        if read.complete { "" } else { " (incomplete: no watch state claimed)" },
        evidence.resolved,
        targets.len(),
        evidence.entries.len(),
    );
    // The fitter reads the same plays the daemon used.
    match serde_json::to_vec(&evidence.plays) {
        Ok(bytes) => {
            if let Err(error) = flinch_archive::persist::replace(state_dir().join(PLAYS_FILE).as_path(), &bytes) {
                eprintln!("[flinch-arrd] {PLAYS_FILE} write failed: {error}");
            }
        }
        Err(error) => eprintln!("[flinch-arrd] {PLAYS_FILE} encode failed: {error}"),
    }
    Jellyfin { configured: true, complete: read.complete, evidence }
}

/// An item's plays from the Plex/Tautulli log with its Jellyfin plays added,
/// oldest first.
pub(in super::super) fn with_plays<'a>((mut item, mut audience): Plays<'a>, jellyfin: Option<&'a CardPlays>) -> Plays<'a> {
    if let Some(plays) = jellyfin {
        item.extend(&plays.item);
        audience.extend(&plays.audience);
        item.sort_by_key(|play| play.epoch);
        audience.sort_by_key(|play| play.epoch);
    }
    (item, audience)
}
