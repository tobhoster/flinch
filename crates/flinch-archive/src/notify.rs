//! Telling the household what FLINCH did, without being asked: an item joined
//! Leaving Soon (with a link to keep it), an item left the library, a problem
//! persisted, a daily digest for the admin and a weekly newsletter for the
//! household; and, with `notify.household`, telling the people who requested
//! a title on their own addresses (`notify/personal.rs`).
//!
//! Nothing here decides anything. Events are facts the daemon already
//! published; this module only delivers them, at most once per channel
//! (sent keys persist in `notify.json`), within a per-channel hourly budget,
//! and never with a credential in a log or an error: a webhook URL carries its
//! secret in the path, so channels name an environment variable instead, and
//! every error drops the URL. Delivery that fails is retried next cycle, since
//! a key is stored only once its channel answered 2xx.

pub mod household;
pub mod newsletter;
mod outbox;
mod personal;
pub mod recipients;
mod render;
#[cfg(test)]
mod tests;

pub use household::{HouseholdNotify, RecipientOverride};
pub use newsletter::{NewsItem, Newsletter};
pub use outbox::{
    answer_test, ChannelOutcome, Notifier, SendReport, TestRequest, TestResult, STATE_FILE, TEST_REQUEST_FILE, TEST_RESULT_FILE,
};
pub use personal::Personal;
pub use recipients::Recipient;

use serde::{Deserialize, Serialize};

/// One thing worth telling. Every variant carries what makes it unique, so a
/// re-sent cycle never repeats it ([`Event::key`]). Other parts of FLINCH emit
/// events only through these variants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// An item joined the Leaving Soon window: shown in the media server and
    /// deleted at `leaves_at` unless someone plays or keeps it first.
    LeavingSoon {
        id: String,
        title: String,
        bytes: u64,
        handed_at: u64,
        leaves_at: Option<u64>,
        /// Who requested it in Seerr: named (unless `hide_requester`),
        /// mentioned and told on their own addresses.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        requesters: Vec<String>,
        /// Its TMDB poster ([`recipients::tmdb_poster`]).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        poster: Option<String>,
        /// The no-login keep link ([`crate::requests::link`]); without one
        /// the Keep link opens the item in FLINCH's UI.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        keep_url: Option<String>,
    },
    /// An item was handed to its deleter, or has left the library.
    Deleted { id: String, title: String, bytes: u64, handed_at: u64, stage: Stage },
    /// A problem seen on more than [`PERSIST_CYCLES`]` - 1` cycles in a row,
    /// since `since` (unix seconds of the first).
    Problem { key: String, message: String, since: u64 },
    /// The day's summary; `day` counts UTC days since the epoch.
    Digest(Digest),
    /// The household's weekly newsletter.
    Newsletter(Newsletter),
}

/// How far a deletion has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Handed to the deleter (Maintainerr's delete collection, or the native
    /// executor's queue): it goes on the deleter's next run.
    Handed,
    /// Gone from Radarr or Sonarr.
    Gone,
}

/// The daily summary: each disk's forecast, what left, what goes next.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Digest {
    pub day: u64,
    pub dry_run: bool,
    pub disks: Vec<DiskLine>,
    /// Items, and their bytes, that left the library in the last 24 hours.
    pub freed_items: usize,
    pub freed_bytes: u64,
    /// The plan's first picks, in eviction order.
    pub top: Vec<TopCandidate>,
}

/// One governed volume's forecast.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiskLine {
    pub volume: String,
    pub used_bytes: u64,
    pub capacity_bytes: u64,
    pub projected_used_bytes: u64,
    pub window_days: u32,
    /// What the volume must free; 0 while healthy.
    pub target_reclaim_bytes: u64,
    pub emergency: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopCandidate {
    pub id: String,
    pub title: String,
    pub bytes: u64,
    pub regret: f64,
}

/// A problem has to be seen on this many consecutive cycles before it is
/// told: one failed read is weather, three are a fault.
pub const PERSIST_CYCLES: u32 = 3;

/// The event kinds a channel subscribes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    LeavingSoon,
    Deleted,
    Problem,
    Digest,
    /// The household's weekly newsletter. Not in a new channel's defaults:
    /// it is for the household, the other kinds for the admin.
    Newsletter,
}

