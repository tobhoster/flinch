//! Who may use the API, and what every response tells the browser.
//!
//! The API hands out the library, the deletion plan and the settings, and
//! takes writes that decide what Maintainerr deletes. People log in with the
//! username and password in `FLINCH_WEB_USERNAME` and `FLINCH_WEB_PASSWORD` and
//! get a session cookie. Machines (Home Assistant, n8n, TypeSafe clients) send
//! the API key in `FLINCH_WEB_TOKEN`, as `X-Api-Key` or `Authorization: Bearer`,
//! the way Sonarr and Radarr take theirs. A server with neither refuses
//! everything rather than serving an open homelab endpoint.
//!
//! Sessions live in memory only: restarting flinch-web logs everyone out.

use crate::{refuse, AppState};
use axum::{
    extract::{Request, State as AxumState},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::collections::HashMap;
use std::io::Read;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

const CONTENT_SECURITY_POLICY: &str =
    "default-src 'self'; img-src 'self' https: data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'";

/// The session cookie. Not `__Host-` prefixed: that needs `Secure`, and plain
/// `http://localhost` through a port-forward must keep working.
const COOKIE: &str = "flinch_session";
/// A session unused this long ends; using it pushes the end back.
const SESSION_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// The end, and the cookie with it, moves at most once a day.
const SESSION_REFRESH: Duration = Duration::from_secs(24 * 60 * 60);
/// Live sessions at once; a login past this ends the one idle the longest.
const MAX_SESSIONS: usize = 64;

/// Failed logins in a row before logins pause, the first pause, and the
/// longest: each further failure doubles it.
const THROTTLE_AFTER: u32 = 5;
const THROTTLE_FIRST: Duration = Duration::from_secs(30);
const THROTTLE_MAX: Duration = Duration::from_secs(15 * 60);

/// The largest login body read: a username and password never need more, and
/// the route is open to anyone, so nobody can make flinch-web buffer more.
pub const LOGIN_BODY_LIMIT: usize = 4 * 1024;

/// The API key's own header, as Sonarr and Radarr take theirs.
const API_KEY: &str = "x-api-key";
/// What a login, a logout and every write made with the session cookie must
/// carry. The UI sends it; a page on another site cannot add a custom header
/// without CORS, which FLINCH never grants, so a forged form post is refused.
const UI_REQUEST: &str = "x-flinch-request";
/// How the ingress says the browser used HTTPS.
const FORWARDED_PROTO: &str = "x-forwarded-proto";

/// Every 401's challenges: a login form that sets a cookie, for people, and
/// the API key as a bearer token, for machines. Neither opens the browser's
/// own password dialog, as `Basic` would.
const CHALLENGES: [&str; 2] =
    [r#"Cookie realm="flinch", form-action="/api/login", cookie-name="flinch_session""#, r#"Bearer realm="flinch""#];

const NOTHING_SET: &str = "No login and no API key are set on the server, so the API refuses every request: set FLINCH_WEB_USERNAME \
     and FLINCH_WEB_PASSWORD (and FLINCH_WEB_TOKEN for automations), then restart flinch-web";
const LOGIN_OFF: &str = "The login is off: set FLINCH_WEB_USERNAME and FLINCH_WEB_PASSWORD on the server, then restart flinch-web";
const LOGIN_REFUSED: &str = "That username and password were not accepted";
/// One answer to a wrong API key, whether or not the server has one, so a
/// guess never tells a caller that a key exists.
const KEY_REFUSED: &str = "That API key was not accepted: send FLINCH_WEB_TOKEN's value";

/// Who may use the API: the login and the API key, read from the environment
/// once at startup, the live sessions, and the pause after failed logins.
pub struct Auth {
    login: Option<Login>,
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
        Self { login, api_key: configured(api_key), sessions: Mutex::default(), throttle: Mutex::default() }
    }

    /// `FLINCH_WEB_USERNAME`, `FLINCH_WEB_PASSWORD` and `FLINCH_WEB_TOKEN`,
    /// announced in one startup line that names what is set, never its value.
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok();
        let (username, password) = (var("FLINCH_WEB_USERNAME"), var("FLINCH_WEB_PASSWORD"));
        let auth = Self::new(username.as_deref(), password.as_deref(), var("FLINCH_WEB_TOKEN").as_deref());
        let half_a_login = configured(username.as_deref()).is_some() != configured(password.as_deref()).is_some();
        auth.announce(half_a_login);
        auth
    }

    fn announce(&self, half_a_login: bool) {
        if half_a_login {
            log("flinch-web auth: only one of FLINCH_WEB_USERNAME and FLINCH_WEB_PASSWORD is set - the login stays off until both are");
        }
        log(match (&self.login, &self.api_key) {
            (Some(_), Some(_)) => "flinch-web auth: login and API key set",
            (Some(_), None) => "flinch-web auth: login set; no API key (FLINCH_WEB_TOKEN), so automations cannot call the API",
            (None, Some(_)) => {
                "flinch-web auth: API key set; no login (FLINCH_WEB_USERNAME, FLINCH_WEB_PASSWORD), so the UI cannot be opened"
            }
            (None, None) => "flinch-web auth: no login and no API key set - the API refuses every request until one is",
        });
    }

    fn sessions(&self) -> MutexGuard<'_, HashMap<String, Instant>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A refusal the UI can act on: `login_configured` says whether to show
    /// the login form or how to turn the login on.
    fn unauthorized(&self, message: &str) -> Response {
        let body = axum::Json(serde_json::json!({ "error": message, "login_configured": self.login.is_some() }));
        let mut response = (StatusCode::UNAUTHORIZED, body).into_response();
        for challenge in CHALLENGES {
            response.headers_mut().append(header::WWW_AUTHENTICATE, HeaderValue::from_static(challenge));
        }
        response
    }

    /// Whether a request may use the API. An API key, when one is sent, is
    /// the whole answer: a wrong one is refused even beside a live cookie.
    fn admit(&self, headers: &HeaderMap, now: Instant) -> Admission {
        if self.login.is_none() && self.api_key.is_none() {
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
            None if self.login.is_none() => Admission::Refused(self.unauthorized(LOGIN_OFF)),
            None if cookie_values(headers).next().is_some() => {
                Admission::Refused(self.unauthorized("Your session has ended: log in again"))
            }
            None => Admission::Refused(self.unauthorized("Log in to FLINCH")),
        }
    }

    /// The live session the request's cookie names, and whether its end just
    /// moved (a day or more since it last did). An ended one is dropped.
    fn touch(&self, headers: &HeaderMap, now: Instant) -> Option<(String, bool)> {
        let mut sessions = self.sessions();
        for id in cookie_values(headers) {
            match sessions.get(id).copied() {
                Some(ends) if ends > now => {
                    let refreshed = ends + SESSION_REFRESH < now + SESSION_TTL;
                    if refreshed {
                        sessions.insert(id.to_string(), now + SESSION_TTL);
                    }
                    return Some((id.to_string(), refreshed));
                }
                Some(_) => {
                    sessions.remove(id);
                }
                None => {}
            }
        }
        None
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
        let id = match new_session_id() {
            Ok(id) => id,
            Err(error) => {
                log(&format!("flinch-web auth: cannot read /dev/urandom for a session id: {error}"));
                return refuse(StatusCode::SERVICE_UNAVAILABLE, "FLINCH cannot start a session: the server has no source of randomness");
            }
        };
        let mut sessions = self.sessions();
        // Every login gets a new id: the session this browser had ends here.
        for old in cookie_values(headers) {
            sessions.remove(old);
        }
        sessions.retain(|_, ends| *ends > now);
        while sessions.len() >= MAX_SESSIONS {
            let Some(idle) = sessions.iter().min_by_key(|(_, ends)| **ends).map(|(id, _)| id.clone()) else { break };
            sessions.remove(&idle);
        }
        sessions.insert(id.clone(), now + SESSION_TTL);
        let cookie = session_cookie(&id, SESSION_TTL, over_https(headers));
        (StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie), (header::CACHE_CONTROL, HeaderValue::from_static("no-store"))])
            .into_response()
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
/// the API key), and whether the login is set up at all. Open to anyone, so
/// it says nothing else: not who, not whether an API key exists. (A server
/// with no login says in every refusal whether it has a key, since with
/// neither it refuses everything and must say how to fix that.)
pub async fn session(AxumState(st): AxumState<AppState>, headers: HeaderMap) -> Response {
    let now = Instant::now();
    let (authenticated, refreshed) = match st.auth.admit(&headers, now) {
        Admission::ApiKey => (true, None),
        Admission::Session(refreshed) => (true, refreshed),
        Admission::Refused(_) => (false, None),
    };
    let body = serde_json::json!({ "authenticated": authenticated, "login_configured": st.auth.login.is_some() });
    let mut response = ([(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))], axum::Json(body)).into_response();
    if let Some(cookie) = refreshed {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    response
}

