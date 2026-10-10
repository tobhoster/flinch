//! A fake OpenID Connect provider on a loopback port: discovery, token,
//! JWKS and userinfo, answering as the specifications describe, and an
//! ES256 key that signs its ID tokens. Codes, tokens and the client secret
//! are drawn per run, so a test can prove none of them reaches a log.

use super::super::super::new_session_id;
use axum::{
    extract::{Form, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

pub const CLIENT_ID: &str = "flinch";
pub const SUBJECT: &str = "user-7f3a";

pub fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// A lock that a panicked test thread left behind still opens.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn drawn() -> String {
    new_session_id().expect("the tests draw their secrets from /dev/urandom")
}

/// An ES256 key pair and its public JWK.
pub struct Signer {
    rng: SystemRandom,
    key: EcdsaKeyPair,
    pub kid: String,
}

impl Signer {
    pub fn new(kid: &str) -> Self {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).expect("generate a P-256 key");
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).expect("load the P-256 key");
        Self { rng, key, kid: kid.to_string() }
    }

    pub fn jwk(&self) -> Value {
        // The uncompressed point: 0x04 || x || y.
        let point = self.key.public_key().as_ref();
        json!({ "kty": "EC", "crv": "P-256", "kid": self.kid, "use": "sig", "alg": "ES256",
                "x": b64(&point[1..33]), "y": b64(&point[33..65]) })
    }

    /// A compact JWS of `claims` with this `header`.
    pub fn sign_with(&self, header: &Value, claims: &Value) -> String {
        let input = format!("{}.{}", b64(header.to_string().as_bytes()), b64(claims.to_string().as_bytes()));
        let signature = self.key.sign(&self.rng, input.as_bytes()).expect("sign");
        format!("{input}.{}", b64(signature.as_ref()))
    }

    pub fn sign(&self, claims: &Value) -> String {
        self.sign_with(&json!({ "alg": "ES256", "typ": "JWT", "kid": self.kid }), claims)
    }
}

/// One request the fake answered: path, `Authorization`, and form fields.
#[derive(Debug, Clone)]
pub struct Seen {
    pub path: &'static str,
    pub authorization: Option<String>,
    pub form: HashMap<String, String>,
}

pub struct Fake {
    pub issuer: String,
    pub signer: Signer,
    pub secret: String,
    pub code: String,
    pub access_token: String,
    pub seen: Mutex<Vec<Seen>>,
    /// The `code_challenge` and `nonce` the browser carried to /authorize.
    pub flow: Mutex<Option<(String, String)>>,
    /// Merged over the ID token's claims (`null` removes one).
    pub id_claims: Mutex<Map<String, Value>>,
    pub userinfo: Mutex<Value>,
    /// The issuer discovery names, when not `issuer`.
    pub discovery_issuer: Mutex<Option<String>>,
    /// The token endpoint answers with a redirect instead.
    pub token_redirects: Mutex<bool>,
    /// ID tokens are signed by a key the provider never published.
    pub forged: Mutex<bool>,
}

impl Fake {
    pub fn seen(&self, path: &str) -> Vec<Seen> {
        lock(&self.seen).iter().filter(|seen| seen.path == path).cloned().collect()
    }

    fn record(&self, path: &'static str, headers: &HeaderMap, form: HashMap<String, String>) {
        let authorization = headers.get(header::AUTHORIZATION).map(|value| value.to_str().unwrap().to_string());
        lock(&self.seen).push(Seen { path, authorization, form });
    }
}

