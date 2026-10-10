//! Single sign-on through the whole app against a fake provider: the
//! redirect out, the callback, the session it opens, and every way a
//! callback is refused.

mod fake;
mod tokens;

use super::super::tests::LOGGED;
use super::*;
use crate::auth::Auth;
use crate::tests::{body_of, get, scratch, send, state_with};
use base64::Engine;
use fake::{lock, Fake, CLIENT_ID, SUBJECT};
use rstest::rstest;
use serde_json::json;
use std::path::Path;

/// The settings a test runs with: the fake's issuer and secret, and `extra`.
fn config(fake: &Fake, extra: &[(&str, &str)]) -> Config {
    let mut vars: HashMap<String, String> = [
        ("FLINCH_WEB_OIDC_ISSUER", fake.issuer.as_str()),
        ("FLINCH_WEB_OIDC_CLIENT_ID", CLIENT_ID),
        ("FLINCH_WEB_OIDC_CLIENT_SECRET", fake.secret.as_str()),
        ("FLINCH_WEB_OIDC_REDIRECT_URL", "https://flinch.example.org/api/oidc/callback"),
        ("FLINCH_WEB_OIDC_NAME", "Authelia"),
    ]
    .into_iter()
    .chain(extra.iter().copied())
    .map(|(name, value)| (name.to_string(), value.to_string()))
    .collect();
    vars.retain(|_, value| !value.is_empty());
    Config::from_vars(|name| vars.get(name).cloned()).expect("valid settings").expect("single sign-on set")
}

/// A server with single sign-on only: no password login, no API key.
fn sso_state(tmp: &Path, fake: &Fake, extra: &[(&str, &str)]) -> AppState {
    let sso = Sso::new(config(fake, extra)).expect("an HTTP client");
    state_with(tmp, Auth::new(None, None, None).with_sso(Some(sso)))
}

/// `GET /api/oidc/login`: the authorization request's query, and the
/// `name=value` of the state cookie. Tells the fake what the browser would
/// carry to its /authorize page.
async fn begin(st: &AppState, fake: &Fake, headers: &[(&str, &str)]) -> (HashMap<String, String>, String) {
    let res = send(st, get(LOGIN_PATH, headers)).await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    let location = url::Url::parse(res.headers()[header::LOCATION].to_str().unwrap()).unwrap();
    assert_eq!(location.path(), "/authorize");
    let query: HashMap<String, String> = location.query_pairs().into_owned().collect();
    *lock(&fake.flow) = Some((query["code_challenge"].clone(), query["nonce"].clone()));
    let cookie = res.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string();
    (query, cookie)
}

fn callback_to(code: &str, state: &str) -> String {
    format!("{CALLBACK_PATH}?code={code}&state={state}")
}

fn set_cookies(res: &Response) -> Vec<String> {
    res.headers().get_all(header::SET_COOKIE).iter().map(|value| value.to_str().unwrap().to_string()).collect()
}

/// The `flinch_session=<id>` a callback set, if it set one.
fn session_from(res: &Response) -> Option<String> {
    set_cookies(res).into_iter().find(|c| c.starts_with("flinch_session=")).map(|c| c.split(';').next().unwrap().to_string())
}

/// Where the hop page sends the browser.
async fn hops_to(res: Response) -> String {
    let body = body_of(res).await;
    let start = body.find("url=").expect("a meta refresh") + 4;
    body[start..start + body[start..].find('"').unwrap()].to_string()
}

