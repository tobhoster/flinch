//! flinch-web — the swarm's window for a homelab.
//!
//! Serves the React UI (built by `frontend/` at image build time) next to the
//! JSON API. It reads the state files the daemon publishes and writes only two:
//! `settings.json` from the Settings page and `run.now` to ask for a run. No
//! *arr keys, no database.

mod auth;
mod snapshot;
mod systemone;

use anyhow::{Context, Result};
use axum::{
    extract::{Path as AxumPath, State as AxumState},
    http::{header, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{any, get, post},
    Router,
};
use flinch_archive::daemon::{read_settings, write_settings, RuntimeSettings};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone)]
struct AppState {
    dir: Arc<PathBuf>,
    web: Arc<PathBuf>,
    /// `FLINCH_WEB_TOKEN`, read once at startup. `None` keeps the API closed.
    token: Option<Arc<str>>,
    /// `items.json`, parsed once per published version for `/v1/systemone`.
    items: Arc<snapshot::Snapshot>,
}

fn read_json(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|_| "null".to_string())
}

async fn api_status(AxumState(st): AxumState<AppState>) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/json")], read_json(&st.dir.join("status.json")))
}

async fn api_items(AxumState(st): AxumState<AppState>) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/json")], read_json(&st.dir.join("items.json")))
}

/// Settings as the browser sees them. The Plex token stays on the server;
/// `plex_token_set` says whether one is saved.
#[derive(serde::Serialize)]
struct SettingsView {
    #[serde(flatten)]
    settings: RuntimeSettings,
    plex_token_set: bool,
}

/// The settings the daemon runs with: `settings.json`, every missing field at
/// its default. An unreadable file is reported, never replaced by defaults:
/// the daemon keeps its last good settings then, which this cannot show.
async fn api_settings_get(AxumState(st): AxumState<AppState>) -> Response {
    match read_settings(&st.dir.join("settings.json")) {
        Ok(settings) => {
            let plex_token_set = !settings.plex_token.is_empty();
            let settings = RuntimeSettings { plex_token: String::new(), ..settings };
            axum::Json(SettingsView { settings, plex_token_set }).into_response()
        }
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

/// Persist operator settings. The daemon re-reads this file each cycle, so a
/// saved change takes effect on the next loop without a redeploy. Every
/// refusal is a 400 with `{"error": …}` the page shows as it is.
async fn api_settings_put(AxumState(st): AxumState<AppState>, body: String) -> Response {
    let path = st.dir.join("settings.json");
    let mut settings: RuntimeSettings = match serde_json::from_str(&body) {
        Ok(settings) => settings,
        Err(error) => return refuse(StatusCode::BAD_REQUEST, &format!("invalid settings: {error}")),
    };
    if let Err(error) = settings.validate() {
        return refuse(StatusCode::BAD_REQUEST, &error.to_string());
    }
    if let Err(problem) = pair_plex_credential(&mut settings, read_settings(&path).ok()) {
        return refuse(StatusCode::BAD_REQUEST, problem);
    }
    match write_settings(&path, &settings) {
        Ok(()) => (StatusCode::OK, "saved").into_response(),
        Err(error) => refuse(StatusCode::INTERNAL_SERVER_ERROR, &format!("write failed: {error}")),
    }
}

/// The Plex URL and token are one credential: a token saved for one server
/// must never be sent to another. The browser never holds the saved token, so
/// a blank one keeps it only while the URL stays the one it was saved with. An
/// unreadable saved file counts as a different URL: the token is asked again.
fn pair_plex_credential(settings: &mut RuntimeSettings, saved: Option<RuntimeSettings>) -> Result<(), &'static str> {
    trim_in_place(&mut settings.plex_url);
    trim_in_place(&mut settings.plex_token);
    if settings.plex_url.is_empty() {
        // No server, no credential: a token alone is never left on disk.
        settings.plex_token.clear();
        return Ok(());
    }
    if !settings.plex_token.is_empty() {
        return Ok(());
    }
    match saved {
        Some(saved) if saved.plex_url.trim() == settings.plex_url => {
            settings.plex_token = saved.plex_token;
            Ok(())
        }
        _ => Err("A new Plex URL needs its token: enter the Plex token again"),
    }
}

fn trim_in_place(text: &mut String) {
    text.truncate(text.trim_end().len());
    let leading = text.len() - text.trim_start().len();
    text.drain(..leading);
}

async fn api_history(AxumState(st): AxumState<AppState>) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/json")], read_json(&st.dir.join("history.json")))
}

