//! The ID token checks and the settings, each on their own.

use super::super::config::{Config, ConfigError};
use super::super::jwt::{self, IdTokenError, Jwk};
use super::fake::{b64, now_unix, Signer, CLIENT_ID, SUBJECT};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use rstest::rstest;
use serde_json::{json, Value};
use std::collections::HashMap;

const ISSUER: &str = "https://auth.example.org";
const NONCE: &str = "n-0S6_WzA2Mj";

fn keys(jwks: &[Value]) -> Vec<Jwk> {
    jwks.iter().map(|jwk| serde_json::from_value(jwk.clone()).expect("a JWK")).collect()
}

fn claims() -> Value {
    json!({ "iss": ISSUER, "aud": CLIENT_ID, "sub": SUBJECT, "exp": now_unix() + 300, "iat": now_unix(), "nonce": NONCE })
}

fn verify(token: &str, jwks: &[Value]) -> Result<(), IdTokenError> {
    let expected = jwt::Expected { issuer: ISSUER, client_id: CLIENT_ID, nonce: NONCE, now_unix: now_unix() };
    jwt::verify(token, &keys(jwks), &expected).map(|claims| assert_eq!(claims["sub"], SUBJECT))
}

/// `claims()` with `changes` applied (`null` removes a claim).
fn with(changes: Value) -> Value {
    let mut claims = claims();
    for (name, value) in changes.as_object().unwrap() {
        match value {
            Value::Null => drop(claims.as_object_mut().unwrap().remove(name)),
            value => claims[name] = value.clone(),
        }
    }
    claims
}

#[rstest]
#[case::as_issued(json!({}), Ok(()))]
#[case::clock_a_little_behind(json!({ "exp": now_unix() - 30 }), Ok(()))]
#[case::several_audiences_for_us(json!({ "aud": ["other", CLIENT_ID], "azp": CLIENT_ID }), Ok(()))]
#[case::other_issuer(json!({ "iss": "https://auth.example.org/" }), Err(IdTokenError::Issuer))]
#[case::other_audience(json!({ "aud": "other" }), Err(IdTokenError::Audience))]
#[case::several_audiences_no_azp(json!({ "aud": ["other", CLIENT_ID] }), Err(IdTokenError::Audience))]
#[case::other_azp(json!({ "azp": "other" }), Err(IdTokenError::Audience))]
#[case::expired(json!({ "exp": now_unix() - 120 }), Err(IdTokenError::Expired))]
#[case::no_expiry(json!({ "exp": null }), Err(IdTokenError::Expired))]
#[case::not_yet(json!({ "nbf": now_unix() + 600 }), Err(IdTokenError::Expired))]
#[case::other_nonce(json!({ "nonce": "replayed" }), Err(IdTokenError::Nonce))]
#[case::no_nonce(json!({ "nonce": null }), Err(IdTokenError::Nonce))]
#[case::no_subject(json!({ "sub": "" }), Err(IdTokenError::Subject))]
fn the_claims_must_be_for_this_sign_in(#[case] changes: Value, #[case] expected: Result<(), IdTokenError>) {
    let signer = Signer::new("k1");
    assert_eq!(verify(&signer.sign(&with(changes)), &[signer.jwk()]), expected);
}

#[test]
fn an_ed25519_signature_checks_too() {
    let rng = SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let key = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let input = format!("{}.{}", b64(json!({ "alg": "EdDSA" }).to_string().as_bytes()), b64(claims().to_string().as_bytes()));
    let token = format!("{input}.{}", b64(key.sign(input.as_bytes()).as_ref()));
    let jwk = json!({ "kty": "OKP", "crv": "Ed25519", "x": b64(key.public_key().as_ref()) });
    assert_eq!(verify(&token, &[jwk]), Ok(()));
}