#[tokio::test]
async fn a_sign_in_redirects_out_comes_back_and_opens_a_strict_session() {
    let tmp = scratch("sso-happy");
    let fake = fake::serve().await;
    let st = sso_state(&tmp, &fake, &[("FLINCH_WEB_OIDC_ALLOWED_EMAILS", "keeper@example.org")]);

    let (query, cookie) = begin(&st, &fake, &[]).await;
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["client_id"], CLIENT_ID);
    assert_eq!(query["redirect_uri"], "https://flinch.example.org/api/oidc/callback");
    assert_eq!(query["scope"], "openid email profile");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["tenant"], "home", "the endpoint's own query is kept");
    assert_eq!(cookie, format!("flinch_oidc={}", query["state"]));
    assert_ne!(query["state"], query["nonce"]);

    let res = send(&st, get(&callback_to(&fake.code, &query["state"]), &[("cookie", &cookie)])).await;
    assert_eq!(res.status(), StatusCode::OK);
    let cookies = set_cookies(&res);
    let session = session_from(&res).expect("a session cookie");
    assert!(
        cookies.iter().any(|c| c.starts_with("flinch_session=") && c.contains("; HttpOnly; SameSite=Strict; Max-Age=2592000")),
        "{cookies:?}"
    );
    assert!(cookies.contains(&"flinch_oidc=; Path=/api/oidc/; HttpOnly; SameSite=Lax; Max-Age=0".to_string()), "{cookies:?}");
    assert_eq!(hops_to(res).await, "/");

    // The session is the store's own: the API opens to it.
    assert_eq!(send(&st, get("/api/status", &[("cookie", &session)])).await.status(), StatusCode::OK);
    let token = lock(&fake.seen).iter().find(|seen| seen.path == "/token").cloned().expect("a token request");
    assert_eq!(token.form["grant_type"], "authorization_code");
    assert_eq!(token.form["redirect_uri"], "https://flinch.example.org/api/oidc/callback");
    assert!(!token.form.contains_key("client_secret"), "the secret goes in Basic, not the body");
    // RFC 6749 §2.3.1: the secret's space form-encoded before base64.
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("{CLIENT_ID}:{}", fake.secret.replace(' ', "+")));
    assert_eq!(token.authorization, Some(format!("Basic {basic}")));
    assert_eq!(fake.seen("/userinfo").len(), 1);

    let logged = LOGGED.with(|lines| lines.borrow().join("\n"));
    assert!(logged.contains("a single sign-on opened a session"), "{logged}");
    for secret in [&fake.code, &fake.access_token, &fake.secret, &query["state"], &query["nonce"], SUBJECT, "keeper@example.org"] {
        assert!(!logged.to_lowercase().contains(&secret.to_lowercase()), "the log holds a secret or an identity: {logged}");
    }
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn behind_https_both_cookies_are_secure() {
    let tmp = scratch("sso-https");
    let fake = fake::serve().await;
    let st = sso_state(&tmp, &fake, &[("FLINCH_WEB_OIDC_ALLOWED_SUBJECTS", SUBJECT)]);
    let https = ("x-forwarded-proto", "https");
    let res = send(&st, get(LOGIN_PATH, &[https])).await;
    assert!(res.headers()[header::SET_COOKIE].to_str().unwrap().ends_with("; SameSite=Lax; Max-Age=600; Secure"));
    let (query, cookie) = begin(&st, &fake, &[https]).await;
    let res = send(&st, get(&callback_to(&fake.code, &query["state"]), &[("cookie", &cookie), https])).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(set_cookies(&res).iter().all(|c| c.ends_with("; Secure")), "{:?}", set_cookies(&res));
    // A subject alone needs no userinfo.
    assert!(fake.seen("/userinfo").is_empty());
    std::fs::remove_dir_all(&tmp).ok();
}

/// What a callback may get wrong, and how the browser is sent back.
#[derive(Debug, Clone, Copy)]
enum Wrong {
    NoCookie,
    OtherBrowsersCookie,
    UnknownState,
    Replayed,
    Expired,
    ProviderSaysNo,
    OtherIssuerInCallback,
    BadCode,
    OtherNonce,
    OtherAudience,
    ExpiredToken,
    ForgedToken,
    TokenRedirects,
}

