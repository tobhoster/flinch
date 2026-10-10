//! Checking the ID token the provider's token endpoint hands back.
//!
//! OpenID Connect Core 1.0 §3.1.3.7 lists the checks: the issuer is the one
//! configured, the audience holds FLINCH's client id (and `azp`, when sent, is
//! it), the token has not expired, and the `nonce` is the one this sign-in
//! sent. §3.1.3.7 lets a client that got the token straight from the token
//! endpoint over TLS skip the signature; FLINCH checks it anyway, against the
//! provider's published keys (RFC 7517 JWK Set), so a token that did not come
//! from the provider is refused even where the issuer is plain-http loopback.
//! Algorithms are those of RFC 7518 §3 that a provider signs with in practice;
//! `none` and the shared-secret HMAC ones are refused.

use base64::engine::{general_purpose::GeneralPurposeConfig, DecodePaddingMode, GeneralPurpose};
use base64::Engine;
use ring::signature::{self, RsaPublicKeyComponents, UnparsedPublicKey};
use serde_json::{Map, Value};

/// Base64url (RFC 4648 §5) as JWS (RFC 7515 §2) and JWK use it: unpadded,
/// though a padded value from a lenient provider is read too.
const BASE64URL: GeneralPurpose =
    GeneralPurpose::new(&base64::alphabet::URL_SAFE, GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent));

/// How far the provider's clock may run from FLINCH's.
const LEEWAY_SECS: i64 = 60;

/// The signing algorithms FLINCH checks (RFC 7518 §3.3–3.5, RFC 8037 §3.1).
const ACCEPTED: [&str; 9] = ["RS256", "RS384", "RS512", "PS256", "PS384", "PS512", "ES256", "ES384", "EdDSA"];

/// One key from the provider's `jwks_uri` (RFC 7517 §4, RFC 7518 §6).
#[derive(Debug, serde::Deserialize)]
pub struct Jwk {
    kty: String,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    alg: Option<String>,
    #[serde(default, rename = "use")]
    usage: Option<String>,
    #[serde(default)]
    crv: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
}

/// The JWK Set document: `{"keys": [...]}`.
#[derive(Debug, serde::Deserialize)]
pub struct JwkSet {
    pub keys: Vec<Jwk>,
}

/// What the token must say for this sign-in.
pub struct Expected<'a> {
    pub issuer: &'a str,
    pub client_id: &'a str,
    pub nonce: &'a str,
    pub now_unix: i64,
}

/// Why an ID token was refused. Never carries the token or a claim's value.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum IdTokenError {
    #[error("it is not a signed JWT (three base64url parts with a JSON header and claims)")]
    Malformed,
    #[error("it is signed with {0:?}, which FLINCH does not accept (RS256/384/512, PS256/384/512, ES256/384, EdDSA)")]
    Algorithm(String),
    #[error("none of the provider's published keys checks its signature")]
    Signature,
    #[error("its issuer is not FLINCH_WEB_OIDC_ISSUER")]
    Issuer,
    #[error("it was issued to another client (aud/azp is not FLINCH_WEB_OIDC_CLIENT_ID)")]
    Audience,
    #[error("it has expired, or is not valid yet (check both clocks)")]
    Expired,
    #[error("its nonce is not the one this sign-in sent")]
    Nonce,
    #[error("it names no subject")]
    Subject,
}

/// The token's claims once its signature and every §3.1.3.7 check passed.
pub fn verify(token: &str, keys: &[Jwk], expected: &Expected<'_>) -> Result<Map<String, Value>, IdTokenError> {
    let mut parts = token.split('.');
    let (Some(header), Some(payload), Some(signature), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(IdTokenError::Malformed);
    };
    let header: Map<String, Value> = decode_json(header)?;
    let signature = BASE64URL.decode(signature).map_err(|_| IdTokenError::Malformed)?;
    let alg = header.get("alg").and_then(Value::as_str).ok_or(IdTokenError::Malformed)?;
    if !ACCEPTED.contains(&alg) {
        return Err(IdTokenError::Algorithm(alg.chars().take(16).collect()));
    }
    let kid = header.get("kid").and_then(Value::as_str);
    let signed = &token.as_bytes()[..header_len(token)];
    if !keys.iter().filter(|key| fits(key, alg, kid)).any(|key| check_signature(alg, key, signed, &signature)) {
        return Err(IdTokenError::Signature);
    }
    let claims: Map<String, Value> = decode_json(payload)?;
    check_claims(&claims, expected)?;
    Ok(claims)
}

/// The length of `header.payload`, the bytes the signature covers.
fn header_len(token: &str) -> usize {
    token.rfind('.').unwrap_or(0)
}

fn decode_json(part: &str) -> Result<Map<String, Value>, IdTokenError> {
    let bytes = BASE64URL.decode(part).map_err(|_| IdTokenError::Malformed)?;
    serde_json::from_slice(&bytes).map_err(|_| IdTokenError::Malformed)
}

