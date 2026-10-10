//! Single sign-on through an OpenID Connect provider (Authelia, Authentik,
//! Keycloak, Kanidm, Pocket ID …), next to the password login and the API key.
//!
//! The authorization code flow (OpenID Connect Core 1.0 §3.1) with PKCE
//! (RFC 7636, S256), a `state` and a `nonce`:
//! 1. `GET /api/oidc/login` draws the three, keeps them here for ten minutes,
//!    binds `state` to this browser with a short-lived cookie, and sends the
//!    browser to the provider.
//! 2. The provider sends it back to `GET /api/oidc/callback?code=…&state=…`.
//!    The state must be one FLINCH handed out, unused, unexpired, and this
//!    browser's (its cookie), so nobody can log someone in as themselves by
//!    sending them a callback link. The code is traded for tokens, the ID token
//!    checked (see `jwt`), userinfo read for email and groups, and the account
//!    matched against the allow lists. Nobody is allowed by default.
//! 3. A session from the same store the password login uses.
//!
//! Cookies: the session cookie keeps `SameSite=Strict`. The provider's
//! redirect back is a navigation from another site, on which a browser sends
//! no `Strict` cookie, so the cookie binding `state` is `SameSite=Lax` (sent
//! on top-level `GET` navigations), lives ten minutes and only under
//! `/api/oidc/`. The callback answers with a page that moves on to `/` itself:
//! the first load of the UI is then a same-site navigation, so the new
//! `Strict` session cookie goes with it.
//!
//! Nothing here logs or returns a code, token, subject or email.

mod config;
mod jwt;
mod provider;
#[cfg(test)]
mod tests;

