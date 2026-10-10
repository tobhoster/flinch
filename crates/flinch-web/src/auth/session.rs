//! The session store and its cookie: one store for every way a person logs
//! in (the password login, single sign-on), in memory only.

use super::{log, Auth};
use crate::refuse;
use axum::{
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use std::collections::HashMap;
use std::io::Read;
use std::sync::{MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The session cookie. Not `__Host-` prefixed: that needs `Secure`, and plain
/// `http://localhost` through a port-forward must keep working.
pub(super) const COOKIE: &str = "flinch_session";
/// A session unused this long ends; using it pushes the end back.
pub(super) const SESSION_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// The end, and the cookie with it, moves at most once a day.
pub(super) const SESSION_REFRESH: Duration = Duration::from_secs(24 * 60 * 60);
/// Live sessions at once; a login past this ends the one idle the longest.
pub(super) const MAX_SESSIONS: usize = 64;
/// How the ingress says the browser used HTTPS.
const FORWARDED_PROTO: &str = "x-forwarded-proto";

impl Auth {
    pub(super) fn sessions(&self) -> MutexGuard<'_, HashMap<String, Instant>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The live session the request's cookie names, and whether its end just
    /// moved (a day or more since it last did). An ended one is dropped.
    pub(super) fn touch(&self, headers: &HeaderMap, now: Instant) -> Option<(String, bool)> {
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

    /// A new session for a browser that just proved who it is, by password
    /// or single sign-on, and its cookie. Any session the browser had ends.
    pub(super) fn open_session(&self, headers: &HeaderMap, now: Instant) -> Result<HeaderValue, NoRandomness> {
        let id = new_session_id().map_err(|error| {
            log(&format!("flinch-web auth: cannot read /dev/urandom for a session id: {error}"));
            NoRandomness
        })?;
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
        Ok(session_cookie(&id, SESSION_TTL, over_https(headers)))
    }
}

/// No session could be drawn: the server has no source of randomness.
pub(super) struct NoRandomness;

impl IntoResponse for NoRandomness {
    fn into_response(self) -> Response {
        refuse(StatusCode::SERVICE_UNAVAILABLE, "FLINCH cannot start a session: the server has no source of randomness")
    }
}

/// Every value the request's `Cookie` headers give the session cookie.
pub(super) fn cookie_values(headers: &HeaderMap) -> impl Iterator<Item = &str> {
    cookies_named(headers, COOKIE)
}

/// Every value the request's `Cookie` headers give the cookie `wanted`.
pub(super) fn cookies_named<'a>(headers: &'a HeaderMap, wanted: &'a str) -> impl Iterator<Item = &'a str> {
    headers.get_all(header::COOKIE).iter().filter_map(|value| value.to_str().ok()).flat_map(|value| value.split(';')).filter_map(
        move |pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name.trim() == wanted).then(|| value.trim())
        },
    )
}

/// The session cookie: script-proof, sent to this site only, and `Secure`
/// when the browser came over HTTPS. An empty id with no age clears it.
pub(super) fn session_cookie(id: &str, age: Duration, secure: bool) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    let cookie = format!("{COOKIE}={id}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{secure}", age.as_secs());
    HeaderValue::from_str(&cookie).expect("a hex id and fixed attributes make a valid header")
}

/// Whether the browser reached FLINCH over HTTPS. The ingress terminates TLS
/// and says so in `X-Forwarded-Proto`; plain `http://localhost` through a
/// port-forward gets a cookie without `Secure`, which the browser would drop.
pub(super) fn over_https(headers: &HeaderMap) -> bool {
    let proto = headers.get(FORWARDED_PROTO).and_then(|value| value.to_str().ok());
    proto.and_then(|value| value.split(',').next()).is_some_and(|first| first.trim().eq_ignore_ascii_case("https"))
}

/// 32 bytes from the kernel's random source, as hex. There is no fallback:
/// without randomness there is no session to hand out.
pub(super) fn new_session_id() -> std::io::Result<String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().flat_map(|byte| [HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 0x0f)]]).map(char::from).collect())
}