/// Whether `key` may check a signature made with `alg`: the token's `kid`
/// when it names one, a signing key, and the key's own `alg` when it states
/// one (RFC 7517 §4.4), so a key published for one algorithm is never used
/// for another.
fn fits(key: &Jwk, alg: &str, kid: Option<&str>) -> bool {
    kid.is_none_or(|kid| key.kid.as_deref() == Some(kid))
        && key.usage.as_deref().is_none_or(|usage| usage == "sig")
        && key.alg.as_deref().is_none_or(|key_alg| key_alg == alg)
}

/// Whether `key` checks the signature; a key of another type never does.
fn check_signature(alg: &str, key: &Jwk, signed: &[u8], signature: &[u8]) -> bool {
    let field = |value: &Option<String>| value.as_deref().and_then(|value| BASE64URL.decode(value).ok());
    let rsa = |params: &'static signature::RsaParameters| match (key.kty.as_str(), field(&key.n), field(&key.e)) {
        ("RSA", Some(n), Some(e)) => {
            // ring wants the numbers without leading zero bytes, which RFC 7518
            // §6.3.1 forbids but some providers still send.
            let unpadded = |bytes: &[u8]| bytes.iter().position(|byte| *byte != 0).unwrap_or(0);
            let (n, e) = (&n[unpadded(&n)..], &e[unpadded(&e)..]);
            RsaPublicKeyComponents { n, e }.verify(params, signed, signature).is_ok()
        }
        _ => false,
    };
    let ec = |crv: &str, width: usize, algorithm: &'static signature::EcdsaVerificationAlgorithm| {
        match (key.kty.as_str(), key.crv.as_deref(), field(&key.x), field(&key.y)) {
            ("EC", Some(curve), Some(x), Some(y)) if curve == crv && x.len() == width && y.len() == width => {
                // The uncompressed SEC1 point ring expects: 0x04 || x || y.
                let point = [&[0x04][..], &x, &y].concat();
                UnparsedPublicKey::new(algorithm, point).verify(signed, signature).is_ok()
            }
            _ => false,
        }
    };
    match alg {
        "RS256" => rsa(&signature::RSA_PKCS1_2048_8192_SHA256),
        "RS384" => rsa(&signature::RSA_PKCS1_2048_8192_SHA384),
        "RS512" => rsa(&signature::RSA_PKCS1_2048_8192_SHA512),
        "PS256" => rsa(&signature::RSA_PSS_2048_8192_SHA256),
        "PS384" => rsa(&signature::RSA_PSS_2048_8192_SHA384),
        "PS512" => rsa(&signature::RSA_PSS_2048_8192_SHA512),
        "ES256" => ec("P-256", 32, &signature::ECDSA_P256_SHA256_FIXED),
        "ES384" => ec("P-384", 48, &signature::ECDSA_P384_SHA384_FIXED),
        "EdDSA" => match (key.kty.as_str(), key.crv.as_deref(), field(&key.x)) {
            ("OKP", Some("Ed25519"), Some(x)) => UnparsedPublicKey::new(&signature::ED25519, x).verify(signed, signature).is_ok(),
            _ => false,
        },
        _ => false,
    }
}

/// OpenID Connect Core §3.1.3.7, items 2–3, 9–11, and §2's required claims.
fn check_claims(claims: &Map<String, Value>, expected: &Expected<'_>) -> Result<(), IdTokenError> {
    if claims.get("iss").and_then(Value::as_str) != Some(expected.issuer) {
        return Err(IdTokenError::Issuer);
    }
    let audience: Vec<&str> = match claims.get("aud") {
        Some(Value::String(one)) => vec![one.as_str()],
        Some(Value::Array(many)) => many.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    let azp = claims.get("azp").and_then(Value::as_str);
    let for_us = audience.contains(&expected.client_id)
        && azp.is_none_or(|azp| azp == expected.client_id)
        // Several audiences: the token must say it was issued for FLINCH.
        && (audience.len() == 1 || azp.is_some());
    if !for_us {
        return Err(IdTokenError::Audience);
    }
    let time = |name: &str| claims.get(name).and_then(Value::as_i64);
    let live = time("exp").is_some_and(|exp| exp + LEEWAY_SECS > expected.now_unix)
        && time("nbf").is_none_or(|nbf| nbf - LEEWAY_SECS <= expected.now_unix);
    if !live {
        return Err(IdTokenError::Expired);
    }
    let nonce = claims.get("nonce").and_then(Value::as_str).unwrap_or_default();
    if !crate::auth::same_bytes(nonce.as_bytes(), expected.nonce.as_bytes()) {
        return Err(IdTokenError::Nonce);
    }
    if claims.get("sub").and_then(Value::as_str).is_none_or(str::is_empty) {
        return Err(IdTokenError::Subject);
    }
    Ok(())
}