/// Starts the fake on a loopback port; it lives as long as the test's runtime.
pub async fn serve() -> Arc<Fake> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind the fake provider");
    let issuer = format!("http://{}", listener.local_addr().expect("fake address"));
    let fake = Arc::new(Fake {
        issuer,
        signer: Signer::new("key-1"),
        secret: format!("secret {}", &drawn()[..16]),
        code: drawn(),
        access_token: drawn(),
        seen: Mutex::default(),
        flow: Mutex::default(),
        id_claims: Mutex::default(),
        userinfo: Mutex::new(json!({ "sub": SUBJECT, "email": "Keeper@Example.org", "email_verified": true, "groups": ["media"] })),
        discovery_issuer: Mutex::default(),
        token_redirects: Mutex::new(false),
        forged: Mutex::new(false),
    });
    let app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/token", post(token))
        .route("/jwks", get(jwks))
        .route("/userinfo", get(userinfo))
        .route("/elsewhere", any_elsewhere())
        .with_state(Arc::clone(&fake));
    tokio::spawn(async move { axum::serve(listener, app).await });
    fake
}

fn any_elsewhere() -> axum::routing::MethodRouter<Arc<Fake>> {
    axum::routing::any(|State(fake): State<Arc<Fake>>, headers: HeaderMap| async move {
        fake.record("/elsewhere", &headers, HashMap::new());
        Json(json!({ "access_token": "x", "id_token": "x" }))
    })
}

async fn discovery(State(fake): State<Arc<Fake>>, headers: HeaderMap) -> Json<Value> {
    fake.record("/.well-known/openid-configuration", &headers, HashMap::new());
    let issuer = lock(&fake.discovery_issuer).clone().unwrap_or_else(|| fake.issuer.clone());
    let base = &fake.issuer;
    Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{base}/authorize?tenant=home"),
        "token_endpoint": format!("{base}/token"),
        "jwks_uri": format!("{base}/jwks"),
        "userinfo_endpoint": format!("{base}/userinfo"),
        "response_types_supported": ["code"],
        "code_challenge_methods_supported": ["S256"],
    }))
}

/// RFC 6749 §4.1.3 and RFC 7636 §4.6: the code, the redirect URI and the
/// verifier's S256 must match, or `invalid_grant`.
async fn token(State(fake): State<Arc<Fake>>, headers: HeaderMap, Form(form): Form<HashMap<String, String>>) -> Response {
    fake.record("/token", &headers, form.clone());
    if *lock(&fake.token_redirects) {
        return (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, format!("{}/elsewhere", fake.issuer))]).into_response();
    }
    let Some((challenge, nonce)) = lock(&fake.flow).clone() else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid_request" }))).into_response();
    };
    let verifier = form.get("code_verifier").cloned().unwrap_or_default();
    let digest = ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes());
    let grant_ok = form.get("grant_type").map(String::as_str) == Some("authorization_code")
        && form.get("code") == Some(&fake.code)
        && b64(digest.as_ref()) == challenge;
    if !grant_ok {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid_grant", "error_description": fake.code }))).into_response();
    }
    let now = now_unix();
    let mut claims = json!({ "iss": fake.issuer, "aud": CLIENT_ID, "sub": SUBJECT, "exp": now + 300, "iat": now, "nonce": nonce });
    for (name, value) in lock(&fake.id_claims).iter() {
        match value {
            Value::Null => {
                claims.as_object_mut().unwrap().remove(name);
            }
            value => claims[name] = value.clone(),
        }
    }
    let id_token = if *lock(&fake.forged) { Signer::new(&fake.signer.kid).sign(&claims) } else { fake.signer.sign(&claims) };
    Json(json!({ "access_token": fake.access_token, "token_type": "Bearer", "expires_in": 300, "id_token": id_token })).into_response()
}

async fn jwks(State(fake): State<Arc<Fake>>, headers: HeaderMap) -> Json<Value> {
    fake.record("/jwks", &headers, HashMap::new());
    Json(json!({ "keys": [fake.signer.jwk()] }))
}

async fn userinfo(State(fake): State<Arc<Fake>>, headers: HeaderMap) -> Response {
    fake.record("/userinfo", &headers, HashMap::new());
    let expected = format!("Bearer {}", fake.access_token);
    if headers.get(header::AUTHORIZATION).and_then(|value| value.to_str().ok()) != Some(expected.as_str()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(lock(&fake.userinfo).clone()).into_response()
}