/// RFC 7515 Appendix A.2: an RS256 JWS and its key. Its claims are not an ID
/// token's (`"iss":"joe"`), so a checked signature shows as the issuer refusal.
const RFC7515_A2_TOKEN: &str = concat!(
    "eyJhbGciOiJSUzI1NiJ9",
    ".eyJpc3MiOiJqb2UiLA0KICJleHAiOjEzMDA4MTkzODAsDQogImh0dHA6Ly9leGFtcGxlLmNvbS9pc19yb290Ijp0cnVlfQ",
    ".cC4hiUPoj9Eetdgtv3hF80EGrhuB__dzERat0XF9g2VtQgr9PJbu3XOiZj5RZmh7AAuHIm4Bh-0Qc_lF5YKt_O8W2Fp5jujGbds9uJdbF9CUAr7t1dnZcAcQjbKBYNX4",
    "BAynRFdiuB--f_nZLgrnbyTyWzO75vRK5h6xBArLIARNPvkSjtQBMHlb1L07Qe7K0GarZRmB_eSN9383LcOLn6_dO--xi12jzDwusC-eOkHWEsqtFZESc6BfI7noOPqv",
    "hJ1phCnvWh6IeYI2w9QOYEUipUTI8np6LbgGY9Fs98rqVt5AXLIhWkWywlVmtVrBp0igcN_IoypGlUPQGe77Rw",
);
const RFC7515_A2_N: &str = concat!(
    "ofgWCuLjybRlzo0tZWJjNiuSfb4p4fAkd_wWJcyQoTbji9k0l8W26mPddxHmfHQp-Vaw-4qPCJrcS2mJPMEzP1Pt0Bm4d4QlL-yRT-SFd2lZS-pCgNMs",
    "D1W_YpRPEwOWvG6b32690r2jZ47soMZo9wGzjb_7OMg0LOL-bSf63kpaSHSXndS5z5rexMdbBYUsLA9e-KXBdQOS-UTo7WTBEMa2R2CapHg665xsmtdV",
    "MTBQY4uDZlxvb3qCo5ZwKh9kG4LT6_I5IhlJH7aGhyxXFvUK-DWNmoudF8NAco9_h9iaGNj8q2ethFkMLs91kzk2PAcDTW9gb54h4FRWyuXpoQ",
);