use super::{cookies_named, log, new_session_id, over_https, same_bytes};
use crate::{refuse, AppState};
use axum::{
    extract::{RawQuery, State as AxumState},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
pub use config::Config;
use provider::{Discovery, ProviderError};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Where the provider sends the browser back; `FLINCH_WEB_OIDC_REDIRECT_URL`
/// must end in it.
pub const CALLBACK_PATH: &str = "/api/oidc/callback";
/// Where the login page's button goes.
pub const LOGIN_PATH: &str = "/api/oidc/login";
/// The cookie binding a sign-in's `state` to the browser that started it.
const FLOW_COOKIE: &str = "flinch_oidc";
/// How long a started sign-in may take at the provider.
const FLOW_TTL: Duration = Duration::from_secs(10 * 60);
/// Sign-ins under way at once; starting one past this drops the oldest.
const MAX_PENDING: usize = 64;
/// How long the provider's discovery document is reused.
const DISCOVERY_TTL: Duration = Duration::from_secs(10 * 60);
/// The longest any call to the provider may take.
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(10);

/// A sign-in under way: what the callback must find.
struct Pending {
    nonce: String,
    verifier: String,
    started: Instant,
}

/// Single sign-on, when the environment sets it up.
pub struct Sso {
    config: Config,
    http: reqwest::Client,
    pending: Mutex<HashMap<String, Pending>>,
    discovery: Mutex<Option<(Instant, Arc<Discovery>)>>,
}

impl Sso {
    /// The client follows no redirect: the provider's endpoints come from
    /// discovery and answer directly.
    pub fn new(config: Config) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(PROVIDER_TIMEOUT)
            .user_agent(concat!("flinch-web/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { config, http, pending: Mutex::default(), discovery: Mutex::default() })
    }

    /// `FLINCH_WEB_OIDC_*`, announced in one startup line that names the
    /// provider's issuer and what is allowed, never the client secret.
    pub fn from_env() -> Option<Self> {
        let config = match Config::from_vars(|name| std::env::var(name).ok()) {
            Ok(config) => config?,
            Err(error) => {
                log(&format!("flinch-web auth: {error}"));
                return None;
            }
        };
        let allowed = &config.allowed;
        let (subjects, emails, groups) = (allowed.subjects.len(), allowed.emails.len(), allowed.groups.len());
        log(&if subjects + emails + groups == 0 {
            format!(
                "flinch-web auth: single sign-on with {} set, but nobody is allowed (FLINCH_WEB_OIDC_ALLOWED_SUBJECTS, \
                 _EMAILS, _GROUPS) - every sign-in is refused",
                config.issuer
            )
        } else {
            format!(
                "flinch-web auth: single sign-on with {} set; {subjects} subjects, {emails} emails and {groups} groups allowed",
                config.issuer
            )
        });
        match Self::new(config) {
            Ok(sso) => Some(sso),
            Err(error) => {
                log(&format!("flinch-web auth: single sign-on stays off: no HTTP client: {error}"));
                None
            }
        }
    }

    /// What the login page needs: the button's label and where it goes.
    pub fn view(&self) -> Value {
        serde_json::json!({ "name": &*self.config.name, "login": LOGIN_PATH })
    }

    fn pending(&self) -> MutexGuard<'_, HashMap<String, Pending>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Discovery, reused for [`DISCOVERY_TTL`].
    async fn discovery(&self, now: Instant) -> Result<Arc<Discovery>, ProviderError> {
        let cached = self.discovery.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some((_, discovery)) = cached.filter(|(fetched, _)| now.saturating_duration_since(*fetched) < DISCOVERY_TTL) {
            return Ok(discovery);
        }
        let discovery = Arc::new(provider::discover(&self.http, &self.config.issuer).await?);
        *self.discovery.lock().unwrap_or_else(PoisonError::into_inner) = Some((now, Arc::clone(&discovery)));
        Ok(discovery)
    }

    /// The provider's authorization URL for a new sign-in, and its `state`.
    async fn begin(&self, now: Instant) -> Result<(String, String), Failure> {
        let discovery = self.discovery(now).await.map_err(Failure::Provider)?;
        let (state, nonce, verifier) = (drawn()?, drawn()?, drawn()?);
        let mut url = provider::reachable(&discovery.authorization_endpoint)
            .map_err(|reason| Failure::Provider(ProviderError::Malformed { what: "authorization endpoint", reason }))?;
        let challenge = pkce_challenge(&verifier);
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.config.client_id)
            .append_pair("redirect_uri", &self.config.redirect_url)
            .append_pair("scope", self.config.scope())
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        let mut pending = self.pending();
        pending.retain(|_, flow| now.saturating_duration_since(flow.started) < FLOW_TTL);
        while pending.len() >= MAX_PENDING {
            let Some(oldest) = pending.iter().min_by_key(|(_, flow)| flow.started).map(|(id, _)| id.clone()) else { break };
            pending.remove(&oldest);
        }
        pending.insert(state.clone(), Pending { nonce, verifier, started: now });
        Ok((url.into(), state))
    }

    /// Checks the callback, and whether the account may sign in.
    async fn finish(&self, headers: &HeaderMap, query: &Callback, now: Instant) -> Result<(), Failure> {
        let state = query.state.as_deref().unwrap_or_default();
        // Single use: whatever happens next, this state is spent.
        let flow = self.pending().remove(state).filter(|flow| now.saturating_duration_since(flow.started) < FLOW_TTL);
        let bound = cookies_named(headers, FLOW_COOKIE).any(|value| same_bytes(value.as_bytes(), state.as_bytes()));
        let Some(flow) = flow.filter(|_| bound && !state.is_empty()) else { return Err(Failure::Unknown) };
        if let Some(error) = &query.error {
            return Err(Failure::Refused(error.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(64).collect()));
        }
        // RFC 9207: a provider that names itself must name the configured one.
        if query.iss.as_deref().is_some_and(|iss| iss != &*self.config.issuer) {
            return Err(Failure::MixUp);
        }
        let code = query.code.as_deref().filter(|code| !code.is_empty()).ok_or(Failure::Unknown)?;
        let discovery = self.discovery(now).await.map_err(Failure::Provider)?;
        let client = provider::Client {
            id: &self.config.client_id,
            secret: self.config.client_secret.as_deref(),
            redirect_url: &self.config.redirect_url,
        };
        let tokens = provider::redeem(&self.http, &discovery, &client, code, &flow.verifier).await.map_err(Failure::Provider)?;
        let keys = provider::keys(&self.http, &discovery).await.map_err(Failure::Provider)?;
        let expected =
            jwt::Expected { issuer: &self.config.issuer, client_id: &self.config.client_id, nonce: &flow.nonce, now_unix: unix_now() };
        let mut claims = jwt::verify(&tokens.id_token, &keys.keys, &expected).map_err(Failure::IdToken)?;
        let allowed = &self.config.allowed;
        if !allowed.emails.is_empty() || !allowed.groups.is_empty() {
            let subject = claims.get("sub").and_then(Value::as_str).unwrap_or_default().to_string();
            let info = provider::userinfo(&self.http, &discovery, &tokens.access_token, &subject).await.map_err(Failure::Provider)?;
            claims.extend(info.into_iter().flatten());
        }
        if allowed.admits(&claims) {
            Ok(())
        } else {
            Err(Failure::NotAllowed)
        }
    }
}

/// Why a sign-in did not end in a session. Every variant is safe to log.
#[derive(Debug, thiserror::Error)]
enum Failure {
    #[error("{0}")]
    Provider(ProviderError),
    #[error("the ID token was refused: {0}")]
    IdToken(jwt::IdTokenError),
    #[error("the callback names no sign-in this browser started in the last ten minutes (or it was used already)")]
    Unknown,
    #[error("the callback names another issuer than FLINCH_WEB_OIDC_ISSUER (RFC 9207 iss)")]
    MixUp,
    #[error("the provider refused the sign-in ({0})")]
    Refused(String),
    #[error("the account is in none of FLINCH_WEB_OIDC_ALLOWED_SUBJECTS, _EMAILS or _GROUPS")]
    NotAllowed,
    #[error("cannot read /dev/urandom: {0}")]
    Randomness(std::io::Error),
}

fn drawn() -> Result<String, Failure> {
    new_session_id().map_err(Failure::Randomness)
}

/// RFC 7636 §4.2: `BASE64URL(SHA256(verifier))`, unpadded.
fn pkce_challenge(verifier: &str) -> String {
    use base64::Engine;
    let digest = ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest.as_ref())
}