/// TypeSafe's System One API (`POST /v1/systemone`), answered from the
/// published snapshot. The body is parsed by hand, not by axum's `Json`, so
/// every refusal is a 400 with `{"error": …}` whatever the content type.
async fn api_systemone(AxumState(st): AxumState<AppState>, body: String) -> Response {
    let started = std::time::Instant::now();
    let request: flinch_archive::systemone::Request = match serde_json::from_str(&body) {
        Ok(request) => request,
        Err(error) => return refuse(StatusCode::BAD_REQUEST, &format!("malformed request: {error}")),
    };
    let Some(items) = st.items.items().await else {
        return refuse(StatusCode::SERVICE_UNAVAILABLE, "no library snapshot yet: the daemon has not published items.json");
    };
    match systemone::answer(&items, &request) {
        Ok(answers) => axum::Json(flinch_archive::systemone::Response {
            model: systemone::model_name(&read_json(&st.dir.join("status.json"))),
            answers,
            latency_ms: Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
        })
        .into_response(),
        Err(error) => refuse(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

fn refuse(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(serde_json::json!({ "error": message }))).into_response()
}

/// The brand marks: the original, and a light-on-dark variant for the dark UI.
/// Small and harmless, but still only ever these two static files.
async fn logo(AxumState(st): AxumState<AppState>) -> Response {
    brand_mark(&st, "logo.png")
}

async fn logo_dark(AxumState(st): AxumState<AppState>) -> Response {
    brand_mark(&st, "logo-dark.png")
}

fn brand_mark(st: &AppState, name: &str) -> Response {
    match std::fs::read(st.web.join(name)) {
        Ok(bytes) => ([(header::CONTENT_TYPE, "image/png")], bytes).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "no logo").into_response(),
    }
}

async fn healthz() -> &'static str {
    "ok"
}

/// Ask the daemon for an immediate scan.
///
/// Cross-container signalling is a file on the shared volume — no queue, no
/// broker, no auth surface. The daemon consumes and deletes it at the top of
/// its loop, so a burst of clicks collapses into one run.
async fn api_run(AxumState(st): AxumState<AppState>) -> Response {
    let path = st.dir.join("run.now");
    match std::fs::write(&path, b"1") {
        Ok(()) => (StatusCode::ACCEPTED, "queued").into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("cannot queue a run: {error}"),
        )
            .into_response(),
    }
}

/// The SPA shell for any non-API path; React owns the rest of the routing.
async fn index(AxumState(st): AxumState<AppState>) -> Response {
    let mut buf = String::new();
    std::fs::File::open(st.web.join("index.html"))
        .and_then(|mut f| f.read_to_string(&mut buf))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response())
        .map(|_| {
            (
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                buf,
            )
                .into_response()
        })
        .unwrap_or_else(|r: Response| r)
}

/// Hashed build assets, served verbatim from the mounted dist dir.
async fn assets(AxumState(st): AxumState<AppState>, AxumPath(path): AxumPath<String>) -> Response {
    let Some(name) = Path::new(&path).file_name().map(|n| n.to_os_string()) else {
        return (StatusCode::NOT_FOUND, "missing asset").into_response();
    };
    let candidate = st.web.join("assets").join(name);
    match std::fs::read(&candidate) {
        Ok(bytes) => {
            let content_type = if candidate.extension().map(|e| e == "css").unwrap_or(false) {
                "text/css; charset=utf-8"
            } else if candidate.extension().map(|e| e == "svg").unwrap_or(false) {
                "image/svg+xml"
            } else {
                "application/javascript; charset=utf-8"
            };
            ([(header::CONTENT_TYPE, content_type)], bytes).into_response()
        }
        Err(err) => {
            eprintln!("[assets] read {:?} failed: {err}", candidate);
            (StatusCode::NOT_FOUND, "missing asset").into_response()
        }
    }
}

/// Any other path under `/api/`: a JSON 404 behind the token, not the SPA
/// shell, so the whole API namespace is guarded and a typo reads as one.
async fn api_unknown() -> Response {
    refuse(StatusCode::NOT_FOUND, "no such API endpoint")
}

/// Everything under `/api/` and `/v1/systemone` needs the token; the shell,
/// its assets, the logos and the health probe carry no data and stay open.
fn app(state: AppState) -> Router {
    let protected = Router::new()
        .route("/api/status", get(api_status))
        .route("/api/items", get(api_items))
        .route("/api/history", get(api_history))
        .route("/api/run", post(api_run))
        .route("/api/settings", get(api_settings_get).put(api_settings_put))
        .route("/api/{*rest}", any(api_unknown))
        .route(flinch_archive::systemone::PATH, post(api_systemone))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::require_token));
    Router::new()
        .route("/", get(index))
        .route("/healthz", get(healthz))
        .route("/assets/{path}", get(assets))
        .route("/logo.png", get(logo))
        .route("/logo-dark.png", get(logo_dark))
        .merge(protected)
        .fallback(get(index))
        .layer(middleware::map_response(auth::security_headers))
        .with_state(state)
}

#[tokio::main]
async fn main() -> Result<()> {
    let port = std::env::var("FLINCH_WEB_PORT").unwrap_or_else(|_| "7911".to_string());
    let dir = std::env::var("FLINCH_STATE_DIR").unwrap_or_else(|_| "state".to_string());
    let web = std::env::var("FLINCH_WEB_DIR").unwrap_or_else(|_| "web".to_string());
    let token = auth::configured_token(std::env::var("FLINCH_WEB_TOKEN").ok().as_deref());
    if token.is_some() {
        println!("flinch-web auth: token set");
    } else {
        eprintln!("flinch-web auth: FLINCH_WEB_TOKEN not set - the API refuses every request until it is");
    }
    let dir = PathBuf::from(dir);
    let items = Arc::new(snapshot::Snapshot::new(dir.join("items.json")));
    let app = app(AppState { dir: Arc::from(dir), web: Arc::from(PathBuf::from(web)), token, items });
    let addr = format!("0.0.0.0:{port}");
    println!("flinch-web on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await.context("bind")?;
    axum::serve(listener, app).await.context("serve")
}

#[cfg(test)]
mod tests;
