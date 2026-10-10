//! Several Radarr and Sonarr instances: HD beside 4K, anime beside the rest.
//!
//! The default instance of each app is the one every install has: its URL and
//! key from `RADARR_URL`/`RADARR_API_KEY` (or the flags), its card ids
//! unqualified ([`crate::ids`]), its per-app settings (archive root, compact
//! profile, TRaSH instance) where they always were. Each extra instance is
//! named, and comes from `settings.json` `instances` or from numbered env
//! vars (`RADARR_2_URL`, `RADARR_2_API_KEY`, optional `RADARR_2_NAME`). Its
//! key only ever comes from the environment: the settings file sits on a
//! volume the web UI writes and never carries a secret.

use crate::capacity::App;
use crate::ids::{instance_key, valid_instance_name};
use serde::{Deserialize, Serialize};

/// The most extra instances per app: a guard on a hand-edited file.
pub const MAX_EXTRA: usize = 8;

/// One extra instance as `settings.json` names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceConfig {
    pub app: App,
    /// Part of every card id of the instance (`radarr@<name>-7`): renaming
    /// one makes its items new to FLINCH.
    pub name: String,
    pub url: String,
    /// The environment variable that holds its API key.
    pub key_env: String,
    /// Where its items archive to ([`crate::archive`]); blank: never.
    #[serde(default)]
    pub archive_root: String,
    /// Its compact quality profile's name, when no TRaSH sync manages one
    /// ([`crate::quality::act`]); blank: none.
    #[serde(default)]
    pub compact_profile: String,
    /// Where the browser opens it (`https://radarr4k.example.com`); blank: a
    /// sibling host of FLINCH's named `<app>-<name>`.
    #[serde(default)]
    pub public_url: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("{0}")]
pub struct InvalidInstances(pub &'static str);

/// What the Settings page enforces: names that fit an id, unique per app, a
/// URL, and an env var name (never the key itself).
pub fn validate(instances: &[InstanceConfig]) -> Result<(), InvalidInstances> {
    for app in [App::Radarr, App::Sonarr] {
        if instances.iter().filter(|instance| instance.app == app).count() > MAX_EXTRA {
            return Err(InvalidInstances("instances: at most 8 extra instances per app"));
        }
    }
    for (index, instance) in instances.iter().enumerate() {
        if !valid_instance_name(&instance.name) {
            return Err(InvalidInstances("instances: a name is 1-24 of a-z, 0-9 and _"));
        }
        if instances[..index].iter().any(|other| other.app == instance.app && other.name == instance.name) {
            return Err(InvalidInstances("instances: two instances of one app share a name"));
        }
        let url = instance.url.trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(InvalidInstances("instances: url must start with http:// or https://"));
        }
        let public = instance.public_url.trim();
        if !(public.is_empty() || public.starts_with("http://") || public.starts_with("https://")) {
            return Err(InvalidInstances("instances: public_url must start with http:// or https://"));
        }
        let env = instance.key_env.as_str();
        if env.is_empty() || env.len() > 64 || !env.bytes().all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_') {
            return Err(InvalidInstances("instances: key_env names an environment variable (A-Z, 0-9, _), never the key itself"));
        }
    }
    Ok(())
}

/// One instance the daemon talks to, key resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub app: App,
    /// Empty for the default instance.
    pub name: String,
    /// Without a trailing `/`.
    pub base: String,
    pub key: String,
    pub archive_root: String,
    pub compact_profile: String,
    pub public_url: String,
}

impl Connection {
    /// `radarr`, `radarr@4k`: the prefix of its card ids, and its name in logs.
    pub fn key(&self) -> String {
        instance_key(self.app, &self.name)
    }

    pub fn is_default(&self) -> bool {
        self.name.is_empty()
    }

    /// `Radarr`, or `Radarr 4k` for a named instance: its name in messages.
    pub fn label(&self) -> String {
        let app = match self.app {
            App::Radarr => "Radarr",
            App::Sonarr => "Sonarr",
        };
        if self.is_default() {
            app.to_string()
        } else {
            format!("{app} {}", self.name)
        }
    }

