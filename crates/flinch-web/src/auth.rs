//! Who may read the API, and what every response tells the browser.
//!
//! The API hands out the library, the deletion plan and the settings, and
//! takes writes that decide what Maintainerr deletes. One shared bearer token
//! (`FLINCH_WEB_TOKEN`) guards it; a server without one refuses everything
//! rather than serving an open homelab endpoint.

use crate::AppState;
use axum::{
    extract::{Request, State as AxumState},
    http::{header, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

const CONTENT_SECURITY_POLICY: &str =
    "default-src 'self'; img-src 'self' https: data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'";

/// The token from the environment, trimmed; blank counts as unset so a
/// `FLINCH_WEB_TOKEN=` line cannot open the API with an empty password.
pub fn configured_token(raw: Option<&str>) -> Option<std::sync::Arc<str>> {
    let token = raw?.trim();
    (!token.is_empty()).then(|| std::sync::Arc::from(token))
}

/// Admits a request only with `Authorization: Bearer <FLINCH_WEB_TOKEN>`.
pub async fn require_token(AxumState(st): AxumState<AppState>, request: Request, next: Next) -> Response {
    let Some(expected) = st.token.as_deref() else {
        return unauthorized(
            "FLINCH_WEB_TOKEN is not set on the server, so the API refuses every request: set it and restart flinch-web",
            false,
        );
    };
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.as_bytes().strip_prefix(b"Bearer "));
    match presented {
        Some(token) if same_bytes(token, expected.as_bytes()) => next.run(request).await,
        Some(_) => unauthorized("That token was not accepted: enter the FLINCH web token again", true),
        None => unauthorized("This FLINCH is locked: enter its web token", true),
    }
}

fn unauthorized(message: &str, configured: bool) -> Response {
    let body = axum::Json(serde_json::json!({ "error": message, "configured": configured }));
    let challenge = [(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer realm=\"flinch\""))];
    (StatusCode::UNAUTHORIZED, challenge, body).into_response()
}

/// Equality that looks at every byte whatever the first difference, so the
/// response time does not reveal how much of a guessed token was right. The
/// length is not secret: guessing it gets an attacker no closer to the token.
fn same_bytes(presented: &[u8], expected: &[u8]) -> bool {
    if presented.len() != expected.len() {
        return false;
    }
    let difference = presented.iter().zip(expected).fold(0u8, |acc, (a, b)| acc | (a ^ b));
    // Keeps the optimiser from turning the fold back into an early exit.
    std::hint::black_box(difference) == 0
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
mod tests {
    use super::*;
    use crate::tests::{body_of, get, scratch, send, state};
    use rstest::rstest;

    #[rstest]
    #[case::no_header(Some("s3cret"), None, StatusCode::UNAUTHORIZED, Some(true))]
    #[case::wrong_token(Some("s3cret"), Some("Bearer s3cre7"), StatusCode::UNAUTHORIZED, Some(true))]
    #[case::token_prefix(Some("s3cret"), Some("Bearer s3cre"), StatusCode::UNAUTHORIZED, Some(true))]
    #[case::not_a_bearer(Some("s3cret"), Some("Basic s3cret"), StatusCode::UNAUTHORIZED, Some(true))]
    #[case::right_token(Some("s3cret"), Some("Bearer s3cret"), StatusCode::OK, None)]
    // No token on the server: closed, even to a caller who sends an empty one.
    #[case::unconfigured(None, Some("Bearer "), StatusCode::UNAUTHORIZED, Some(false))]
    #[tokio::test]
    async fn the_api_answers_only_the_configured_token(
        #[case] server: Option<&str>,
        #[case] authorization: Option<&str>,
        #[case] expected: StatusCode,
        #[case] configured: Option<bool>,
    ) {
        let tmp = scratch("auth");
        let st = state(&tmp, server);
        for path in ["/api/status", "/api/settings", "/api/nothing-here"] {
            let res = send(&st, get(path, authorization)).await;
            let wanted = if path == "/api/nothing-here" && expected == StatusCode::OK { StatusCode::NOT_FOUND } else { expected };
            assert_eq!(res.status(), wanted, "{path}");
            if let Some(configured) = configured {
                assert_eq!(res.headers()[header::WWW_AUTHENTICATE], "Bearer realm=\"flinch\"");
                let body: serde_json::Value = serde_json::from_str(&body_of(res).await).unwrap();
                assert_eq!(body["configured"], configured, "{path}");
                assert!(body["error"].is_string());
            }
        }
        let systemone = axum::http::Request::post(flinch_archive::systemone::PATH)
            .body(axum::body::Body::from(r#"{"state": "radarr-1", "questions": {}}"#))
            .unwrap();
        assert_eq!(send(&st, systemone).await.status(), StatusCode::UNAUTHORIZED);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn public_routes_stay_open_and_every_response_carries_the_security_headers() {
        let tmp = std::env::temp_dir().join(format!("fw-{}-public", std::process::id()));
        std::fs::create_dir_all(tmp.join("web")).unwrap();
        std::fs::write(tmp.join("web/index.html"), "<div id=\"root\"></div>").unwrap();
        let st = state(&tmp, Some("s3cret"));
        for (path, expected) in [("/healthz", StatusCode::OK), ("/", StatusCode::OK), ("/items", StatusCode::OK), ("/api/status", StatusCode::UNAUTHORIZED)] {
            let res = send(&st, get(path, None)).await;
            assert_eq!(res.status(), expected, "{path}");
            let headers = res.headers();
            assert_eq!(headers[header::CONTENT_SECURITY_POLICY], CONTENT_SECURITY_POLICY, "{path}");
            assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY", "{path}");
            assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff", "{path}");
            assert_eq!(headers[header::REFERRER_POLICY], "no-referrer", "{path}");
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[rstest]
    #[case::unset(None, None)]
    #[case::blank(Some("  \n"), None)]
    #[case::trimmed(Some(" s3cret\n"), Some("s3cret"))]
    fn a_blank_environment_token_leaves_the_api_closed(#[case] raw: Option<&str>, #[case] token: Option<&str>) {
        assert_eq!(configured_token(raw).as_deref(), token);
    }
}
