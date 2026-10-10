//! Household self-service in the web: the no-login link pages and the
//! admin's removal queue ([`flinch_archive::requests`]).
//!
//! - `GET /r/{token}` shows the title and one button; `POST /r/{token}`
//!   does what the token says. Both are open: the token is the credential,
//!   verified against `FLINCH_WEB_LINK_SECRET` before anything else is read,
//!   and good for one card, one action and until its expiry. A second POST
//!   finds the request the first one made.
//! - `GET /api/requests` lists the book; `POST /api/requests/{id}/{decision}`
//!   approves, denies or cancels. Both sit behind the login.
//!
//! This is the only writer of `requests.json`; one lock serialises the
//! writes of this process. A keep wakes the daemon (`run.now`) so the item
//! leaves the shelf on the next cycle, not the next scheduled one.

use crate::{refuse, AppState};
use axum::extract::{Path as AxumPath, State as AxumState};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use flinch_archive::daemon::read_settings;
use flinch_archive::requests::link::{token_id, Action, Link, LinkError};
use flinch_archive::requests::{Decision, Recorded, RequestBook, Status};
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

static WRITING: Mutex<()> = Mutex::new(());

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Text for HTML: the title is the *arr's, not ours.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// A page with no script and no style: the CSP allows neither inline.
fn page(status: StatusCode, heading: &str, body: &str) -> Response {
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\">\
         <meta name=\"robots\" content=\"noindex\"><title>FLINCH</title></head><body><main><h1>{}</h1>{body}</main></body></html>",
        escape(heading)
    );
    (status, [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], html).into_response()
}

/// Every library item as (id, display title), from the published items.
fn titles(dir: &Path) -> Vec<(String, String)> {
    let items: Vec<serde_json::Value> =
        std::fs::read(dir.join("items.json")).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
    items
        .iter()
        .filter_map(|item| {
            let id = item["id"].as_str()?.to_string();
            let title = item["title"].as_str()?;
            let title = match item["season_label"].as_str() {
                Some(season) => format!("{title} {season}"),
                None => title.to_string(),
            };
            Some((id, title))
        })
        .collect()
}

/// What a link may do here, or the page saying why not.
struct Checked {
    link: Link,
    id: String,
    title: String,
    config: flinch_archive::requests::HouseholdConfig,
    library: Vec<(String, String)>,
}

fn check(st: &AppState, token: &str, now: u64) -> Result<Checked, Box<Response>> {
    let gone = || page(StatusCode::NOT_FOUND, "This link does not work", "<p>Ask whoever sent it for a new one.</p>");
    let secret = st.links.as_ref().ok_or_else(gone)?;
    let link = secret.verify(token, now).map_err(|error| match error {
        LinkError::Expired => page(StatusCode::GONE, "This link has expired", "<p>Links last until the title's leave date.</p>"),
        LinkError::Invalid => gone(),
    })?;
    let config = read_settings(&st.dir.join("settings.json")).map(|settings| settings.household).ok().filter(|config| config.enabled);
    let config = config.ok_or_else(|| page(StatusCode::NOT_FOUND, "Self-service is switched off", "<p>Ask the admin.</p>"))?;
    let library = titles(&st.dir);
    let title = library.iter().find(|(id, _)| *id == link.card_id).map(|(_, title)| title.clone());
    let title = title.ok_or_else(|| page(StatusCode::NOT_FOUND, "This title is no longer in the library", ""))?;
    let id = token_id(token).ok_or_else(gone)?;
    Ok(Checked { link, id, title, config, library })
}

fn ask(action: Action) -> (&'static str, &'static str) {
    match action {
        Action::Keep => ("Keep it", "It stays in the library, and off the Leaving Soon list, for a while."),
        Action::Remove => ("I'm done, remove it", "The admin decides; nothing goes before that."),
    }
}

pub(crate) async fn show(AxumState(st): AxumState<AppState>, AxumPath(token): AxumPath<String>) -> Response {
    let checked = match check(&st, &token, now()) {
        Ok(checked) => checked,
        Err(page) => return *page,
    };
    let (button, note) = ask(checked.link.action);
    let body = format!("<p>{note}</p><form method=\"post\"><button type=\"submit\">{button}</button></form>");
    page(StatusCode::OK, &checked.title, &body)
}

