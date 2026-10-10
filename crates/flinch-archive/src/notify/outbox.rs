//! Delivery: which events each channel has not had yet, how many messages it
//! may still get this hour, the post itself, and the test the Settings page
//! asks for. What was sent persists in `notify.json`; an unreadable file is
//! started over, so the failure mode is one repeat of what is still current,
//! never silence.

use super::render::{self, Message};
use super::{ChannelConfig, ChannelKind, Event, EventKind, NotifyConfig, NotifyError, Recipient, PERSIST_CYCLES};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// Sent keys, hourly posts and problem streaks, on the state volume.
pub const STATE_FILE: &str = "notify.json";
/// The web UI asks for a test by writing this; the daemon answers in
/// [`TEST_RESULT_FILE`]. Only the daemon holds the channels' secrets.
pub const TEST_REQUEST_FILE: &str = "notify-test.json";
pub const TEST_RESULT_FILE: &str = "notify-test-result.json";

/// How long a sent key is remembered: past the longest an item stays handed
/// over (90 days), so nothing current is told twice.
const SENT_TTL_SECS: u64 = 120 * 86_400;
const HOUR_SECS: u64 = 3_600;
/// The order kinds are posted in: what needs attention first.
pub(super) const ORDER: [EventKind; 5] =
    [EventKind::Problem, EventKind::LeavingSoon, EventKind::Deleted, EventKind::Digest, EventKind::Newsletter];
/// Sent keys and posts of a person's own addresses are filed under names
/// starting with this, beside the channels' ([`super::personal`]).
pub(super) const PERSONAL: &str = "household:";

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct State {
    /// Channel name → event key → unix seconds it was delivered.
    pub(super) sent: BTreeMap<String, BTreeMap<String, u64>>,
    /// Channel name → unix seconds of each post within the last hour.
    pub(super) posts: BTreeMap<String, Vec<u64>>,
    /// Problem key → its run of consecutive cycles.
    problems: BTreeMap<String, Streak>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Streak {
    since: u64,
    cycles: u32,
}

/// What one [`Notifier::send`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendReport {
    /// Messages channels accepted.
    pub messages: usize,
    /// Events held back by a channel's hourly budget; sent on a later cycle.
    pub deferred: usize,
    /// Per failed channel, why; retried next cycle. Never a URL or token.
    pub failures: Vec<String>,
}

/// One channel's answer to a test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelOutcome {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

/// A test the web UI asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestRequest {
    pub id: String,
    pub requested_at: u64,
}

/// The daemon's answer to a [`TestRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestResult {
    pub id: String,
    pub finished_at: u64,
    /// Set when the settings could not be read: nothing was sent.
    #[serde(default)]
    pub error: Option<String>,
    pub channels: Vec<ChannelOutcome>,
}

/// Delivers events to the configured channels. Built per cycle from the
/// settings in force; holds no state of its own beyond the file.
pub struct Notifier<'a> {
    pub(super) http: &'a reqwest::Client,
    pub(super) config: &'a NotifyConfig,
    dir: &'a Path,
    /// The household's recipients, for Discord mentions.
    pub(super) people: &'a [Recipient],
}

pub(super) fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs())
}

impl<'a> Notifier<'a> {
    /// `http` must follow no redirect: a webhook URL is its own credential.
    pub fn new(http: &'a reqwest::Client, config: &'a NotifyConfig, dir: &'a Path) -> Self {
        Self { http, config, dir, people: &[] }
    }

    /// The household's recipients ([`super::recipients::resolve`]), so
    /// shared Discord messages mention the people who asked for a title.
    pub fn with_recipients(self, people: &'a [Recipient]) -> Self {
        Self { people, ..self }
    }

