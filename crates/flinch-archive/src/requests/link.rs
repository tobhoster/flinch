//! No-login links: a household member who got a notification can keep a
//! title, or ask for it to go, with one click and no account.
//!
//! A link is `{ui_url}/r/{token}`, where the token is the request itself
//! (`card id`, action, expiry, who it was addressed to) and an
//! HMAC-SHA256 over it under `FLINCH_WEB_LINK_SECRET`. Nothing is stored
//! when a link is made, so the daemon signs and the web verifies with the
//! same secret, and a token cannot be widened: changing the card, the
//! action or the expiry breaks the MAC. Each token does one thing and
//! expires; spending it twice finds the request it already made
//! ([`super::RequestBook::record`]), so a replay changes nothing.
//!
//! HMAC comes from `ring` (already in the tree through rustls), the
//! encoding from `base64` (already in the tree too): neither is written
//! here, and `ring::hmac::verify` compares in constant time.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::hmac;
use serde::{Deserialize, Serialize};

/// The environment variable holding the secret, in both containers.
pub const SECRET_ENV: &str = "FLINCH_WEB_LINK_SECRET";
/// Shorter secrets are refused: 32 random bytes is what HMAC-SHA256 keys need.
pub const MIN_SECRET_LEN: usize = 32;
const DAY: u64 = 86_400;
/// Bound on what any link may be valid for, whatever the settings say.
pub const MAX_LINK_DAYS: u64 = 90;
/// Domain separation: a MAC over a link is never a MAC over anything else.
const CONTEXT: &str = "flinch-link-v1";
/// Longest token accepted before decoding: a card id, an action, a number
/// and a name never need more.
const MAX_TOKEN_LEN: usize = 600;

/// What a link does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Keep the title: a pin, at once (keeping is always safe).
    Keep,
    /// Ask for it to go: waits for the admin's approval.
    Remove,
}

impl Action {
    fn word(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Remove => "remove",
        }
    }
}

/// The request a token carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub card_id: String,
    pub action: Action,
    /// Unix seconds; the link is dead from then on.
    pub expires: u64,
    /// Who it was addressed to: a Seerr name, or `household` for a shared
    /// channel or the Plex shelf.
    pub by: String,
}

/// Why a token was refused. No variant echoes the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    #[error("this link is not valid")]
    Invalid,
    #[error("this link has expired")]
    Expired,
}

/// The signing key. Never printed: `Debug` names only its length.
pub struct LinkSecret {
    key: hmac::Key,
    len: usize,
}

impl std::fmt::Debug for LinkSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LinkSecret({} bytes)", self.len)
    }
}

impl LinkSecret {
    /// `None` for a secret shorter than [`MIN_SECRET_LEN`] bytes.
    pub fn new(secret: &[u8]) -> Option<Self> {
        (secret.len() >= MIN_SECRET_LEN).then(|| Self { key: hmac::Key::new(hmac::HMAC_SHA256, secret), len: secret.len() })
    }

    /// The secret from [`SECRET_ENV`]; `None` while unset or too short.
    pub fn from_env() -> Option<Self> {
        std::env::var(SECRET_ENV).ok().and_then(|value| Self::new(value.trim().as_bytes()))
    }

    fn signed(link: &Link) -> String {
        // `by` goes last and loses its line breaks, so the fields split back
        // exactly as they were joined.
        let by: String = link.by.chars().filter(|c| *c != '\n').collect();
        format!("{}\n{}\n{}\n{by}", link.card_id, link.action.word(), link.expires)
    }

    /// The token for `link`: `base64url(fields).base64url(mac)`.
    pub fn sign(&self, link: &Link) -> String {
        let payload = Self::signed(link);
        let tag = hmac::sign(&self.key, format!("{CONTEXT}\n{payload}").as_bytes());
        format!("{}.{}", URL_SAFE_NO_PAD.encode(payload), URL_SAFE_NO_PAD.encode(tag.as_ref()))
    }

    /// The link a token carries, if its MAC holds and it has not expired at
    /// `now`. The MAC is checked before anything in the payload is believed.
    pub fn verify(&self, token: &str, now: u64) -> Result<Link, LinkError> {
        if token.len() > MAX_TOKEN_LEN {
            return Err(LinkError::Invalid);
        }
        let (payload, tag) = token.split_once('.').ok_or(LinkError::Invalid)?;
        let payload = URL_SAFE_NO_PAD.decode(payload).map_err(|_| LinkError::Invalid)?;
        let tag = URL_SAFE_NO_PAD.decode(tag).map_err(|_| LinkError::Invalid)?;
        let mut signed = format!("{CONTEXT}\n").into_bytes();
        signed.extend_from_slice(&payload);
        hmac::verify(&self.key, &signed, &tag).map_err(|_| LinkError::Invalid)?;
        let payload = String::from_utf8(payload).map_err(|_| LinkError::Invalid)?;
        let mut fields = payload.splitn(4, '\n');
        let (Some(card_id), Some(action), Some(expires), Some(by)) = (fields.next(), fields.next(), fields.next(), fields.next()) else {
            return Err(LinkError::Invalid);
        };
        let action = match action {
            "keep" => Action::Keep,
            "remove" => Action::Remove,
            _ => return Err(LinkError::Invalid),
        };
        let expires: u64 = expires.parse().map_err(|_| LinkError::Invalid)?;
        if now >= expires {
            return Err(LinkError::Expired);
        }
        Ok(Link { card_id: card_id.to_string(), action, expires, by: by.to_string() })
    }
}

/// A token's stable id: the first 12 bytes of its MAC. It names the request
/// the token made, so a second click finds it instead of making another.
pub fn token_id(token: &str) -> Option<String> {
    let (_, tag) = token.split_once('.')?;
    let tag = URL_SAFE_NO_PAD.decode(tag).ok()?;
    (tag.len() >= 12).then(|| URL_SAFE_NO_PAD.encode(&tag[..12]))
}

/// When a link made at `now` dies: a day after the title's leave date when
/// it has one (nothing is left to keep afterwards), else `days` from now.
/// Rounded up to a whole UTC day so the same link comes out all day long:
/// a shelf summary is rewritten only when its items change.
pub fn expiry(leaves_at: Option<u64>, now: u64, days: u32) -> u64 {
    let horizon = now + u64::from(days).min(MAX_LINK_DAYS) * DAY;
    let at = leaves_at.map_or(horizon, |leaves| (leaves + DAY).min(now + MAX_LINK_DAYS * DAY));
    at.div_ceil(DAY) * DAY
}

/// `{ui_url}/r/{token}`; `None` without an address to send people to.
pub fn url(ui_url: &str, token: &str) -> Option<String> {
    let base = ui_url.trim().trim_end_matches('/');
    (base.starts_with("http://") || base.starts_with("https://")).then(|| format!("{base}/r/{token}"))
}

/// Makes the links of one cycle: the secret and where FLINCH is reached.
pub struct Signer<'a> {
    pub secret: &'a LinkSecret,
    pub ui_url: &'a str,
    /// Link lifetime when the title has no leave date.
    pub days: u32,
    pub now: u64,
}

impl Signer<'_> {
    /// The link doing `action` on `card_id` for `by`.
    pub fn link(&self, card_id: &str, action: Action, leaves_at: Option<u64>, by: &str) -> Option<String> {
        let link = Link { card_id: card_id.to_string(), action, expires: expiry(leaves_at, self.now, self.days), by: by.to_string() };
        url(self.ui_url, &self.secret.sign(&link))
    }
}
