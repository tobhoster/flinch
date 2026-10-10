//! The daemon's connections: flags, env, and the *arr instances of a cycle.
//!
//! Flags are the defaults and env overrides them (a container's knob). The
//! default Radarr and Sonarr come from here; extra instances from
//! `settings.json` `instances` and numbered env vars, resolved each cycle
//! ([`flinch_archive::arr::instances`]) so adding one needs no restart. Every
//! *arr call picks its instance here, by app and name.

use clap::Parser;
use flinch_archive::arr::instances::{self, Connection};
use flinch_archive::capacity::App;
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(name = "flinch-arrd", about = "Continuous *arr swarm loop")]
pub(super) struct Args {
    #[arg(long, default_value = "http://radarr:7878")]
    pub(super) radarr_url: String,
    #[arg(long, default_value = "")]
    pub(super) radarr_key: String,
    #[arg(long, default_value = "http://sonarr:8989")]
    pub(super) sonarr_url: String,
    #[arg(long, default_value = "")]
    pub(super) sonarr_key: String,
    #[arg(long, default_value = "http://maintainerr:5555")]
    pub(super) maintainerr_url: String,
    #[arg(long, default_value = "")]
    pub(super) maintainerr_key: String,
    /// Seerr (Overseerr/Jellyseerr): requests and watchlists. Empty disables.
    #[arg(long, default_value = "")]
    pub(super) seerr_url: String,
    #[arg(long, default_value = "")]
    pub(super) seerr_key: String,
    /// Prowlarr: seeders per title, for how hard a re-download would be.
    #[arg(long, default_value = "")]
    pub(super) prowlarr_url: String,
    #[arg(long, default_value = "")]
    pub(super) prowlarr_key: String,
    /// SABnzbd: the usenet servers' retention.
    #[arg(long, default_value = "")]
    pub(super) sabnzbd_url: String,
    #[arg(long, default_value = "")]
    pub(super) sabnzbd_key: String,
    /// JSON export of media-server watch state (see examples/watch-state.json).
    #[arg(long)]
    pub(super) watch_state: Option<PathBuf>,
    /// Run once and exit instead of looping.
    #[arg(long)]
    pub(super) once: bool,
    // Grace runs, per-run caps and collections are operator settings
    // (state/settings.json, set from the UI); they have no flags.
    #[arg(long, default_value_t = 3600)]
    pub(super) interval_s: u64,
    /// Every *arr instance this cycle talks to, defaults first; filled by
    /// [`Args::for_cycle`], never by a flag.
    #[arg(skip)]
    pub(super) arrs: Vec<Connection>,
}

/// Flag value unless an environment variable sets it. Environment wins when the
/// flag would; for a container the flags are the defaults and env is the knob.
fn env_or(env: &str, flag: String) -> String {
    match std::env::var(env) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => flag,
    }
}

impl Args {
    pub(super) fn resolve(&mut self) {
        self.radarr_url = env_or("RADARR_URL", self.radarr_url.clone());
        self.sonarr_url = env_or("SONARR_URL", self.sonarr_url.clone());
        self.maintainerr_url = env_or("MAINTAINERR_URL", self.maintainerr_url.clone());
        self.seerr_url = env_or("SEERR_URL", self.seerr_url.clone());
        self.prowlarr_url = env_or("PROWLARR_URL", self.prowlarr_url.clone());
        self.sabnzbd_url = env_or("SABNZBD_URL", self.sabnzbd_url.clone());
        for (key, env) in [
            (&mut self.radarr_key, "RADARR_API_KEY"),
            (&mut self.sonarr_key, "SONARR_API_KEY"),
            (&mut self.maintainerr_key, "MAINTAINERR_API_KEY"),
            (&mut self.seerr_key, "SEERR_API_KEY"),
            (&mut self.prowlarr_key, "PROWLARR_API_KEY"),
            (&mut self.sabnzbd_key, "SABNZBD_API_KEY"),
        ] {
            if key.is_empty() {
                *key = env_or(env, String::new());
            }
        }
        if let Ok(value) = std::env::var("FLINCH_WATCH_STATE") {
            if !value.is_empty() {
                self.watch_state = Some(PathBuf::from(value));
            }
        }
        if let Ok(value) = std::env::var("FLINCH_INTERVAL_S") {
            if let Ok(seconds) = value.parse() {
                self.interval_s = seconds;
            }
        }
        if std::env::var("FLINCH_ONCE").map(|v| v == "1" || v == "true").unwrap_or(false) {
            self.once = true;
        }
    }

    /// This cycle's copy, with every instance resolved: the two defaults,
    /// then `settings.instances`, then numbered env vars. A broken extra is
    /// logged and left out; the defaults always stay.
    pub(super) fn for_cycle(&self, configured: &[instances::InstanceConfig]) -> Self {
        let default = |app: App, url: &str, key: &str| Connection {
            app,
            name: String::new(),
            base: url.trim().trim_end_matches('/').to_string(),
            key: key.to_string(),
            archive_root: String::new(),
            compact_profile: String::new(),
            public_url: String::new(),
        };
        let defaults = [default(App::Radarr, &self.radarr_url, &self.radarr_key), default(App::Sonarr, &self.sonarr_url, &self.sonarr_key)];
        let env = |name: &str| std::env::var(name).ok();
        let (arrs, problems) = instances::resolve(defaults, configured, env, std::env::vars().map(|(name, _)| name));
        for problem in problems {
            eprintln!("[flinch-arrd] instances: {problem}");
        }
        Self { arrs, ..self.clone() }
    }

    /// Every instance of `app`, the default first.
    pub(super) fn arrs_of(&self, app: App) -> impl Iterator<Item = &Connection> {
        self.arrs.iter().filter(move |arr| arr.app == app)
    }

    /// The instance `instance` (empty: the default) of `app`.
    pub(super) fn arr(&self, app: App, instance: &str) -> Option<&Connection> {
        self.arrs.iter().find(|arr| arr.app == app && arr.name == instance)
    }
}