    pub(super) fn read(&self) -> State {
        std::fs::read(self.dir.join(STATE_FILE)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }

    pub(super) fn write(&self, state: &State) -> std::io::Result<()> {
        crate::persist::replace(&self.dir.join(STATE_FILE), &serde_json::to_vec(state)?)
    }

    /// Advance the problem streaks by one cycle and return the problems seen
    /// on [`PERSIST_CYCLES`] cycles in a row. `seen` is (stable key, words).
    /// A cycle that failed early (`complete` false) saw only its failure: the
    /// other streaks are neither advanced nor broken by it.
    pub fn problems(&self, seen: &[(String, String)], complete: bool, now: u64) -> std::io::Result<Vec<Event>> {
        if self.config.channels.is_empty() {
            return Ok(Vec::new());
        }
        let seen: BTreeMap<&str, &str> = seen.iter().map(|(key, message)| (key.as_str(), message.as_str())).collect();
        let mut state = self.read();
        if complete {
            state.problems.retain(|key, _| seen.contains_key(key.as_str()));
        }
        let mut persisting = Vec::new();
        for (key, message) in seen {
            let streak = state.problems.entry(key.to_string()).or_insert(Streak { since: now, cycles: 0 });
            streak.cycles = streak.cycles.saturating_add(1);
            if streak.cycles >= PERSIST_CYCLES {
                persisting.push(Event::Problem { key: key.to_string(), message: message.to_string(), since: streak.since });
            }
        }
        self.write(&state)?;
        Ok(persisting)
    }

    /// Deliver `events` now; see [`Self::send_at`].
    pub async fn send(&self, events: &[Event]) -> SendReport {
        self.send_at(events, now()).await
    }

    /// Deliver to each channel the subscribed events it has not had, one
    /// message per kind (one post of them all for a webhook), within its
    /// hourly budget. A channel that fails is left for the next cycle.
    pub async fn send_at(&self, events: &[Event], now: u64) -> SendReport {
        let mut report = SendReport::default();
        if self.config.channels.is_empty() {
            return report;
        }
        let mut state = self.read();
        for sent in state.sent.values_mut() {
            sent.retain(|_, at| now.saturating_sub(*at) < SENT_TTL_SECS);
        }
        for posts in state.posts.values_mut() {
            posts.retain(|at| now.saturating_sub(*at) < HOUR_SECS);
        }
        for channel in &self.config.channels {
            let sent = state.sent.entry(channel.name.clone()).or_default();
            let pending: Vec<&Event> =
                events.iter().filter(|event| channel.events.contains(&event.kind()) && !sent.contains_key(&event.key())).collect();
            if pending.is_empty() {
                continue;
            }
            let url = match channel.resolve_url(env) {
                Ok(url) => url,
                Err(error) => {
                    report.failures.push(format!("{}: {error}", channel.name));
                    continue;
                }
            };
            let posts = state.posts.entry(channel.name.clone()).or_default();
            for (body, keys) in outgoing(channel.kind, &pending, self.config, self.people, &url) {
                if posts.len() >= self.config.max_per_hour as usize {
                    report.deferred += keys.len();
                    continue;
                }
                // A failed post counts too: a broken endpoint is not hammered.
                posts.push(now);
                match self.post(channel, &url, &body).await {
                    Ok(()) => {
                        report.messages += 1;
                        sent.extend(keys.into_iter().map(|key| (key, now)));
                    }
                    Err(error) => {
                        report.failures.push(format!("{}: {error}", channel.name));
                        break;
                    }
                }
            }
        }
        state.sent.retain(|name, _| name.starts_with(PERSONAL) || self.config.channels.iter().any(|channel| &channel.name == name));
        state.posts.retain(|_, posts| !posts.is_empty());
        if let Err(error) = self.write(&state) {
            report.failures.push(format!("{STATE_FILE} not written, sent events may repeat: {error}"));
        }
        report
    }

    /// Post a test message to every channel, outside the dedupe and the
    /// budget: the operator asked for exactly this one.
    pub async fn test(&self) -> Vec<ChannelOutcome> {
        let mut outcomes = Vec::new();
        for channel in &self.config.channels {
            let result = match channel.resolve_url(env) {
                Ok(url) => {
                    let message = render::test_message(&self.config.ui_url);
                    let body = render::body(channel.kind, &message, &self.config.ui_url);
                    self.post(channel, &url, &with_topic(channel.kind, body, &url)).await
                }
                Err(error) => Err(error),
            };
            let (ok, detail) = match result {
                Ok(()) => (true, "delivered".to_string()),
                Err(error) => (false, error.to_string()),
            };
            outcomes.push(ChannelOutcome { name: channel.name.clone(), ok, detail });
        }
        outcomes
    }

    async fn post(&self, channel: &ChannelConfig, url: &str, body: &Value) -> Result<(), NotifyError> {
        let target = match channel.kind {
            ChannelKind::Ntfy => render::ntfy_topic(url).map(|(root, _)| root).unwrap_or_else(|| url.to_string()),
            _ => url.to_string(),
        };
        let mut request = self.http.post(target).json(body);
        if !channel.token_env.is_empty() {
            let token = env(&channel.token_env).filter(|token| !token.trim().is_empty());
            let token = token.ok_or_else(|| NotifyError::MissingEnv { variable: channel.token_env.clone() })?;
            request = request.bearer_auth(token.trim());
        }
        let response = request.send().await.map_err(|error| NotifyError::Transport(error.without_url()))?;
        let status = response.status();
        // A redirect is never followed (the client refuses), so it lands here.
        if !status.is_success() {
            return Err(NotifyError::Http { status: status.as_u16() });
        }
        Ok(())
    }
}

/// The ntfy topic goes in the JSON body; other channels' bodies are as built.
fn with_topic(kind: ChannelKind, mut body: Value, url: &str) -> Value {
    if kind == ChannelKind::Ntfy {
        if let Some((_, topic)) = render::ntfy_topic(url) {
            body["topic"] = Value::from(topic);
        }
    }
    body
}

/// Each post for `pending`, with the event keys it delivers.
fn outgoing(kind: ChannelKind, pending: &[&Event], config: &NotifyConfig, people: &[Recipient], url: &str) -> Vec<(Value, Vec<String>)> {
    let ui_url = config.ui_url.as_str();
    if kind == ChannelKind::Webhook {
        return vec![(render::webhook_events(pending, ui_url), pending.iter().map(|event| event.key()).collect())];
    }
    ORDER
        .iter()
        .filter_map(|&group| {
            let events: Vec<&Event> = pending.iter().copied().filter(|event| event.kind() == group).collect();
            if events.is_empty() {
                return None;
            }
            let message: Message = render::message(group, &events, config, people);
            let body = with_topic(kind, render::body(kind, &message, ui_url), url);
            Some((body, events.iter().map(|event| event.key()).collect()))
        })
        .collect()
}

/// Answer a pending test request, if the web UI left one: read the saved
/// settings, post the test to every channel, write the result for the page.
/// `None` when nothing was asked.
pub async fn answer_test(http: &reqwest::Client, dir: &Path) -> Option<TestResult> {
    let path = dir.join(TEST_REQUEST_FILE);
    let bytes = std::fs::read(&path).ok()?;
    // Consumed first, so a test that fails to answer is not repeated forever.
    std::fs::remove_file(&path).ok()?;
    let request: TestRequest = serde_json::from_slice(&bytes).ok()?;
    let (error, channels) = match crate::daemon::read_settings(&dir.join("settings.json")) {
        Ok(settings) => (None, Notifier::new(http, &settings.notify, dir).test().await),
        Err(error) => (Some(error.to_string()), Vec::new()),
    };
    let result = TestResult { id: request.id, finished_at: now(), error, channels };
    // Unwritten, the page times out and says so; the daemon logs the result.
    let written = serde_json::to_vec(&result).map_err(std::io::Error::from);
    written.and_then(|bytes| crate::persist::replace(&dir.join(TEST_RESULT_FILE), &bytes)).ok();
    Some(result)
}