#[rstest]
#[case::no_cookie(Wrong::NoCookie, StatusCode::BAD_REQUEST)]
#[case::other_browser(Wrong::OtherBrowsersCookie, StatusCode::BAD_REQUEST)]
#[case::unknown_state(Wrong::UnknownState, StatusCode::BAD_REQUEST)]
#[case::replayed(Wrong::Replayed, StatusCode::BAD_REQUEST)]
#[case::expired(Wrong::Expired, StatusCode::BAD_REQUEST)]
#[case::provider_says_no(Wrong::ProviderSaysNo, StatusCode::BAD_REQUEST)]
#[case::other_issuer(Wrong::OtherIssuerInCallback, StatusCode::BAD_REQUEST)]
#[case::bad_code(Wrong::BadCode, StatusCode::BAD_GATEWAY)]
#[case::other_nonce(Wrong::OtherNonce, StatusCode::BAD_REQUEST)]
#[case::other_audience(Wrong::OtherAudience, StatusCode::BAD_REQUEST)]
#[case::expired_token(Wrong::ExpiredToken, StatusCode::BAD_REQUEST)]
#[case::forged_token(Wrong::ForgedToken, StatusCode::BAD_REQUEST)]
#[case::token_redirects(Wrong::TokenRedirects, StatusCode::BAD_GATEWAY)]
#[tokio::test]
async fn a_bad_callback_opens_no_session(#[case] wrong: Wrong, #[case] status: StatusCode) {
    let tmp = scratch("sso-refused");
    let fake = fake::serve().await;
    let st = sso_state(&tmp, &fake, &[("FLINCH_WEB_OIDC_ALLOWED_SUBJECTS", SUBJECT)]);
    let (query, mut cookie) = begin(&st, &fake, &[]).await;
    let mut path = callback_to(&fake.code, &query["state"]);
    match wrong {
        Wrong::NoCookie => cookie = "theme=dark".into(),
        Wrong::OtherBrowsersCookie => cookie = begin(&st, &fake, &[]).await.1,
        Wrong::UnknownState => path = callback_to(&fake.code, &fake::drawn()),
        Wrong::Replayed => assert_eq!(send(&st, get(&path, &[("cookie", &cookie)])).await.status(), StatusCode::OK),
        Wrong::Expired => lock(&st.auth.sso.as_ref().unwrap().pending).values_mut().for_each(|flow| flow.started -= FLOW_TTL),
        Wrong::ProviderSaysNo => path = format!("{CALLBACK_PATH}?error=access_denied&state={}", query["state"]),
        Wrong::OtherIssuerInCallback => path = format!("{path}&iss=https%3A%2F%2Fevil.example"),
        Wrong::BadCode => path = callback_to("not-the-code", &query["state"]),
        Wrong::OtherNonce => lock(&fake.id_claims).extend([("nonce".to_string(), json!("another"))]),
        Wrong::OtherAudience => lock(&fake.id_claims).extend([("aud".to_string(), json!("someone-else"))]),
        Wrong::ExpiredToken => lock(&fake.id_claims).extend([("exp".to_string(), json!(fake::now_unix() - 3600))]),
        Wrong::ForgedToken => *lock(&fake.forged) = true,
        Wrong::TokenRedirects => *lock(&fake.token_redirects) = true,
    }
    let res = send(&st, get(&path, &[("cookie", &cookie)])).await;
    assert_eq!(res.status(), status, "{wrong:?}");
    assert!(session_from(&res).is_none(), "{wrong:?}");
    assert!(set_cookies(&res).iter().any(|c| c.starts_with("flinch_oidc=; ")), "the state cookie is cleared");
    assert_eq!(hops_to(res).await, "/?sso=failed");
    let logged = LOGGED.with(|lines| lines.borrow().join("\n"));
    assert!(logged.contains("a single sign-on failed"), "{logged}");
    assert!(!logged.contains(&fake.code) && !logged.contains(&fake.access_token), "{logged}");
    if matches!(wrong, Wrong::TokenRedirects) {
        assert!(fake.seen("/elsewhere").is_empty(), "the redirect is never followed");
    }
    // No session but the replayed callback's first, good use.
    let sessions = st.auth.sessions().len();
    assert_eq!(sessions, usize::from(matches!(wrong, Wrong::Replayed)), "{wrong:?}");
    std::fs::remove_dir_all(&tmp).ok();
}