    /// What the UI lists: everything but the key.
    pub fn view(&self) -> InstanceView {
        InstanceView { app: self.app, name: self.name.clone(), url: self.base.clone(), public_url: self.public_url.clone() }
    }
}

/// An instance as the status publishes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceView {
    pub app: App,
    /// Empty for the default instance.
    #[serde(default)]
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub public_url: String,
}

/// Every instance: the two defaults first (as the caller resolved them from
/// flags and env), then each configured one, then each numbered env one. An
/// extra whose key is missing, or that repeats a name, is left out and said;
/// it never stops the defaults.
pub fn resolve(
    defaults: [Connection; 2],
    configured: &[InstanceConfig],
    env: impl Fn(&str) -> Option<String>,
    env_names: impl IntoIterator<Item = String>,
) -> (Vec<Connection>, Vec<String>) {
    let (mut out, mut problems) = (defaults.to_vec(), Vec::new());
    let present = |value: Option<String>| value.map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
    let add = |connection: Connection, out: &mut Vec<Connection>, problems: &mut Vec<String>| {
        if out.iter().any(|other| other.app == connection.app && other.name == connection.name) {
            problems.push(format!("{}: a second instance with this name is ignored", connection.key()));
        } else {
            out.push(connection);
        }
    };
    for config in configured {
        let Some(key) = present(env(&config.key_env)) else {
            problems.push(format!("{}: {} is unset, the instance is not read", instance_key(config.app, &config.name), config.key_env));
            continue;
        };
        let connection = Connection {
            app: config.app,
            name: config.name.clone(),
            base: config.url.trim().trim_end_matches('/').to_string(),
            key,
            archive_root: config.archive_root.trim().to_string(),
            compact_profile: config.compact_profile.trim().to_string(),
            public_url: config.public_url.trim().trim_end_matches('/').to_string(),
        };
        add(connection, &mut out, &mut problems);
    }
    let mut numbered: Vec<(App, u32)> = env_names.into_iter().filter_map(|name| numbered_url(&name)).collect();
    numbered.sort_unstable();
    numbered.dedup();
    for (app, number) in numbered {
        let prefix = format!("{}_{number}", app.label().to_ascii_uppercase());
        let name = present(env(&format!("{prefix}_NAME"))).unwrap_or_else(|| number.to_string());
        if !valid_instance_name(&name) {
            problems.push(format!("{prefix}_NAME: {name:?} is not 1-24 of a-z, 0-9 and _; the instance is not read"));
            continue;
        }
        let (Some(base), Some(key)) = (present(env(&format!("{prefix}_URL"))), present(env(&format!("{prefix}_API_KEY")))) else {
            problems.push(format!("{prefix}: _URL and _API_KEY are both needed; the instance is not read"));
            continue;
        };
        let connection = Connection {
            app,
            name,
            base: base.trim_end_matches('/').to_string(),
            key,
            archive_root: present(env(&format!("{prefix}_ARCHIVE_ROOT"))).unwrap_or_default(),
            compact_profile: present(env(&format!("{prefix}_COMPACT_PROFILE"))).unwrap_or_default(),
            public_url: present(env(&format!("{prefix}_PUBLIC_URL"))).map(|url| url.trim_end_matches('/').to_string()).unwrap_or_default(),
        };
        add(connection, &mut out, &mut problems);
    }
    (out, problems)
}

/// `RADARR_2_URL` → (Radarr, 2). The unnumbered default is not one.
fn numbered_url(name: &str) -> Option<(App, u32)> {
    let (app, rest) = match (name.strip_prefix("RADARR_"), name.strip_prefix("SONARR_")) {
        (Some(rest), _) => (App::Radarr, rest),
        (None, Some(rest)) => (App::Sonarr, rest),
        (None, None) => return None,
    };
    let number = rest.strip_suffix("_URL")?;
    let number: u32 = (!number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())).then(|| number.parse().ok()).flatten()?;
    Some((app, number))
}

#[cfg(test)]
mod tests;