pub(crate) async fn act(AxumState(st): AxumState<AppState>, AxumPath(token): AxumPath<String>) -> Response {
    let now = now();
    let checked = match check(&st, &token, now) {
        Ok(checked) => checked,
        Err(page) => return *page,
    };
    let recorded = {
        let _writing = WRITING.lock().unwrap_or_else(PoisonError::into_inner);
        let mut book = RequestBook::read(&st.dir);
        let present: HashSet<&str> = checked.library.iter().map(|(id, _)| id.as_str()).collect();
        book.forget_gone(&present, now);
        let recorded = book.record(&checked.link, &checked.id, &checked.title, &checked.config, now);
        if let Ok(Recorded::New(_)) = &recorded {
            if let Err(error) = book.write(&st.dir) {
                eprintln!("[flinch-web] requests.json write failed: {error}");
                return page(StatusCode::INTERNAL_SERVER_ERROR, "Not saved", "<p>Try again in a minute.</p>");
            }
        }
        recorded
    };
    let request = match &recorded {
        Ok(recorded) => recorded.request(),
        Err(refused) => return page(StatusCode::CONFLICT, &checked.title, &format!("<p>{}</p>", escape(&refused.to_string()))),
    };
    let words = match (request.kind, request.status) {
        (Action::Keep, Status::Active) => {
            // Woken, the daemon takes it off the shelf on its next cycle.
            std::fs::write(st.dir.join("run.now"), b"1").ok();
            let until =
                request.until.map_or_else(String::new, |until| format!(" until {}", flinch_archive::notify::utc_date(until / 86_400)));
            format!("Kept{until}. Thanks!")
        }
        (Action::Remove, Status::Pending) => "Asked. The admin decides whether it goes.".to_string(),
        (Action::Remove, Status::Approved) => "Already approved: it goes when the space is needed.".to_string(),
        _ => "This was already decided.".to_string(),
    };
    page(StatusCode::OK, &checked.title, &format!("<p>{}</p>", escape(&words)))
}

/// `GET /api/requests`: the book, and whether links can be made.
pub(crate) async fn list(AxumState(st): AxumState<AppState>) -> Response {
    let enabled = read_settings(&st.dir.join("settings.json")).map(|settings| settings.household.enabled).unwrap_or(false);
    let mut book = RequestBook::read(&st.dir);
    book.prune(now());
    book.requests.reverse();
    axum::Json(serde_json::json!({ "enabled": enabled, "secret": st.links.is_some(), "requests": book.requests })).into_response()
}

/// `POST /api/requests/{id}/{approve|deny|cancel}`.
pub(crate) async fn decide(AxumState(st): AxumState<AppState>, AxumPath((id, decision)): AxumPath<(String, String)>) -> Response {
    let decision = match decision.as_str() {
        "approve" => Decision::Approve,
        "deny" => Decision::Deny,
        "cancel" => Decision::Cancel,
        _ => return refuse(StatusCode::BAD_REQUEST, "the decision is approve, deny or cancel"),
    };
    let now = now();
    let _writing = WRITING.lock().unwrap_or_else(PoisonError::into_inner);
    let mut book = RequestBook::read(&st.dir);
    book.prune(now);
    let decided = match book.decide(&id, decision, now) {
        Ok(request) => request.clone(),
        Err(refused) => {
            let status = if refused == flinch_archive::requests::Refused::Unknown { StatusCode::NOT_FOUND } else { StatusCode::CONFLICT };
            return refuse(status, &refused.to_string());
        }
    };
    if let Err(error) = book.write(&st.dir) {
        return refuse(StatusCode::INTERNAL_SERVER_ERROR, &format!("cannot save the decision: {error}"));
    }
    std::fs::write(st.dir.join("run.now"), b"1").ok();
    axum::Json(decided).into_response()
}

#[cfg(test)]
mod tests;