/// The failed logins in a row, counted for everyone at once: behind an
/// ingress every request arrives from the same address, so there is no
/// client to tell apart. A lockout pauses new logins only; open sessions and
/// the API key keep working.
#[derive(Default)]
struct Throttle {
    failures: u32,
    until: Option<Instant>,
}

impl Throttle {
    /// How long logins stay paused, or `None` while they are open.
    fn wait(&self, now: Instant) -> Option<Duration> {
        self.until.map(|until| until.saturating_duration_since(now)).filter(|wait| !wait.is_zero())
    }

    /// Counts a failure, and returns the pause it starts: from the fifth in a
    /// row, 30 s, doubling with each further one up to 15 minutes.
    fn failed(&mut self, now: Instant) -> Option<Duration> {
        self.failures = self.failures.saturating_add(1);
        let beyond = self.failures.checked_sub(THROTTLE_AFTER)?;
        let pause = THROTTLE_FIRST.saturating_mul(1 << beyond.min(10)).min(THROTTLE_MAX);
        self.until = Some(now + pause);
        Some(pause)
    }

    fn succeeded(&mut self) {
        *self = Self::default();
    }
}

/// `429` with `Retry-After` in whole seconds, rounded up.
fn paused(wait: Duration) -> Response {
    let seconds = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    let mut response = refuse(StatusCode::TOO_MANY_REQUESTS, &format!("Too many failed logins: try again in {}", spoken(wait)));
    response.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(seconds));
    response
}

