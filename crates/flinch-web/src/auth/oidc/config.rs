//! Single sign-on's settings, from `FLINCH_WEB_OIDC_*`, and who they let in.
//! Environment only, like the password login: the client secret is a secret,
//! and who may open FLINCH is not something the Settings page should change.

use super::{provider, CALLBACK_PATH};
use serde_json::{Map, Value};

/// Who may sign in: any one match lets the account in. All empty lets
/// nobody in.
#[derive(Debug, Default)]
pub struct Allowed {
    /// `sub` claims, exactly.
    pub subjects: Vec<Box<str>>,
    /// Lower-cased; only an `email` the provider marks `email_verified`.
    pub emails: Vec<Box<str>>,
    /// Values of `groups_claim`, exactly.
    pub groups: Vec<Box<str>>,
    pub groups_claim: Box<str>,
}

/// The provider and the client FLINCH is registered as. Never `Debug`: it
/// holds the client secret.
pub struct Config {
    /// Shown on the button: "Sign in with <name>".
    pub name: Box<str>,
    pub issuer: Box<str>,
    pub client_id: Box<str>,
    pub client_secret: Option<Box<str>>,
    pub redirect_url: Box<str>,
    pub allowed: Allowed,
}

/// Why the SSO settings in the environment were not taken. Names variables,
/// never their values.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("single sign-on stays off: {0} is not set")]
    Missing(&'static str),
    #[error("single sign-on stays off: {name} {reason}")]
    Url { name: &'static str, reason: &'static str },
}

impl Config {
    /// `None` when none of the issuer, client id and redirect URL is set; an
    /// error when some are and the rest are missing or unusable. Values are
    /// trimmed and blank counts as unset; lists are comma-separated.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, ConfigError> {
        let value = |name: &str| var(name).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let list = |name: &str| -> Vec<Box<str>> {
            value(name).map(|v| v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(Box::from).collect()).unwrap_or_default()
        };
        let (issuer, client_id, redirect_url) =
            (value("FLINCH_WEB_OIDC_ISSUER"), value("FLINCH_WEB_OIDC_CLIENT_ID"), value("FLINCH_WEB_OIDC_REDIRECT_URL"));
        if issuer.is_none() && client_id.is_none() && redirect_url.is_none() {
            return Ok(None);
        }
        let issuer = issuer.ok_or(ConfigError::Missing("FLINCH_WEB_OIDC_ISSUER"))?;
        let client_id = client_id.ok_or(ConfigError::Missing("FLINCH_WEB_OIDC_CLIENT_ID"))?;
        let redirect_url = redirect_url.ok_or(ConfigError::Missing("FLINCH_WEB_OIDC_REDIRECT_URL"))?;
        provider::reachable(&issuer).map_err(|reason| ConfigError::Url { name: "FLINCH_WEB_OIDC_ISSUER", reason })?;
        let redirect = url::Url::parse(&redirect_url)
            .map_err(|_| ConfigError::Url { name: "FLINCH_WEB_OIDC_REDIRECT_URL", reason: "is not a URL" })?;
        if !matches!(redirect.scheme(), "http" | "https") || redirect.path() != CALLBACK_PATH || redirect.query().is_some() {
            let reason = "must be FLINCH's own address followed by /api/oidc/callback";
            return Err(ConfigError::Url { name: "FLINCH_WEB_OIDC_REDIRECT_URL", reason });
        }
        let allowed = Allowed {
            subjects: list("FLINCH_WEB_OIDC_ALLOWED_SUBJECTS"),
            emails: list("FLINCH_WEB_OIDC_ALLOWED_EMAILS").into_iter().map(|e| e.to_lowercase().into()).collect(),
            groups: list("FLINCH_WEB_OIDC_ALLOWED_GROUPS"),
            groups_claim: value("FLINCH_WEB_OIDC_GROUPS_CLAIM").unwrap_or_else(|| "groups".into()).into(),
        };
        Ok(Some(Self {
            name: value("FLINCH_WEB_OIDC_NAME").unwrap_or_else(|| "single sign-on".into()).into(),
            issuer: issuer.into(),
            client_id: client_id.into(),
            client_secret: value("FLINCH_WEB_OIDC_CLIENT_SECRET").map(Box::from),
            redirect_url: redirect_url.into(),
            allowed,
        }))
    }

    /// `openid`, `email` and `profile`, and `groups` when groups are allowed
    /// (the scope Authelia, Authentik and Kanidm release them under).
    pub fn scope(&self) -> &'static str {
        if self.allowed.groups.is_empty() {
            "openid email profile"
        } else {
            "openid email profile groups"
        }
    }
}

impl Allowed {
    /// Whether any list names the account whose claims these are.
    pub fn admits(&self, claims: &Map<String, Value>) -> bool {
        let text = |name: &str| claims.get(name).and_then(Value::as_str);
        let subject = text("sub").is_some_and(|sub| self.subjects.iter().any(|s| **s == *sub));
        // Some providers (Cognito) send `email_verified` as the string "true".
        let verified = matches!(claims.get("email_verified"), Some(Value::Bool(true))) || text("email_verified") == Some("true");
        let email = verified && text("email").is_some_and(|email| self.emails.iter().any(|e| **e == *email.to_lowercase()));
        let groups: Vec<&str> = match claims.get(&*self.groups_claim) {
            Some(Value::String(one)) => vec![one.as_str()],
            Some(Value::Array(many)) => many.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        let group = groups.iter().any(|group| self.groups.iter().any(|g| **g == **group));
        subject || email || group
    }
}