/// Claims from userinfo and the ID token, against each allow list.
#[rstest]
#[case::email_any_case(&[("FLINCH_WEB_OIDC_ALLOWED_EMAILS", "KEEPER@example.org")], json!({}), true)]
#[case::email_unverified(&[("FLINCH_WEB_OIDC_ALLOWED_EMAILS", "keeper@example.org")], json!({ "email_verified": false }), false)]
#[case::email_verified_missing(&[("FLINCH_WEB_OIDC_ALLOWED_EMAILS", "keeper@example.org")], json!({ "email_verified": null }), false)]
#[case::other_email(&[("FLINCH_WEB_OIDC_ALLOWED_EMAILS", "someone@example.org")], json!({}), false)]
#[case::group(&[("FLINCH_WEB_OIDC_ALLOWED_GROUPS", "admins, media")], json!({}), true)]
#[case::other_group(&[("FLINCH_WEB_OIDC_ALLOWED_GROUPS", "admins")], json!({}), false)]
#[case::own_claim(&[("FLINCH_WEB_OIDC_ALLOWED_GROUPS", "admins"), ("FLINCH_WEB_OIDC_GROUPS_CLAIM", "roles")], json!({ "roles": "admins" }), true)]
#[case::subject(&[("FLINCH_WEB_OIDC_ALLOWED_SUBJECTS", "someone, user-7f3a")], json!({}), true)]
#[case::nobody(&[], json!({}), false)]
#[tokio::test]
async fn only_an_allowed_account_gets_in(#[case] allow: &[(&str, &str)], #[case] userinfo: serde_json::Value, #[case] admitted: bool) {
    let tmp = scratch("sso-allow");
    let fake = fake::serve().await;
    {
        let mut info = lock(&fake.userinfo);
        for (name, value) in userinfo.as_object().unwrap() {
            match value {
                serde_json::Value::Null => drop(info.as_object_mut().unwrap().remove(name)),
                value => info[name] = value.clone(),
            }
        }
    }
    let st = sso_state(&tmp, &fake, allow);
    let (query, cookie) = begin(&st, &fake, &[]).await;
    let groups_wanted = allow.iter().any(|(name, _)| *name == "FLINCH_WEB_OIDC_ALLOWED_GROUPS");
    assert_eq!(query["scope"].ends_with(" groups"), groups_wanted);
    let res = send(&st, get(&callback_to(&fake.code, &query["state"]), &[("cookie", &cookie)])).await;
    assert_eq!(session_from(&res).is_some(), admitted);
    if admitted {
        assert_eq!(res.status(), StatusCode::OK);
    } else {
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        assert_eq!(hops_to(res).await, "/?sso=denied");
    }
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn userinfo_for_another_subject_is_refused() {
    let tmp = scratch("sso-userinfo-sub");
    let fake = fake::serve().await;
    lock(&fake.userinfo)["sub"] = json!("someone-else");
    let st = sso_state(&tmp, &fake, &[("FLINCH_WEB_OIDC_ALLOWED_EMAILS", "keeper@example.org")]);
    let (query, cookie) = begin(&st, &fake, &[]).await;
    let res = send(&st, get(&callback_to(&fake.code, &query["state"]), &[("cookie", &cookie)])).await;
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    assert!(session_from(&res).is_none());
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn a_provider_naming_another_issuer_is_never_used() {
    let tmp = scratch("sso-issuer");
    let fake = fake::serve().await;
    *lock(&fake.discovery_issuer) = Some("https://evil.example".into());
    let st = sso_state(&tmp, &fake, &[("FLINCH_WEB_OIDC_ALLOWED_SUBJECTS", SUBJECT)]);
    let res = send(&st, get(LOGIN_PATH, &[])).await;
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    assert!(res.headers().get(header::LOCATION).is_none());
    assert_eq!(hops_to(res).await, "/?sso=failed");
    let logged = LOGGED.with(|lines| lines.borrow().join("\n"));
    assert!(logged.contains("names issuer \"https://evil.example\""), "{logged}");
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn the_login_page_learns_of_the_button_and_the_password_login_still_works() {
    let tmp = scratch("sso-session");
    let fake = fake::serve().await;
    let password = fake::drawn();
    let sso = Sso::new(config(&fake, &[])).expect("an HTTP client");
    let st = state_with(&tmp, Auth::new(Some("keeper"), Some(&password), None).with_sso(Some(sso)));
    let button = json!({ "name": "Authelia", "login": "/api/oidc/login" });
    let session: serde_json::Value = serde_json::from_str(&body_of(send(&st, get("/api/session", &[])).await).await).unwrap();
    assert_eq!(session, json!({ "authenticated": false, "login_configured": true, "sso": button }));
    let refused: serde_json::Value = serde_json::from_str(&body_of(send(&st, get("/api/status", &[])).await).await).unwrap();
    assert_eq!(refused["sso"], button);
    let body = json!({ "username": "keeper", "password": password }).to_string();
    let login = crate::tests::request("POST", "/api/login", &[("x-flinch-request", "1")], body);
    assert_eq!(send(&st, login).await.status(), StatusCode::NO_CONTENT);
    std::fs::remove_dir_all(&tmp).ok();
}

#[tokio::test]
async fn without_sso_its_routes_say_so() {
    let tmp = scratch("sso-off");
    let st = crate::tests::state(&tmp, Some("s3cret"));
    for path in [LOGIN_PATH, CALLBACK_PATH] {
        assert_eq!(send(&st, get(path, &[])).await.status(), StatusCode::NOT_FOUND, "{path}");
    }
    std::fs::remove_dir_all(&tmp).ok();
}
