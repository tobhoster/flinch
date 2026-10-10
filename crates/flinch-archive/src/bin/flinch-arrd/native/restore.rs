//! The undo queue: the web UI drops `restore/<card id>` for a native delete of
//! the last 30 days; here it is monitored and searched again in its *arr (or
//! a removed movie added back), read back, and marked restored. A dry run
//! prints the writes and leaves the request queued for a live run.

use super::act::Actor;
use super::state_dir;
use flinch_archive::executor::state::{pending_restores, restore_dir};
use flinch_archive::executor::{NativeState, NativeStatus, RestoreTarget};

pub(super) async fn process(actor: &Actor<'_>, state: &mut NativeState, status: &mut NativeStatus, now: u64) {
    let dir = state_dir();
    for id in pending_restores(&dir) {
        let request = restore_dir(&dir).join(&id);
        let Some(item) = state.restorable(&id, now).cloned() else {
            status.problems.push(format!("undo of {id}: not a FLINCH delete of the last 30 days, request dropped"));
            std::fs::remove_file(&request).ok();
            continue;
        };
        // The card id names the instance that deleted it.
        let instance = flinch_archive::ids::ArrRef::card(&item.id).map_or("", |card| card.instance);
        let result = match &item.target {
            RestoreTarget::Radarr { .. } => match actor.radarr(instance) {
                Some(radarr) => radarr.restore(&item.target).await,
                None => Err(unconfigured(instance)),
            },
            RestoreTarget::Sonarr { series_id, season } => match actor.sonarr(instance) {
                Some(sonarr) => sonarr.restore(*series_id, *season).await,
                None => Err(unconfigured(instance)),
            },
        };
        match result {
            Ok(true) => {
                println!("[flinch-arrd] native: {} restored: monitored and searched again", item.title);
                if let Some(entry) = state.deleted.iter_mut().rev().find(|entry| entry.id == item.id && entry.deleted_at == item.deleted_at)
                {
                    entry.restored_at = Some(now);
                }
                std::fs::remove_file(&request).ok();
            }
            Ok(false) => status.simulated += 1,
            Err(error) => {
                status.problems.push(format!("undo of {}: {error}; request dropped, ask again", item.title));
                std::fs::remove_file(&request).ok();
            }
        }
    }
}

fn unconfigured(instance: &str) -> flinch_archive::executor::ExecutorError {
    flinch_archive::executor::ExecutorError::NotApplied { endpoint: "arr", detail: format!("no configured instance named {instance:?}") }
}
