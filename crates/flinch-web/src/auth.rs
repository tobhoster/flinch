//! Who may use the API, and what every response tells the browser.
//!
//! The API hands out the library, the deletion plan and the settings, and
//! takes writes that decide what Maintainerr deletes. People log in with the
//! username and password in `FLINCH_WEB_USERNAME` and `FLINCH_WEB_PASSWORD` and
//! get a session cookie. Machines (Home Assistant, n8n, TypeSafe clients) send
//! the API key in `FLINCH_WEB_TOKEN`, as `X-Api-Key` or `Authorization: Bearer`,
//! the way Sonarr and Radarr take theirs. People may also sign in through an
//! OpenID Connect provider (`FLINCH_WEB_OIDC_*`, see [`oidc`]), into the same
//! sessions. A server with none of these refuses everything rather than
//! serving an open homelab endpoint.
//!
//! Sessions live in memory only: restarting flinch-web logs everyone out.

pub mod oidc;
mod session;
mod throttle;

use crate::{refuse, AppState};
use axum::{
    extract::{Request, State as AxumState},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use session::{cookie_values, cookies_named, new_session_id, over_https, session_cookie, SESSION_TTL};
use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};
use throttle::{paused, spoken, Throttle};
#[cfg(test)]
use {
    session::{MAX_SESSIONS, SESSION_REFRESH},
    throttle::{THROTTLE_AFTER, THROTTLE_MAX},
};

const CONTENT_SECURITY_POLICY: &str =
    "default-src 'self'; img-src 'self' https: data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'";

/// The largest login body read: a username and password never need more, and
/// the route is open to anyone, so nobody can make flinch-web buffer more.
pub const LOGIN_BODY_LIMIT: usize = 4 * 1024;

/// The API key's own header, as Sonarr and Radarr take theirs.
const API_KEY: &str = "x-api-key";
/// What a login, a logout and every write made with the session cookie must
/// carry. The UI sends it; a page on another site cannot add a custom header
/// without CORS, which FLINCH never grants, so a forged form post is refused.
const UI_REQUEST: &str = "x-flinch-request";