#[test]
fn an_rs256_signature_checks_against_the_rfc_example_key() {
    use base64::Engine;
    let jwk = json!({ "kty": "RSA", "n": RFC7515_A2_N, "e": "AQAB" });
    assert_eq!(verify(RFC7515_A2_TOKEN, std::slice::from_ref(&jwk)), Err(IdTokenError::Issuer));
    // A modulus sent with a leading zero byte is the same key.
    let n = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(RFC7515_A2_N).unwrap();
    let padded = json!({ "kty": "RSA", "n": b64(&[&[0u8][..], &n].concat()), "e": "AQAB" });
    assert_eq!(verify(RFC7515_A2_TOKEN, &[padded]), Err(IdTokenError::Issuer));
    // As PS256, or with one signature byte changed, it does not check.
    let as_pss = RFC7515_A2_TOKEN.replacen("eyJhbGciOiJSUzI1NiJ9", &b64(br#"{"alg":"PS256"}"#), 1);
    assert_eq!(verify(&as_pss, std::slice::from_ref(&jwk)), Err(IdTokenError::Signature));
    let tampered = RFC7515_A2_TOKEN.replacen(".cC4h", ".dC4h", 1);
    assert_eq!(verify(&tampered, &[jwk]), Err(IdTokenError::Signature));
}

#[test]
fn only_a_published_signing_key_of_the_same_algorithm_checks_the_signature() {
    let signer = Signer::new("k1");
    let token = signer.sign(&claims());
    let jwk = signer.jwk();
    let changed = |name: &str, value: Value| {
        let mut jwk = jwk.clone();
        jwk[name] = value;
        jwk
    };
    assert_eq!(verify(&token, &[]), Err(IdTokenError::Signature));
    assert_eq!(verify(&token, &[Signer::new("k1").jwk()]), Err(IdTokenError::Signature), "another key, same kid");
    assert_eq!(verify(&token, &[changed("kid", json!("k2"))]), Err(IdTokenError::Signature));
    assert_eq!(verify(&token, &[changed("use", json!("enc"))]), Err(IdTokenError::Signature));
    assert_eq!(verify(&token, &[changed("alg", json!("RS256"))]), Err(IdTokenError::Signature));
    assert_eq!(verify(&token, &[changed("crv", json!("P-384"))]), Err(IdTokenError::Signature));
    // Several keys: the one the kid names checks it.
    assert_eq!(verify(&token, &[Signer::new("k0").jwk(), jwk.clone()]), Ok(()));
    // A token without a kid tries every fitting key.
    let no_kid = signer.sign_with(&json!({ "alg": "ES256" }), &claims());
    assert_eq!(verify(&no_kid, &[Signer::new("k0").jwk(), jwk.clone()]), Ok(()));
    // Changed claims break the signature.
    let (head, rest) = token.split_once('.').unwrap();
    let signature = rest.split_once('.').unwrap().1;
    let forged = format!("{head}.{}.{signature}", b64(with(json!({ "sub": "admin" })).to_string().as_bytes()));
    assert_eq!(verify(&forged, &[jwk]), Err(IdTokenError::Signature));
}

#[rstest]
#[case::none("none")]
#[case::shared_secret("HS256")]
#[case::unknown("XS999")]
fn an_algorithm_flinch_does_not_check_is_refused(#[case] alg: &str) {
    let signer = Signer::new("k1");
    let token = signer.sign_with(&json!({ "alg": alg, "kid": "k1" }), &claims());
    assert_eq!(verify(&token, &[signer.jwk()]), Err(IdTokenError::Algorithm(alg.to_string())));
}

#[rstest]
#[case::two_parts("e30.e30")]
#[case::four_parts("e30.e30.e30.e30")]
#[case::not_base64("***.e30.e30")]
#[case::not_json("bm9wZQ.e30.e30")]
fn a_token_that_is_not_a_jws_is_refused(#[case] token: &str) {
    assert_eq!(verify(token, &[]), Err(IdTokenError::Malformed));
}

fn parsed(vars: &[(&str, &str)]) -> Result<Option<Config>, ConfigError> {
    let vars: HashMap<&str, &str> = vars.iter().copied().collect();
    Config::from_vars(|name| vars.get(name).map(|value| value.to_string()))
}

const ISSUER_VAR: (&str, &str) = ("FLINCH_WEB_OIDC_ISSUER", ISSUER);
const CLIENT_VAR: (&str, &str) = ("FLINCH_WEB_OIDC_CLIENT_ID", CLIENT_ID);
const REDIRECT_VAR: (&str, &str) = ("FLINCH_WEB_OIDC_REDIRECT_URL", "https://flinch.example.org/api/oidc/callback");

#[rstest]
#[case::issuer_only(&[ISSUER_VAR], ConfigError::Missing("FLINCH_WEB_OIDC_CLIENT_ID"))]
#[case::blank_client(&[ISSUER_VAR, ("FLINCH_WEB_OIDC_CLIENT_ID", "  "), REDIRECT_VAR], ConfigError::Missing("FLINCH_WEB_OIDC_CLIENT_ID"))]
#[case::no_redirect(&[ISSUER_VAR, CLIENT_VAR], ConfigError::Missing("FLINCH_WEB_OIDC_REDIRECT_URL"))]
#[case::plain_http_issuer(&[("FLINCH_WEB_OIDC_ISSUER", "http://auth.lan"), CLIENT_VAR, REDIRECT_VAR], ConfigError::Url { name: "FLINCH_WEB_OIDC_ISSUER", reason: "is not https:// (plain http:// is accepted for localhost only)" })]
#[case::issuer_with_password(&[("FLINCH_WEB_OIDC_ISSUER", "https://a:b@auth.lan"), CLIENT_VAR, REDIRECT_VAR], ConfigError::Url { name: "FLINCH_WEB_OIDC_ISSUER", reason: "carries a user or password" })]
#[case::redirect_elsewhere(&[ISSUER_VAR, CLIENT_VAR, ("FLINCH_WEB_OIDC_REDIRECT_URL", "https://flinch.example.org/callback")], ConfigError::Url { name: "FLINCH_WEB_OIDC_REDIRECT_URL", reason: "must be FLINCH's own address followed by /api/oidc/callback" })]
#[case::redirect_with_query(&[ISSUER_VAR, CLIENT_VAR, ("FLINCH_WEB_OIDC_REDIRECT_URL", "https://flinch.example.org/api/oidc/callback?x=1")], ConfigError::Url { name: "FLINCH_WEB_OIDC_REDIRECT_URL", reason: "must be FLINCH's own address followed by /api/oidc/callback" })]
fn incomplete_settings_leave_sso_off_and_name_the_variable(#[case] vars: &[(&str, &str)], #[case] expected: ConfigError) {
    assert_eq!(parsed(vars).err(), Some(expected));
}

#[test]
fn settings_are_read_as_given() {
    assert!(matches!(parsed(&[]), Ok(None)));
    let config = parsed(&[
        ISSUER_VAR,
        CLIENT_VAR,
        REDIRECT_VAR,
        ("FLINCH_WEB_OIDC_ALLOWED_EMAILS", " Keeper@Example.org , ,b@example.org"),
        ("FLINCH_WEB_OIDC_ALLOWED_GROUPS", "media"),
    ])
    .unwrap()
    .unwrap();
    assert_eq!(&*config.name, "single sign-on");
    assert!(config.client_secret.is_none(), "no secret: a public client");
    assert_eq!(config.allowed.emails, vec![Box::from("keeper@example.org"), Box::from("b@example.org")]);
    assert_eq!(&*config.allowed.groups_claim, "groups");
    assert!(config.allowed.subjects.is_empty());
    // Plain http is fine for a provider on this machine.
    let local = parsed(&[("FLINCH_WEB_OIDC_ISSUER", "http://127.0.0.1:9091"), CLIENT_VAR, REDIRECT_VAR]);
    assert!(matches!(local, Ok(Some(_))));
}
