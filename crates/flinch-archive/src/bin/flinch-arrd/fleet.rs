//! The fleet watch ([`flinch_archive::fleet`]) at the top of each cycle,
//! before anything that could fail the cycle: every Radarr and Sonarr
//! instance and Prowlarr read, the requests the web UI queued under
//! `fleet-requests/` carried out, then the automatic clean-up the settings
//! allow, each write printed instead of sent in a dry run. Publishes
//! `arr-fleet-status.json` and remembers queue progress in
//! `fleet-queue.json`. A request is taken once: one refused, failed or only
//! printed is recorded and dropped, so the operator asks again.

use super::signals::configured;
use super::{read_state, state_dir, write_state, Args};
use flinch_archive::daemon::RuntimeSettings;
use flinch_archive::fleet::{self, act, requests, Fleet, FleetStatus, QueueMemory};

/// One cycle's fleet watch; the problems it saw, in words.
pub(super) async fn run(http: &reqwest::Client, args: &Args, settings: &RuntimeSettings, dry_run: bool) -> Vec<String> {
    if !settings.fleet.enabled {
        return Vec::new();
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let dir = state_dir();
    let fleet = Fleet { http, arrs: &args.arrs, prowlarr: configured(&args.prowlarr_url, &args.prowlarr_key), dry_run };
    let memory_path = dir.join(fleet::QUEUE_MEMORY_FILE);
    let mut memory: QueueMemory = read_state(&memory_path);
    let mut status = fleet::observe(&fleet, &mut memory, &settings.fleet, now).await;
    write_state(&memory_path, &memory);

    let mut actions = Vec::new();
    for (path, request) in requests::pending(&dir) {
        actions.push(act::requested(&fleet, &status, &request, now).await);
        if let Err(error) = std::fs::remove_file(&path) {
            eprintln!("[flinch-arrd] fleet: {} not removed, it is taken again next cycle: {error}", path.display());
        }
    }
    actions.extend(act::auto(&fleet, &status, &settings.fleet, now).await);
    for action in &actions {
        println!("[flinch-arrd] fleet: {:?} {}: {}", action.kind, action.target, action.result);
    }

    let status_path = dir.join(fleet::STATUS_FILE);
    let earlier: FleetStatus = read_state(&status_path);
    status.record(earlier.actions, actions);
    write_state(&status_path, &status);
    status.problems()
}
