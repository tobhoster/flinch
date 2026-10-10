//! The four calls FLINCH makes to the OpenID Connect provider: discovery,
//! the token exchange, the signing keys and userinfo. Every shape here is the
//! one the specifications define:
//! - discovery: OpenID Connect Discovery 1.0 §4 (`{issuer}/.well-known/openid-configuration`)
//!   and §3 (the metadata fields); §4.3 requires the document's `issuer` to be
//!   exactly the configured one.
//! - token exchange: RFC 6749 §4.1.3 with PKCE's `code_verifier` (RFC 7636
//!   §4.5); the client authenticates with HTTP Basic (RFC 6749 §2.3.1, the
//!   default), or in the form body when discovery lists only
//!   `client_secret_post`; a client without a secret is a public client and
//!   sends only its id.
//! - keys: RFC 7517 §5, a JWK Set at `jwks_uri`.
//! - userinfo: OpenID Connect Core 1.0 §5.3, `GET` with the access token as
//!   a bearer token (RFC 6750 §2.1); §5.3.4 requires its `sub` to be the ID
//!   token's.
//!
//! No response body, token or code is ever put into an error: only which call
//! failed, its status and the provider's OAuth `error` code (RFC 6749 §5.2).

use super::jwt::JwkSet;
use base64::Engine;
use serde_json::{Map, Value};

/// Why a call to the provider failed. Safe to log: no token, code or body.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("cannot reach the provider's {what}: {source}")]
    Unreachable {
        what: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("cannot read the provider's {what}: {source}")]
    Body {
        what: &'static str,
        #[source]
        source: flinch_archive::body::BodyError,
    },
    #[error("the provider's {what} answered HTTP {status}{code}")]
    Status { what: &'static str, status: u16, code: String },
    #[error("the provider's {what} is not what OpenID Connect describes: {reason}")]
    Malformed { what: &'static str, reason: &'static str },
    #[error("the provider's discovery names issuer {found:?}, not FLINCH_WEB_OIDC_ISSUER {expected:?}")]
    Issuer { found: String, expected: String },
}

/// The provider's metadata FLINCH uses (Discovery §3).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    #[serde(default)]
    pub userinfo_endpoint: Option<String>,
    #[serde(default)]
    pub token_endpoint_auth_methods_supported: Option<Vec<String>>,
}

/// What the token endpoint hands back (RFC 6749 §5.1, OIDC Core §3.1.3.3).
/// Never `Debug`: it holds the tokens.
#[derive(serde::Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub id_token: String,
}

/// The client's credentials as the token endpoint takes them.
pub struct Client<'a> {
    pub id: &'a str,
    pub secret: Option<&'a str>,
    pub redirect_url: &'a str,
}

/// Whether FLINCH will send anything to `url`: HTTPS, or plain HTTP to this
/// machine only (a provider on localhost, the tests' fake), never with a
/// user or password in it.
pub fn reachable(raw: &str) -> Result<reqwest::Url, &'static str> {
    let url = reqwest::Url::parse(raw).map_err(|_| "is not a URL")?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("carries a user or password");
    }
    let loopback = match url.host() {
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    match url.scheme() {
        "https" => Ok(url),
        "http" if loopback => Ok(url),
        _ => Err("is not https:// (plain http:// is accepted for localhost only)"),
    }
}

/// `{issuer}/.well-known/openid-configuration`, the issuer's trailing `/`
/// removed first (Discovery §4.1), checked against the issuer (§4.3) and
/// with every endpoint [`reachable`].
pub async fn discover(http: &reqwest::Client, issuer: &str) -> Result<Discovery, ProviderError> {
    const WHAT: &str = "discovery document";
    let url = format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/'));
    let discovery: Discovery = json(WHAT, http.get(url)).await?;
    if discovery.issuer != issuer {
        return Err(ProviderError::Issuer { found: discovery.issuer, expected: issuer.to_string() });
    }
    let endpoints = [&discovery.authorization_endpoint, &discovery.token_endpoint, &discovery.jwks_uri];
    if endpoints.into_iter().chain(discovery.userinfo_endpoint.as_ref()).any(|url| reachable(url).is_err()) {
        return Err(ProviderError::Malformed { what: WHAT, reason: "an endpoint is not https:// (or http:// on localhost)" });
    }
    Ok(discovery)
}

