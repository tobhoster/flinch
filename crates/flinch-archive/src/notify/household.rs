//! `notify.household`: messages to the people who asked for a title, and the
//! weekly newsletter. Off by default; the shared channels are unchanged.
//!
//! Who gets what is in [`super::recipients`]; the newsletter's words in
//! [`super::newsletter`]; the per-person posts in [`super::personal`].
//! Every personal address that carries a secret (an Apprise URL with its
//! service's token, an email sender's SMTP login) is named by environment
//! variable, as the shared channels' URLs are. Email goes through Apprise's
//! `mailto://`, so FLINCH carries no SMTP client.

use super::{is_env_name, is_http, InvalidNotifyConfig};
use serde::{Deserialize, Serialize};

/// Most recipient overrides.
pub const MAX_RECIPIENTS: usize = 100;

/// One household member's addresses, matched to a Seerr user by any of
/// their names (display name, username, Plex or Jellyfin name, email).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecipientOverride {
    pub user: String,
    /// A Discord user id: shared Discord messages about their titles
    /// mention them (`<@id>`).
    pub discord_id: String,
    /// Their own topic on [`HouseholdNotify::ntfy_server`].
    pub ntfy_topic: String,
    /// Environment variable holding their Apprise URL(s) (any service
    /// Apprise supports), posted to [`HouseholdNotify::apprise_api`].
    pub apprise_env: String,
    /// Their email instead of Seerr's.
    pub email: String,
    /// Gets the weekly newsletter on their own addresses.
    pub newsletter: bool,
    /// Nothing personal: no message, no mention.
    pub muted: bool,
}

impl Default for RecipientOverride {
    fn default() -> Self {
        Self {
            user: String::new(),
            discord_id: String::new(),
            ntfy_topic: String::new(),
            apprise_env: String::new(),
            email: String::new(),
            newsletter: true,
            muted: false,
        }
    }
}

/// Requester-addressed notifications and the newsletter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HouseholdNotify {
    /// Tell requesters about their titles, on their own addresses and by
    /// Discord mention.
    pub enabled: bool,
    /// Never name who requested a title in a shared message or the
    /// newsletter, and mention nobody; personal messages still go.
    pub hide_requester: bool,
    /// An ntfy server root (`https://ntfy.sh/`) for per-person topics.
    pub ntfy_server: String,
    /// Environment variable with an ntfy access token for those topics.
    pub ntfy_token_env: String,
    /// The Apprise API (`http://apprise:8000`): per-person Apprise URLs and
    /// email are posted to its stateless `/notify/` endpoint.
    pub apprise_api: String,
    /// Environment variable with an Apprise `mailto://` (or `mailtos://`)
    /// URL holding the sender's SMTP login; each email adds `to=`.
    pub email_url_env: String,
    /// Email every Seerr user with an email address, not only overrides.
    pub email_seerr_users: bool,
    /// Send the weekly newsletter (to channels subscribed to `newsletter`,
    /// and to recipients who take it).
    pub newsletter: bool,
    /// 0 Monday … 6 Sunday.
    pub newsletter_weekday: u8,
    pub newsletter_hour_utc: u8,
    pub recipients: Vec<RecipientOverride>,
}

impl Default for HouseholdNotify {
    fn default() -> Self {
        Self {
            enabled: false,
            hide_requester: false,
            ntfy_server: String::new(),
            ntfy_token_env: String::new(),
            apprise_api: String::new(),
            email_url_env: String::new(),
            email_seerr_users: false,
            newsletter: false,
            newsletter_weekday: 4,
            newsletter_hour_utc: 17,
            recipients: Vec::new(),
        }
    }
}

fn is_topic(topic: &str) -> bool {
    (1..=64).contains(&topic.len()) && topic.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// An address Apprise's `to=` takes as it is: no spaces, no URL syntax.
pub(super) fn is_email(email: &str) -> bool {
    let plain =
        !email.is_empty() && email.len() <= 254 && !email.bytes().any(|byte| byte.is_ascii_whitespace() || b"&?#/,;<>\"'".contains(&byte));
    plain && email.split_once('@').is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'))
}

impl HouseholdNotify {
    pub(super) fn validate(&self) -> Result<(), InvalidNotifyConfig> {
        let fail = |words: &'static str| Err(InvalidNotifyConfig(words));
        if !self.ntfy_server.is_empty() && !is_http(&self.ntfy_server) {
            return fail("the household ntfy server must start with http:// or https://");
        }
        if !self.apprise_api.is_empty() && !is_http(&self.apprise_api) {
            return fail("the household Apprise API must start with http:// or https://");
        }
        for env in [&self.ntfy_token_env, &self.email_url_env] {
            if !env.is_empty() && !is_env_name(env) {
                return fail("household variables must be environment variable names (A-Z, 0-9, _)");
            }
        }
        if (self.email_seerr_users || !self.email_url_env.is_empty()) && (self.email_url_env.is_empty() || self.apprise_api.is_empty()) {
            return fail("household email needs both the Apprise API and the variable holding the mailto:// URL");
        }
        if self.newsletter_weekday > 6 || self.newsletter_hour_utc > 23 {
            return fail("the newsletter day must be 0 (Monday) to 6 and its hour 0 to 23 (UTC)");
        }
        if self.recipients.len() > MAX_RECIPIENTS {
            return fail("at most 100 household recipients");
        }
        for (index, recipient) in self.recipients.iter().enumerate() {
            self.check(recipient)?;
            if self.recipients[..index].iter().any(|other| other.user.eq_ignore_ascii_case(&recipient.user)) {
                return fail("every household recipient needs their own user");
            }
        }
        Ok(())
    }

    fn check(&self, recipient: &RecipientOverride) -> Result<(), InvalidNotifyConfig> {
        let fail = |words: &'static str| Err(InvalidNotifyConfig(words));
        let user = recipient.user.trim();
        if user.is_empty() || user.chars().count() > 80 || user != recipient.user {
            return fail("a household recipient needs a user of 1 to 80 characters, without outer spaces");
        }
        let id = &recipient.discord_id;
        if !id.is_empty() && !((17..=20).contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_digit())) {
            return fail("a Discord user id is 17 to 20 digits");
        }
        if !recipient.ntfy_topic.is_empty() {
            if !is_topic(&recipient.ntfy_topic) {
                return fail("an ntfy topic is 1 to 64 letters, digits, - or _");
            }
            if self.ntfy_server.is_empty() {
                return fail("a recipient's ntfy topic needs the household ntfy server");
            }
        }
        if !recipient.apprise_env.is_empty() {
            if !is_env_name(&recipient.apprise_env) {
                return fail("a recipient's Apprise variable must be an environment variable name (A-Z, 0-9, _)");
            }
            if self.apprise_api.is_empty() {
                return fail("a recipient's Apprise URL needs the household Apprise API");
            }
        }
        if !recipient.email.is_empty() {
            if !is_email(&recipient.email) {
                return fail("a recipient's email must be a plain address (name@example.org)");
            }
            if self.email_url_env.is_empty() || self.apprise_api.is_empty() {
                return fail("a recipient's email needs the Apprise API and the mailto:// variable");
            }
        }
        Ok(())
    }
}