impl Event {
    pub fn kind(&self) -> EventKind {
        match self {
            Self::LeavingSoon { .. } => EventKind::LeavingSoon,
            Self::Deleted { .. } => EventKind::Deleted,
            Self::Problem { .. } => EventKind::Problem,
            Self::Digest(_) => EventKind::Digest,
            Self::Newsletter(_) => EventKind::Newsletter,
        }
    }

    /// What makes this event the same one again. The hand-over time is part
    /// of an item's key, so an item taken back and handed over again later is
    /// told again; a problem that clears and returns has a new `since`.
    pub fn key(&self) -> String {
        match self {
            Self::LeavingSoon { id, handed_at, .. } => format!("leaving_soon:{id}:{handed_at}"),
            Self::Deleted { id, handed_at, stage, .. } => {
                let stage = match stage {
                    Stage::Handed => "handed",
                    Stage::Gone => "gone",
                };
                format!("deleted:{stage}:{id}:{handed_at}")
            }
            Self::Problem { key, since, .. } => format!("problem:{key}:{since}"),
            Self::Digest(digest) => format!("digest:{}", digest.day),
            Self::Newsletter(newsletter) => format!("newsletter:{}", newsletter.week),
        }
    }
}

/// Where a channel delivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    /// A Discord channel webhook URL.
    Discord,
    /// An ntfy topic URL (`https://ntfy.sh/<topic>`).
    Ntfy,
    /// Any URL that takes a JSON POST of the events themselves.
    Webhook,
    /// An Apprise API notify endpoint (`http://apprise:8000/notify/<key>`).
    Apprise,
}

/// One delivery target (`settings.json` `notify.channels[]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelConfig {
    /// The operator's label; also what sent keys are filed under, so it must
    /// be unique.
    pub name: String,
    pub kind: ChannelKind,
    /// Environment variable of the daemon holding the URL. Use it for every
    /// URL with a secret in it (a Discord webhook always has one).
    pub url_env: String,
    /// The URL itself, stored in `settings.json` and shown in Settings: only
    /// for URLs without a secret (a self-hosted ntfy topic, an internal hook).
    pub url: String,
    /// Environment variable holding a bearer token (ntfy access token, or the
    /// webhook's own). Blank sends none.
    pub token_env: String,
    pub events: Vec<EventKind>,
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: ChannelKind::Webhook,
            url_env: String::new(),
            url: String::new(),
            token_env: String::new(),
            events: vec![EventKind::LeavingSoon, EventKind::Deleted, EventKind::Problem, EventKind::Digest],
        }
    }
}

/// Notifications as the operator configured them (`settings.json` `notify`).
/// No channel, no notification: the feature is off by default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyConfig {
    pub channels: Vec<ChannelConfig>,
    /// The UTC hour from which the day's digest is sent.
    pub digest_hour_utc: u8,
    /// The address the household opens FLINCH at, for the Keep links. Blank
    /// sends no link.
    pub ui_url: String,
    /// Most messages one channel gets per rolling hour. Events past it wait
    /// for the next cycle, so a flood is delayed, never dropped.
    pub max_per_hour: u32,
    /// Messages to requesters and the weekly newsletter; off by default.
    pub household: HouseholdNotify,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self { channels: Vec::new(), digest_hour_utc: 8, ui_url: String::new(), max_per_hour: 12, household: HouseholdNotify::default() }
    }
}

/// Bounds of [`NotifyConfig::max_per_hour`].
pub const MAX_PER_HOUR: u32 = 120;
/// Most channels one install configures.
pub const MAX_CHANNELS: usize = 10;

/// A [`NotifyConfig`] outside its bounds; the message names the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidNotifyConfig(pub &'static str);