/// Trades the authorization code for tokens.
pub async fn redeem(
    http: &reqwest::Client,
    discovery: &Discovery,
    client: &Client<'_>,
    code: &str,
    verifier: &str,
) -> Result<Tokens, ProviderError> {
    let mut form =
        vec![("grant_type", "authorization_code"), ("code", code), ("redirect_uri", client.redirect_url), ("code_verifier", verifier)];
    let mut request = http.post(&discovery.token_endpoint);
    match client.secret {
        Some(secret) if posts_secret(discovery) => form.extend([("client_id", client.id), ("client_secret", secret)]),
        Some(secret) => request = request.header(reqwest::header::AUTHORIZATION, basic(client.id, secret)),
        None => form.push(("client_id", client.id)),
    }
    json("token endpoint", request.form(&form)).await
}

/// `client_secret_post` only when discovery lists it and not
/// `client_secret_basic`, which is the default (Discovery §3).
fn posts_secret(discovery: &Discovery) -> bool {
    discovery
        .token_endpoint_auth_methods_supported
        .as_ref()
        .is_some_and(|methods| methods.iter().any(|m| m == "client_secret_post") && !methods.iter().any(|m| m == "client_secret_basic"))
}

/// RFC 6749 §2.3.1: both halves form-urlencoded, then joined and base64'd.
fn basic(id: &str, secret: &str) -> String {
    let encode = |value: &str| url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>();
    let pair = format!("{}:{}", encode(id), encode(secret));
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(pair))
}

/// The provider's signing keys, read fresh for every sign-in, so a rotated
/// key is never missed.
pub async fn keys(http: &reqwest::Client, discovery: &Discovery) -> Result<JwkSet, ProviderError> {
    json("JWK Set", http.get(&discovery.jwks_uri)).await
}

/// The signed-in account's claims from userinfo, when the provider has the
/// endpoint; its `sub` must be the ID token's.
pub async fn userinfo(
    http: &reqwest::Client,
    discovery: &Discovery,
    access_token: &str,
    subject: &str,
) -> Result<Option<Map<String, Value>>, ProviderError> {
    const WHAT: &str = "userinfo endpoint";
    let Some(endpoint) = &discovery.userinfo_endpoint else { return Ok(None) };
    let claims: Map<String, Value> = json(WHAT, http.get(endpoint).bearer_auth(access_token)).await?;
    if claims.get("sub").and_then(Value::as_str) != Some(subject) {
        return Err(ProviderError::Malformed { what: WHAT, reason: "its sub is not the ID token's" });
    }
    Ok(Some(claims))
}

/// Sends `request` and parses a 2xx JSON answer. A refusal names its OAuth
/// `error` code when it has one; a parse failure never quotes the body.
async fn json<T: serde::de::DeserializeOwned>(what: &'static str, request: reqwest::RequestBuilder) -> Result<T, ProviderError> {
    let response = request
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|source| ProviderError::Unreachable { what, source })?;
    let status = response.status();
    let body = flinch_archive::body::read(response).await.map_err(|source| ProviderError::Body { what, source })?;
    if !status.is_success() {
        return Err(ProviderError::Status { what, status: status.as_u16(), code: oauth_error(&body) });
    }
    serde_json::from_slice(&body).map_err(|_| ProviderError::Malformed { what, reason: "not the JSON fields expected" })
}

/// `" (invalid_grant)"` from an RFC 6749 §5.2 error body, kept to the
/// characters an error code may use, or nothing.
fn oauth_error(body: &[u8]) -> String {
    let parsed: Option<Map<String, Value>> = serde_json::from_slice(body).ok();
    let code = parsed.as_ref().and_then(|map| map.get("error")).and_then(Value::as_str).unwrap_or_default();
    let clean: String = code.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').take(64).collect();
    if clean.is_empty() {
        String::new()
    } else {
        format!(" ({clean})")
    }
}
