use crate::tests::{body_of, get, request, scratch, send, state, TOKEN};
use crate::AppState;
use axum::body::Body;
use axum::http::StatusCode;
use flinch_archive::requests::link::{Action, Link, LinkSecret};
use flinch_archive::requests::{RequestBook, Status};
use rstest::rstest;
use std::sync::Arc;

const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A server with links on and household self-service `enabled`; the
/// library holds `radarr-1`, "A Movie".
fn household(label: &str, enabled: bool) -> AppState {
    let tmp = scratch(label);
    let st = AppState { links: LinkSecret::new(SECRET).map(Arc::new), ..state(&tmp, Some(TOKEN)) };
    std::fs::write(st.dir.join("settings.json"), format!(r#"{{"household": {{"enabled": {enabled}}}}}"#)).expect("settings");
    st
}

fn token(card: &str, action: Action, expires: u64) -> String {
    let link = Link { card_id: card.to_string(), action, expires, by: "Ann".to_string() };
    LinkSecret::new(SECRET).map(|secret| secret.sign(&link)).unwrap_or_default()
}

fn post(path: &str) -> axum::http::Request<Body> {
    request("POST", path, &[], Body::empty())
}

#[tokio::test]
async fn a_keep_link_shows_the_title_without_a_login_and_one_click_keeps_it_once() {
    let st = household("keep", true);
    let path = format!("/r/{}", token("radarr-1", Action::Keep, now() + 86_400));

    let shown = send(&st, get(&path, &[])).await;
    assert_eq!(shown.status(), StatusCode::OK);
    let html = body_of(shown).await;
    assert!(html.contains("A Movie") && html.contains("<form method=\"post\">"), "{html}");

    assert_eq!(send(&st, post(&path)).await.status(), StatusCode::OK);
    let book = RequestBook::read(&st.dir);
    assert_eq!(book.requests.len(), 1);
    assert_eq!((book.requests[0].kind, book.requests[0].status, book.requests[0].by.as_str()), (Action::Keep, Status::Active, "Ann"));
    assert!(st.dir.join("run.now").exists(), "a keep wakes the daemon");

    let replay = send(&st, post(&path)).await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(RequestBook::read(&st.dir), book, "a replay changes nothing");
}

#[rstest]
#[case::expired(token("radarr-1", Action::Keep, now() - 1), true, true, StatusCode::GONE)]
#[case::tampered(token("radarr-1", Action::Keep, now() + 60).replacen('.', "x.", 1), true, true, StatusCode::NOT_FOUND)]
#[case::not_in_the_library(token("radarr-9", Action::Keep, now() + 60), true, true, StatusCode::NOT_FOUND)]
#[case::self_service_off(token("radarr-1", Action::Keep, now() + 60), false, true, StatusCode::NOT_FOUND)]
#[case::no_secret(token("radarr-1", Action::Keep, now() + 60), true, false, StatusCode::NOT_FOUND)]
#[tokio::test]
async fn a_link_that_cannot_act_says_why_and_writes_nothing(
    #[case] token: String,
    #[case] enabled: bool,
    #[case] secret: bool,
    #[case] expected: StatusCode,
) {
    let st = household(&format!("refused-{}", expected.as_u16()), enabled);
    let st = if secret { st } else { AppState { links: None, ..st } };
    assert_eq!(send(&st, get(&format!("/r/{token}"), &[])).await.status(), expected);
    assert_eq!(send(&st, post(&format!("/r/{token}"))).await.status(), expected);
    assert!(!st.dir.join("requests.json").exists());
}

#[tokio::test]
async fn a_removal_waits_for_the_admin_who_approves_it_behind_the_login() {
    let st = household("remove", true);
    let path = format!("/r/{}", token("radarr-1", Action::Remove, now() + 86_400));
    assert_eq!(send(&st, post(&path)).await.status(), StatusCode::OK);
    let id = RequestBook::read(&st.dir).requests[0].id.clone();
    assert_eq!(RequestBook::read(&st.dir).requests[0].status, Status::Pending);

    let approve = format!("/api/requests/{id}/approve");
    assert_eq!(send(&st, post(&approve)).await.status(), StatusCode::UNAUTHORIZED, "the queue is the admin's");
    let auth = format!("Bearer {TOKEN}");
    let listed = body_of(send(&st, get("/api/requests", &[("authorization", auth.as_str())])).await).await;
    assert!(listed.contains("\"status\":\"pending\""), "{listed}");

    let approved = send(&st, request("POST", &approve, &[("authorization", auth.as_str())], Body::empty())).await;
    assert_eq!(approved.status(), StatusCode::OK);
    assert_eq!(RequestBook::read(&st.dir).requests[0].status, Status::Approved);
    let again = send(&st, request("POST", &approve, &[("authorization", auth.as_str())], Body::empty())).await;
    assert_eq!(again.status(), StatusCode::CONFLICT, "a decided request stays decided");
    let unknown = send(&st, request("POST", "/api/requests/nope/deny", &[("authorization", auth.as_str())], Body::empty())).await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_link_names_one_instances_copy_and_its_approval_rules_only_that_card() {
    let st = household("named", true);
    let library = r#"[{"id":"radarr-1","title":"A Movie","kind":"movie"},{"id":"radarr@4k-1","title":"A Movie (4K)","kind":"movie"}]"#;
    std::fs::write(st.dir.join("items.json"), library).expect("items");
    let path = format!("/r/{}", token("radarr@4k-1", Action::Remove, now() + 86_400));

    let shown = body_of(send(&st, get(&path, &[])).await).await;
    assert!(shown.contains("A Movie (4K)"), "{shown}");
    assert_eq!(send(&st, post(&path)).await.status(), StatusCode::OK);
    let id = RequestBook::read(&st.dir).requests[0].id.clone();
    let auth = format!("Bearer {TOKEN}");
    let approve = request("POST", &format!("/api/requests/{id}/approve"), &[("authorization", auth.as_str())], Body::empty());
    assert_eq!(send(&st, approve).await.status(), StatusCode::OK);

    let book = RequestBook::read(&st.dir);
    assert_eq!(book.requests[0].card_id, "radarr@4k-1");
    let rules = book.rules(now());
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].scope.ids, ["radarr@4k-1"], "the HD copy is not the one asked about");
}
