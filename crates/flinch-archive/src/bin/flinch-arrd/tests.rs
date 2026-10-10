//! The daemon against fake *arrs and old state files: what a cycle reads
//! lands on the instance it came from, what it deletes goes to the instance
//! the card names, and an install upgraded from one Radarr and one Sonarr
//! reads its state on unchanged.

mod fake;
mod instances;
mod upgrade;

use super::Args;
use clap::Parser;
use flinch_archive::arr::instances::Connection;
use flinch_archive::capacity::App;
use std::path::PathBuf;
use tokio::sync::{Mutex, MutexGuard};

/// `FLINCH_STATE_DIR` belongs to the process: tests that read or write
/// state take turns, each in its own scratch directory.
static STATE_DIR: Mutex<()> = Mutex::const_new(());

/// A fresh state directory, the daemon's while the guard lives.
async fn state_dir(label: &str) -> (MutexGuard<'static, ()>, PathBuf) {
    let turn = STATE_DIR.lock().await;
    let dir = std::env::temp_dir().join(format!("flinch-arrd-{}-{label}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("a scratch state dir");
    std::env::set_var("FLINCH_STATE_DIR", &dir);
    (turn, dir)
}

/// The daemon's arguments with no flags, talking to `arrs`.
fn args(arrs: Vec<Connection>) -> Args {
    let mut args = Args::try_parse_from(["flinch-arrd"]).expect("the defaults parse");
    args.arrs = arrs;
    args
}

/// An instance at `base` whose key names it (`key-radarr@4k`), so a test
/// can tell which key reached which server.
fn connection(app: App, name: &str, base: &str) -> Connection {
    let key = format!("key-{}", flinch_archive::ids::instance_key(app, name));
    Connection {
        app,
        name: name.into(),
        base: base.into(),
        key,
        archive_root: String::new(),
        compact_profile: String::new(),
        public_url: String::new(),
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs())
}