/// Every 401's challenges: a login form that sets a cookie, for people, and
/// the API key as a bearer token, for machines. Neither opens the browser's
/// own password dialog, as `Basic` would.
const CHALLENGES: [&str; 2] =
    [r#"Cookie realm="flinch", form-action="/api/login", cookie-name="flinch_session""#, r#"Bearer realm="flinch""#];

const NOTHING_SET: &str = "No login and no API key are set on the server, so the API refuses every request: set FLINCH_WEB_USERNAME \
     and FLINCH_WEB_PASSWORD or single sign-on (FLINCH_WEB_OIDC_*), and FLINCH_WEB_TOKEN for automations, then restart flinch-web";
const LOGIN_OFF: &str = "The login is off: set FLINCH_WEB_USERNAME and FLINCH_WEB_PASSWORD on the server, then restart flinch-web";
const LOGIN_REFUSED: &str = "That username and password were not accepted";
/// One answer to a wrong API key, whether or not the server has one, so a
/// guess never tells a caller that a key exists.
const KEY_REFUSED: &str = "That API key was not accepted: send FLINCH_WEB_TOKEN's value";

/// Who may use the API: the login, single sign-on and the API key, read from
/// the environment once at startup, the live sessions, and the pause after
/// failed logins.
pub struct Auth {
    login: Option<Login>,
    sso: Option<oidc::Sso>,
    api_key: Option<Box<str>>,
    /// Session id to the instant it ends.
    sessions: Mutex<HashMap<String, Instant>>,
    throttle: Mutex<Throttle>,
}

/// The one login. The username must match exactly, case included.
struct Login {
    username: Box<str>,
    password: Box<str>,
}

/// What a login form sends. A missing field is an empty one: refused like
/// any other wrong guess.
#[derive(serde::Deserialize)]
struct Credentials {
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
}

/// A value from the environment, trimmed; blank counts as unset, so a
/// `FLINCH_WEB_PASSWORD=` line cannot open the API with an empty password.
fn configured(raw: Option<&str>) -> Option<Box<str>> {
    let value = raw?.trim();
    (!value.is_empty()).then(|| Box::from(value))
}

impl Auth {
    /// The login needs both halves: a username without a password, or the
    /// reverse, leaves it off.
    pub fn new(username: Option<&str>, password: Option<&str>, api_key: Option<&str>) -> Self {
        let login = match (configured(username), configured(password)) {
            (Some(username), Some(password)) => Some(Login { username, password }),
            _ => None,
        };
        Self { login, sso: None, api_key: configured(api_key), sessions: Mutex::default(), throttle: Mutex::default() }
    }

    /// Adds single sign-on, when it is set up.
    pub fn with_sso(self, sso: Option<oidc::Sso>) -> Self {
        Self { sso, ..self }
    }

    /// `FLINCH_WEB_USERNAME`, `FLINCH_WEB_PASSWORD`, `FLINCH_WEB_TOKEN` and
    /// `FLINCH_WEB_OIDC_*`, announced in startup lines that name what is set,
    /// never its value.
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok();
        let (username, password) = (var("FLINCH_WEB_USERNAME"), var("FLINCH_WEB_PASSWORD"));
        let auth = Self::new(username.as_deref(), password.as_deref(), var("FLINCH_WEB_TOKEN").as_deref()).with_sso(oidc::Sso::from_env());
        let half_a_login = configured(username.as_deref()).is_some() != configured(password.as_deref()).is_some();
        auth.announce(half_a_login);
        auth
    }

    /// Whether people have a way in: the password login or single sign-on.
    fn people_can_log_in(&self) -> bool {
        self.login.is_some() || self.sso.is_some()
    }

    fn announce(&self, half_a_login: bool) {
        if half_a_login {
            log("flinch-web auth: only one of FLINCH_WEB_USERNAME and FLINCH_WEB_PASSWORD is set - the login stays off until both are");
        }
        log(match (self.people_can_log_in(), self.api_key.is_some()) {
            (true, true) => "flinch-web auth: login and API key set",
            (true, false) => "flinch-web auth: login set; no API key (FLINCH_WEB_TOKEN), so automations cannot call the API",
            (false, true) => {
                "flinch-web auth: API key set; no login (FLINCH_WEB_USERNAME, FLINCH_WEB_PASSWORD), so the UI cannot be opened"
            }
            (false, false) => "flinch-web auth: no login and no API key set - the API refuses every request until one is",
        });
    }

    /// A refusal the UI can act on: `login_configured` says whether to show
    /// the login form, `sso` (only when set up) the single sign-on button;
    /// with neither the UI says how to turn a login on.
    fn unauthorized(&self, message: &str) -> Response {
        let mut body = serde_json::json!({ "error": message, "login_configured": self.login.is_some() });
        if let Some(sso) = &self.sso {
            body["sso"] = sso.view();
        }
        let mut response = (StatusCode::UNAUTHORIZED, axum::Json(body)).into_response();
        for challenge in CHALLENGES {
            response.headers_mut().append(header::WWW_AUTHENTICATE, HeaderValue::from_static(challenge));
        }
        response
    }

    /// Whether a request may use the API. An API key, when one is sent, is
    /// the whole answer: a wrong one is refused even beside a live cookie.
    fn admit(&self, headers: &HeaderMap, now: Instant) -> Admission {
        if !self.people_can_log_in() && self.api_key.is_none() {
            return Admission::Refused(self.unauthorized(NOTHING_SET));
        }
        if let Some(presented) = presented_key(headers) {
            return match self.api_key.as_deref() {
                Some(expected) if same_bytes(presented, expected.as_bytes()) => Admission::ApiKey,
                _ => Admission::Refused(self.unauthorized(KEY_REFUSED)),
            };
        }
        match self.touch(headers, now) {
            Some((id, refreshed)) => Admission::Session(refreshed.then(|| session_cookie(&id, SESSION_TTL, over_https(headers)))),
            None if !self.people_can_log_in() => Admission::Refused(self.unauthorized(LOGIN_OFF)),
            None if cookie_values(headers).next().is_some() => {
                Admission::Refused(self.unauthorized("Your session has ended: log in again"))
            }
            None => Admission::Refused(self.unauthorized("Log in to FLINCH")),
        }
    }

    /// Checks a login and hands out a new session. Logins are checked one at
    /// a time, so the pause after failures cannot be raced past. One without
    /// the UI's header is refused before anything else: a page on another
    /// site could otherwise guess passwords through the owner's browser, see
    /// which guess got in, and hold the pause.
    fn log_in(&self, headers: &HeaderMap, body: &str, now: Instant) -> Response {
        if !from_the_ui(headers) {
            return not_from_the_ui("A login");
        }
        let Some(login) = &self.login else {
            return self.unauthorized(LOGIN_OFF);
        };
        let mut throttle = self.throttle.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(wait) = throttle.wait(now) {
            return paused(wait);
        }
        // The body is never echoed: a parse error could quote the password.
        let Ok(presented) = serde_json::from_str::<Credentials>(body) else {
            return refuse(StatusCode::BAD_REQUEST, r#"send the login as JSON: {"username": …, "password": …}"#);
        };
        // Both halves are always compared, so neither the answer nor the time
        // it takes says which one was wrong.
        let username_ok = same_bytes(presented.username.trim().as_bytes(), login.username.as_bytes());
        let password_ok = same_bytes(presented.password.trim().as_bytes(), login.password.as_bytes());
        if !(username_ok & password_ok) {
            let pause = throttle.failed(now);
            let failures = throttle.failures;
            log(&match pause {
                Some(pause) => format!("flinch-web auth: a login was refused ({failures} in a row); logins pause for {}", spoken(pause)),
                None => format!("flinch-web auth: a login was refused ({failures} in a row)"),
            });
            return self.unauthorized(LOGIN_REFUSED);
        }
        throttle.succeeded();
        drop(throttle);
        match self.open_session(headers, now) {
            Ok(cookie) => {
                (StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie), (header::CACHE_CONTROL, HeaderValue::from_static("no-store"))])
                    .into_response()
            }
            Err(refusal) => refusal.into_response(),
        }
    }
}