/// A pause as a person reads it: "30 s", "4 min".
fn spoken(wait: Duration) -> String {
    let seconds = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    if seconds < 60 {
        format!("{seconds} s")
    } else {
        format!("{} min", seconds.div_ceil(60))
    }
}

/// The API key the request sends: `X-Api-Key`, else `Authorization: Bearer`.
/// Any other `Authorization` (a proxy's `Basic`) is not an API key.
fn presented_key(headers: &HeaderMap) -> Option<&[u8]> {
    if let Some(key) = headers.get(API_KEY) {
        return Some(key.as_bytes());
    }
    headers.get(header::AUTHORIZATION).and_then(|value| value.as_bytes().strip_prefix(b"Bearer "))
}

/// Every value the request's `Cookie` headers give the session cookie.
fn cookie_values(headers: &HeaderMap) -> impl Iterator<Item = &str> {
    headers.get_all(header::COOKIE).iter().filter_map(|value| value.to_str().ok()).flat_map(|value| value.split(';')).filter_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        (name.trim() == COOKIE).then(|| value.trim())
    })
}

/// The session cookie: script-proof, sent to this site only, and `Secure`
/// when the browser came over HTTPS. An empty id with no age clears it.
fn session_cookie(id: &str, age: Duration, secure: bool) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    let cookie = format!("{COOKIE}={id}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{secure}", age.as_secs());
    HeaderValue::from_str(&cookie).expect("a hex id and fixed attributes make a valid header")
}

/// Whether the browser reached FLINCH over HTTPS. The ingress terminates TLS
/// and says so in `X-Forwarded-Proto`; plain `http://localhost` through a
/// port-forward gets a cookie without `Secure`, which the browser would drop.
fn over_https(headers: &HeaderMap) -> bool {
    let proto = headers.get(FORWARDED_PROTO).and_then(|value| value.to_str().ok());
    proto.and_then(|value| value.split(',').next()).is_some_and(|first| first.trim().eq_ignore_ascii_case("https"))
}

/// 32 bytes from the kernel's random source, as hex. There is no fallback:
/// without randomness there is no session to hand out.
fn new_session_id() -> std::io::Result<String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().flat_map(|byte| [HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 0x0f)]]).map(char::from).collect())
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