impl NotifyConfig {
    pub fn validate(&self) -> Result<(), InvalidNotifyConfig> {
        if self.digest_hour_utc > 23 {
            return Err(InvalidNotifyConfig("the digest hour must be 0 to 23 (UTC)"));
        }
        if !(1..=MAX_PER_HOUR).contains(&self.max_per_hour) {
            return Err(InvalidNotifyConfig("notifications per channel and hour must be 1 to 120"));
        }
        if !self.ui_url.is_empty() && !is_http(&self.ui_url) {
            return Err(InvalidNotifyConfig("the FLINCH address for Keep links must start with http:// or https://"));
        }
        if self.channels.len() > MAX_CHANNELS {
            return Err(InvalidNotifyConfig("at most 10 notification channels"));
        }
        for (index, channel) in self.channels.iter().enumerate() {
            channel.validate()?;
            if self.channels[..index].iter().any(|other| other.name == channel.name) {
                return Err(InvalidNotifyConfig("every notification channel needs its own name"));
            }
        }
        self.household.validate()
    }
}

impl ChannelConfig {
    fn validate(&self) -> Result<(), InvalidNotifyConfig> {
        let name = self.name.trim();
        if name.is_empty() || name.len() > 40 || name != self.name {
            return Err(InvalidNotifyConfig("a notification channel needs a name of 1 to 40 characters, without outer spaces"));
        }
        match (self.url_env.is_empty(), self.url.is_empty()) {
            (true, true) => return Err(InvalidNotifyConfig("a notification channel needs a URL or the environment variable holding it")),
            (false, false) => return Err(InvalidNotifyConfig("a notification channel takes a URL or an environment variable, not both")),
            (false, true) if !is_env_name(&self.url_env) => {
                return Err(InvalidNotifyConfig("the URL variable must be an environment variable name (A-Z, 0-9, _)"))
            }
            (true, false) => check_url(self.kind, &self.url)?,
            _ => {}
        }
        if !self.token_env.is_empty() {
            if !matches!(self.kind, ChannelKind::Ntfy | ChannelKind::Webhook) {
                return Err(InvalidNotifyConfig("a token variable applies to ntfy and webhook channels only"));
            }
            if !is_env_name(&self.token_env) {
                return Err(InvalidNotifyConfig("the token variable must be an environment variable name (A-Z, 0-9, _)"));
            }
        }
        if self.events.is_empty() {
            return Err(InvalidNotifyConfig("a notification channel needs at least one event"));
        }
        Ok(())
    }

    /// The URL to post to: the variable's value, or the stored URL. The error
    /// names the variable, never a value.
    pub fn resolve_url(&self, env: impl Fn(&str) -> Option<String>) -> Result<String, NotifyError> {
        if self.url_env.is_empty() {
            return Ok(self.url.clone());
        }
        let url = env(&self.url_env).map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
        let url = url.ok_or_else(|| NotifyError::MissingEnv { variable: self.url_env.clone() })?;
        check_url(self.kind, &url).map_err(|problem| NotifyError::BadUrl { variable: self.url_env.clone(), problem: problem.0 })?;
        Ok(url)
    }
}

fn is_http(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

fn is_env_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn check_url(kind: ChannelKind, url: &str) -> Result<(), InvalidNotifyConfig> {
    if !is_http(url) || reqwest::Url::parse(url).is_err() {
        return Err(InvalidNotifyConfig("a notification URL must be a full http:// or https:// address"));
    }
    if kind == ChannelKind::Ntfy && render::ntfy_topic(url).is_none() {
        return Err(InvalidNotifyConfig("an ntfy URL must name its topic (https://ntfy.sh/<topic>)"));
    }
    Ok(())
}

/// Why a delivery failed. No variant carries a URL or a token.
#[derive(Debug, thiserror::Error)]
pub enum NotifyError {
    #[error("environment variable {variable} is not set")]
    MissingEnv { variable: String },
    #[error("environment variable {variable} holds no usable URL: {problem}")]
    BadUrl { variable: String, problem: &'static str },
    #[error("answered HTTP {status}")]
    Http { status: u16 },
    #[error("request failed: {0}")]
    Transport(#[source] reqwest::Error),
}

/// "YYYY-MM-DD" of a UTC day (days since the epoch): Howard Hinnant's
/// `civil_from_days`, <https://howardhinnant.github.io/date_algorithms.html>.
pub fn utc_date(day: u64) -> String {
    let z = day as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}
