//! The pause after failed password logins.

use crate::refuse;
use axum::{
    http::{header, HeaderValue, StatusCode},
    response::Response,
};
use std::time::{Duration, Instant};

/// Failed logins in a row before logins pause, the first pause, and the
/// longest: each further failure doubles it.
pub(super) const THROTTLE_AFTER: u32 = 5;
const THROTTLE_FIRST: Duration = Duration::from_secs(30);
pub(super) const THROTTLE_MAX: Duration = Duration::from_secs(15 * 60);

/// The failed logins in a row, counted for everyone at once: behind an
/// ingress every request arrives from the same address, so there is no
/// client to tell apart. A lockout pauses new logins only; open sessions and
/// the API key keep working.
#[derive(Default)]
pub(super) struct Throttle {
    pub(super) failures: u32,
    pub(super) until: Option<Instant>,
}

impl Throttle {
    /// How long logins stay paused, or `None` while they are open.
    pub(super) fn wait(&self, now: Instant) -> Option<Duration> {
        self.until.map(|until| until.saturating_duration_since(now)).filter(|wait| !wait.is_zero())
    }

    /// Counts a failure, and returns the pause it starts: from the fifth in a
    /// row, 30 s, doubling with each further one up to 15 minutes.
    pub(super) fn failed(&mut self, now: Instant) -> Option<Duration> {
        self.failures = self.failures.saturating_add(1);
        let beyond = self.failures.checked_sub(THROTTLE_AFTER)?;
        let pause = THROTTLE_FIRST.saturating_mul(1 << beyond.min(10)).min(THROTTLE_MAX);
        self.until = Some(now + pause);
        Some(pause)
    }

    pub(super) fn succeeded(&mut self) {
        *self = Self::default();
    }
}

/// `429` with `Retry-After` in whole seconds, rounded up.
pub(super) fn paused(wait: Duration) -> Response {
    let seconds = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    let mut response = refuse(StatusCode::TOO_MANY_REQUESTS, &format!("Too many failed logins: try again in {}", spoken(wait)));
    response.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(seconds));
    response
}

/// A pause as a person reads it: "30 s", "4 min".
pub(super) fn spoken(wait: Duration) -> String {
    let seconds = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    if seconds < 60 {
        format!("{seconds} s")
    } else {
        format!("{} min", seconds.div_ceil(60))
    }
}