/// How a protected request got in, or why not.
enum Admission {
    /// A valid API key: an automation, which needs no `X-Flinch-Request`.
    ApiKey,
    /// A live session; `Some` is its refreshed cookie, to send back.
    Session(Option<HeaderValue>),
    Refused(Response),
}

/// Admits a request with a live session cookie or the API key. A write made
/// with the cookie must also carry `X-Flinch-Request: 1`. No cache may keep
/// any answer: a cookie, unlike `Authorization`, does not stop a shared cache
/// from serving the library, the settings or a refreshed session cookie to
/// the next visitor.
pub async fn require_auth(AxumState(st): AxumState<AppState>, request: Request, next: Next) -> Response {
    let mut response = match st.auth.admit(request.headers(), Instant::now()) {
        Admission::ApiKey => next.run(request).await,
        Admission::Session(refreshed) => {
            let read = matches!(*request.method(), Method::GET | Method::HEAD);
            if read || from_the_ui(request.headers()) {
                let mut response = next.run(request).await;
                if let Some(cookie) = refreshed {
                    response.headers_mut().append(header::SET_COOKIE, cookie);
                }
                response
            } else {
                not_from_the_ui("A write with the session cookie")
            }
        }
        Admission::Refused(response) => response,
    };
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Whether the request carries `X-Flinch-Request: 1`: the UI sends it, and a
/// page on another site cannot.
fn from_the_ui(headers: &HeaderMap) -> bool {
    headers.get(UI_REQUEST).is_some_and(|value| value == "1")
}

/// `403` for a request that needed the UI's header and came without it.
fn not_from_the_ui(what: &str) -> Response {
    refuse(StatusCode::FORBIDDEN, &format!("{what} needs the header X-Flinch-Request: 1, which the FLINCH UI sends"))
}

/// `POST /api/login` with `{"username": …, "password": …}` and
/// `X-Flinch-Request: 1`: `204` and a new session cookie, or one refusal that
/// never says which half was wrong. The route reads at most
/// [`LOGIN_BODY_LIMIT`] bytes.
pub async fn login(AxumState(st): AxumState<AppState>, headers: HeaderMap, body: String) -> Response {
    st.auth.log_in(&headers, &body, Instant::now())
}

/// `POST /api/logout` with `X-Flinch-Request: 1`: ends this browser's
/// session and clears its cookie. Open to anyone: without a live session it
/// only clears the cookie. Without the header it does neither, so a page on
/// another site cannot log the browser out.
pub async fn logout(AxumState(st): AxumState<AppState>, headers: HeaderMap) -> Response {
    if !from_the_ui(&headers) {
        return not_from_the_ui("A logout");
    }
    let mut sessions = st.auth.sessions();
    for id in cookie_values(&headers) {
        sessions.remove(id);
    }
    let cleared = session_cookie("", Duration::ZERO, over_https(&headers));
    (StatusCode::NO_CONTENT, [(header::SET_COOKIE, cleared), (header::CACHE_CONTROL, HeaderValue::from_static("no-store"))]).into_response()
}

/// `GET /api/session`: whether this browser is logged in (a live session, or
/// the API key), whether the password login is set up, and the single
/// sign-on button's label (`sso`, only when set up). Open to anyone, so it
/// says nothing else: not who, not whether an API key exists. (A server with
/// no login says in every refusal whether it has a key, since with neither it
/// refuses everything and must say how to fix that.)
pub async fn session(AxumState(st): AxumState<AppState>, headers: HeaderMap) -> Response {
    let now = Instant::now();
    let (authenticated, refreshed) = match st.auth.admit(&headers, now) {
        Admission::ApiKey => (true, None),
        Admission::Session(refreshed) => (true, refreshed),
        Admission::Refused(_) => (false, None),
    };
    let mut body = serde_json::json!({ "authenticated": authenticated, "login_configured": st.auth.login.is_some() });
    if let Some(sso) = &st.auth.sso {
        body["sso"] = sso.view();
    }
    let mut response = ([(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))], axum::Json(body)).into_response();
    if let Some(cookie) = refreshed {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    response
}

/// The API key the request sends: `X-Api-Key`, else `Authorization: Bearer`.
/// Any other `Authorization` (a proxy's `Basic`) is not an API key.
fn presented_key(headers: &HeaderMap) -> Option<&[u8]> {
    if let Some(key) = headers.get(API_KEY) {
        return Some(key.as_bytes());
    }
    headers.get(header::AUTHORIZATION).and_then(|value| value.as_bytes().strip_prefix(b"Bearer "))
}

/// Equality that looks at every byte whatever the first difference, so the
/// response time does not reveal how much of a guessed key or password was
/// right. The length is not secret: guessing it gets an attacker no closer to
/// the value, and the login pause allows too few tries to time it.
fn same_bytes(presented: &[u8], expected: &[u8]) -> bool {
    if presented.len() != expected.len() {
        return false;
    }
    let difference = presented.iter().zip(expected).fold(0u8, |acc, (a, b)| acc | (a ^ b));
    // Keeps the optimiser from turning the fold back into an early exit.
    std::hint::black_box(difference) == 0
}

/// Every line flinch-web logs about access. It says what happened and how
/// often, never a username, password, key or session id.
fn log(line: &str) {
    #[cfg(test)]
    tests::LOGGED.with(|lines| lines.borrow_mut().push(line.to_string()));
    eprintln!("{line}");
}

/// Every response, public or not: no framing, no sniffing, no referrer, and
/// scripts only from this origin (posters may come from any https host).
pub async fn security_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CONTENT_SECURITY_POLICY));
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    response
}

#[cfg(test)]
mod tests;