/// The wall clock as Unix seconds, for comparing with the token's `exp`.
fn unix_now() -> i64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
}

/// The callback's query (RFC 6749 §4.1.2, §4.1.2.1; RFC 9207 `iss`). A
/// parameter given twice counts once, as first given.
#[derive(Debug, Default)]
pub struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    iss: Option<String>,
}

impl Callback {
    fn parse(query: Option<&str>) -> Self {
        let mut callback = Self::default();
        for (name, value) in url::form_urlencoded::parse(query.unwrap_or_default().as_bytes()) {
            let slot = match &*name {
                "code" => &mut callback.code,
                "state" => &mut callback.state,
                "error" => &mut callback.error,
                "iss" => &mut callback.iss,
                _ => continue,
            };
            slot.get_or_insert_with(|| value.into_owned());
        }
        callback
    }
}

/// The cookie that binds `state` to this browser; an empty one clears it.
fn flow_cookie(state: &str, age: Duration, secure: bool) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    let cookie = format!("{FLOW_COOKIE}={state}; Path=/api/oidc/; HttpOnly; SameSite=Lax; Max-Age={}{secure}", age.as_secs());
    HeaderValue::from_str(&cookie).unwrap_or_else(|_| HeaderValue::from_static("flinch_oidc=; Path=/api/oidc/; Max-Age=0"))
}

/// A page that sends the browser on to `to` by itself, so the next load is a
/// same-site navigation that carries the `Strict` session cookie.
fn hop(status: StatusCode, to: &'static str) -> Response {
    let page = format!(
        "<!doctype html><meta charset=\"utf-8\"><meta http-equiv=\"refresh\" content=\"0;url={to}\">\
         <title>FLINCH</title><p><a href=\"{to}\">Continue to FLINCH</a></p>"
    );
    (status, [(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"))], page).into_response()
}

fn not_set_up() -> Response {
    refuse(
        StatusCode::NOT_FOUND,
        "Single sign-on is not set up: set FLINCH_WEB_OIDC_ISSUER, _CLIENT_ID and _REDIRECT_URL, then restart flinch-web",
    )
}

/// `GET /api/oidc/login`: off to the provider, with this browser's state
/// cookie. A provider that cannot be reached sends the browser back to the
/// login page, which says so.
pub async fn start(AxumState(st): AxumState<AppState>, headers: HeaderMap) -> Response {
    let Some(sso) = st.auth.sso.as_ref() else { return not_set_up() };
    let mut response = match sso.begin(Instant::now()).await {
        Ok((location, state)) => match HeaderValue::from_str(&location) {
            Ok(location) => {
                let cookie = flow_cookie(&state, FLOW_TTL, over_https(&headers));
                (StatusCode::SEE_OTHER, [(header::LOCATION, location), (header::SET_COOKIE, cookie)]).into_response()
            }
            Err(_) => hop(StatusCode::BAD_GATEWAY, "/?sso=failed"),
        },
        Err(failure) => {
            log(&format!("flinch-web auth: a single sign-on could not start: {failure}"));
            hop(StatusCode::BAD_GATEWAY, "/?sso=failed")
        }
    };
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `GET /api/oidc/callback`: a session for an allowed account, then on to
/// the UI; otherwise back to the login page with `?sso=denied` (not allowed)
/// or `?sso=failed` (anything else, logged).
pub async fn callback(AxumState(st): AxumState<AppState>, headers: HeaderMap, RawQuery(query): RawQuery) -> Response {
    let Some(sso) = st.auth.sso.as_ref() else { return not_set_up() };
    let query = Callback::parse(query.as_deref());
    let now = Instant::now();
    let mut response = match sso.finish(&headers, &query, now).await {
        Ok(()) => match st.auth.open_session(&headers, now) {
            Ok(cookie) => {
                log("flinch-web auth: a single sign-on opened a session");
                let mut response = hop(StatusCode::OK, "/");
                response.headers_mut().append(header::SET_COOKIE, cookie);
                response
            }
            Err(refusal) => refusal.into_response(),
        },
        Err(Failure::NotAllowed) => {
            log(&format!("flinch-web auth: a single sign-on was refused: {}", Failure::NotAllowed));
            hop(StatusCode::FORBIDDEN, "/?sso=denied")
        }
        Err(failure) => {
            log(&format!("flinch-web auth: a single sign-on failed: {failure}"));
            let status = if matches!(failure, Failure::Provider(_)) { StatusCode::BAD_GATEWAY } else { StatusCode::BAD_REQUEST };
            hop(status, "/?sso=failed")
        }
    };
    response.headers_mut().append(header::SET_COOKIE, flow_cookie("", Duration::ZERO, over_https(&headers)));
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
