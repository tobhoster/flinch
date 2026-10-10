//! A person's own copy: the Leaving Soon titles they requested and their
//! newsletter, on their own addresses, with links signed for them.
//!
//! Delivery reuses the channels' machinery: each address files its sent
//! keys and hourly posts in `notify.json` under its own name
//! (`household:<person>:<address>`), so every event reaches each address
//! once, within the same hourly budget, and a failed post is retried next
//! cycle. Shapes, from each service's documentation:
//! - ntfy: JSON to the server root with `topic`,
//!   <https://docs.ntfy.sh/publish/#publish-as-json>.
//! - Apprise API, stateless: `POST /notify/` with `urls` beside `title`,
//!   `body`, `type` and `format`, <https://github.com/caronc/apprise-api#api-details>;
//!   email is Apprise's `mailto://` URL with `to=`,
//!   <https://github.com/caronc/apprise/wiki/Notify_email>.

use super::outbox::{env, ORDER, PERSONAL};
use super::recipients::Recipient;
use super::render;
use super::{ChannelKind, Event, EventKind, Notifier, NotifyError, SendReport};
use serde_json::Value;

/// One person and the events meant for them.
#[derive(Debug, Clone, PartialEq)]
pub struct Personal {
    pub recipient: Recipient,
    pub events: Vec<Event>,
}

/// One of a person's own addresses.
enum Address<'a> {
    Ntfy(&'a str),
    Apprise(&'a str),
    Email(&'a str),
}

impl Address<'_> {
    fn label(&self) -> &'static str {
        match self {
            Self::Ntfy(_) => "ntfy",
            Self::Apprise(_) => "apprise",
            Self::Email(_) => "email",
        }
    }
}

fn addresses(person: &Recipient) -> Vec<Address<'_>> {
    let ntfy = person.ntfy_topic.as_deref().map(Address::Ntfy);
    let apprise = person.apprise_env.as_deref().map(Address::Apprise);
    let email = person.email.as_deref().map(Address::Email);
    [ntfy, apprise, email].into_iter().flatten().collect()
}

/// A variable's trimmed value, or the error naming the variable.
fn from_env(variable: &str) -> Result<String, NotifyError> {
    env(variable)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| NotifyError::MissingEnv { variable: variable.to_string() })
}

/// The Apprise URL that emails `to`: the sender's `mailto://` URL with `to=`.
fn mailto(sender: &str, to: &str) -> String {
    let separator = if sender.contains('?') { '&' } else { '?' };
    format!("{sender}{separator}to={to}")
}

impl Notifier<'_> {
    /// Deliver each person's events to each of their addresses, once, within
    /// the hourly budget. People without an address are skipped.
    pub async fn send_personal(&self, people: &[Personal], now: u64) -> SendReport {
        let mut report = SendReport::default();
        if people.iter().all(|person| person.events.is_empty() || !person.recipient.has_address()) {
            return report;
        }
        let mut state = self.read();
        for person in people {
            for address in addresses(&person.recipient) {
                let name = format!("{PERSONAL}{}:{}", person.recipient.name.to_lowercase(), address.label());
                let sent = state.sent.entry(name.clone()).or_default();
                let pending: Vec<&Event> = person.events.iter().filter(|event| !sent.contains_key(&event.key())).collect();
                if pending.is_empty() {
                    continue;
                }
                let posts = state.posts.entry(name).or_default();
                for group in ORDER {
                    let events: Vec<&Event> = pending.iter().copied().filter(|event| event.kind() == group).collect();
                    if events.is_empty() {
                        continue;
                    }
                    if posts.len() >= self.config.max_per_hour as usize {
                        report.deferred += events.len();
                        continue;
                    }
                    let mut message = render::message(group, &events, self.config, &[]);
                    if group == EventKind::LeavingSoon {
                        message.title = format!("{} you asked for", message.title);
                    }
                    posts.push(now);
                    match self.deliver(&address, &message).await {
                        Ok(()) => {
                            report.messages += 1;
                            sent.extend(events.iter().map(|event| (event.key(), now)));
                        }
                        Err(error) => {
                            report.failures.push(format!("{} ({}): {error}", person.recipient.name, address.label()));
                            break;
                        }
                    }
                }
            }
        }
        if let Err(error) = self.write(&state) {
            report.failures.push(format!("{} not written, sent events may repeat: {error}", super::STATE_FILE));
        }
        report
    }

    async fn deliver(&self, address: &Address<'_>, message: &render::Message) -> Result<(), NotifyError> {
        let household = &self.config.household;
        let ui_url = self.config.ui_url.as_str();
        let apprise = || format!("{}/notify/", household.apprise_api.trim_end_matches('/'));
        let (url, mut body, token) = match address {
            Address::Ntfy(topic) => {
                let root = format!("{}/", household.ntfy_server.trim_end_matches('/'));
                let token = (!household.ntfy_token_env.is_empty()).then(|| from_env(&household.ntfy_token_env)).transpose()?;
                let mut body = render::body(ChannelKind::Ntfy, message, ui_url);
                body["topic"] = Value::from(*topic);
                (root, body, token)
            }
            Address::Apprise(variable) => (apprise(), render::body(ChannelKind::Apprise, message, ui_url), Some(from_env(variable)?)),
            Address::Email(to) => {
                let sender = from_env(&household.email_url_env)?;
                (apprise(), render::body(ChannelKind::Apprise, message, ui_url), Some(mailto(&sender, to)))
            }
        };
        let bearer = match address {
            Address::Ntfy(_) => token,
            // Apprise's stateless endpoint takes the target URLs in the body.
            Address::Apprise(_) | Address::Email(_) => {
                body["urls"] = Value::from(token.unwrap_or_default());
                None
            }
        };
        let mut request = self.http.post(url).json(&body);
        if let Some(token) = bearer {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.map_err(|error| NotifyError::Transport(error.without_url()))?;
        // A redirect is never followed (the client refuses), so it lands here.
        if !response.status().is_success() {
            return Err(NotifyError::Http { status: response.status().as_u16() });
        }
        Ok(())
    }
}
