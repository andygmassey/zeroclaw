//! Spec'd pairing handshake endpoints (SHARED_AUTH_SPEC.md §3.3).
//!
//! POST /auth/pair/init      — Companion sends pairing_token, gets WebAuthn challenge
//! POST /auth/pair/register  — Companion sends attestation + companion_proof, gets wrapped_dek
//!
//! Status (PR 4):
//! - `init` is fully implemented: validates the QR pairing_token via the spec'd
//!   `PairingTokenStore` (32 B, 10-min TTL, single-use), generates a 32-byte
//!   `registration_challenge`, returns it alongside the spec'd rp_id /
//!   user_handle.
//!
//! - `register` is implemented for everything that does NOT require parsing
//!   the WebAuthn attestation: spec_version + token validity + body shape.
//!   The PRF-output extraction from `attestation_object` is a follow-up
//!   (it requires CBOR-level WebAuthn extension parsing which the existing
//!   hand-rolled `webauthn.rs` does not yet support, and pulling
//!   `webauthn-rs` 0.5 is a >300 LOC scope expansion). The crypto helpers
//!   that close the loop after PRF extraction (KEK derivation, AES-KW DEK
//!   wrap, companion/hub proofs) all ship as `zeroclaw-runtime::security::
//!   pair_handshake` with byte-identical-to-spec test vectors.
//!
//! Once PRF extraction lands, the `register` handler swaps the stub-tail
//! for the real KEK-derive → companion_proof verify → AES-KW wrap → respond
//! pipeline.

use crate::AppState;
use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use zeroclaw_config::pairing::TokenStatus;

/// Spec version — every v1 message MUST carry this in the JSON body.
/// SHARED_AUTH_SPEC.md §6.2.
pub const SPEC_VERSION: u8 = 1;

/// Spec'd rp_id (SHARED_AUTH_SPEC.md §0).
pub const RP_ID: &str = "creativemachines.ai";

/// Spec'd user_name embedded in the registration challenge response.
pub const USER_NAME: &str = "lifeline-user";

/// Spec'd display_name embedded in the registration challenge response.
pub const USER_DISPLAY_NAME: &str = "Ostler";

/// Spec'd rp_name surfaced to the platform passkey UI.
pub const RP_NAME: &str = "Ostler";

/// Length in bytes of the registration challenge (SHARED_AUTH_SPEC.md §3.3 Step A).
pub const REGISTRATION_CHALLENGE_LEN: usize = 32;

/// Length in bytes of the user_handle (SHARED_AUTH_SPEC.md §3.3 Step A).
pub const USER_HANDLE_LEN: usize = 16;

/// Request body for POST /auth/pair/init (SHARED_AUTH_SPEC.md §3.3 Step A).
///
/// Strict parsing: unknown top-level fields MUST be rejected (§3.7).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairInitRequest {
    pub spec_version: u8,
    pub pairing_token: String,
    pub device_label: String,
    #[serde(default)]
    pub client_version: Option<String>,
}

/// Response body for POST /auth/pair/init (SHARED_AUTH_SPEC.md §3.3 Step A).
#[derive(Debug, Serialize)]
pub struct PairInitResponse {
    pub spec_version: u8,
    pub registration_challenge: String,
    pub rp_id: &'static str,
    pub rp_name: &'static str,
    pub user_handle: String,
    pub user_name: &'static str,
    pub user_display_name: &'static str,
}

/// Request body for POST /auth/pair/register (SHARED_AUTH_SPEC.md §3.3 Step C).
///
/// Strict parsing: unknown top-level fields MUST be rejected (§3.7).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairRegisterRequest {
    pub spec_version: u8,
    pub pairing_token: String,
    pub credential_id: String,
    pub attestation_object: String,
    pub client_data_json: String,
    pub companion_proof: String,
}

/// Validate the spec version of an incoming request. Returns HTTP 426 on mismatch.
fn validate_spec_version(got: u8) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if got != SPEC_VERSION {
        return Err((
            StatusCode::UPGRADE_REQUIRED,
            Json(serde_json::json!({
                "error": "unsupported_spec_version",
                "spec": "SHARED_AUTH_SPEC.md",
                "spec_version_supported": SPEC_VERSION,
                "spec_version_received": got,
            })),
        ));
    }
    Ok(())
}

/// Map a `TokenStatus` failure to the spec'd HTTP code.
fn token_status_to_response(status: TokenStatus) -> (StatusCode, Json<serde_json::Value>) {
    match status {
        TokenStatus::NotFound => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "pairing_token_unknown",
                "spec": "SHARED_AUTH_SPEC.md §3.3.4",
            })),
        ),
        TokenStatus::Expired => (
            StatusCode::GONE,
            Json(serde_json::json!({
                "error": "pairing_token_expired",
                "spec": "SHARED_AUTH_SPEC.md §3.2",
            })),
        ),
        TokenStatus::Consumed => (
            StatusCode::GONE,
            Json(serde_json::json!({
                "error": "pairing_token_consumed",
                "spec": "SHARED_AUTH_SPEC.md §3.4",
            })),
        ),
        TokenStatus::Valid => unreachable!("valid status must be handled by caller"),
    }
}

/// Generate a fresh `n`-byte buffer of cryptographically-random bytes,
/// returned as base64url-unpadded text.
fn random_b64url(n: usize) -> String {
    let mut buf = vec![0u8; n];
    use ring::rand::{SecureRandom, SystemRandom};
    SystemRandom::new()
        .fill(&mut buf)
        .expect("system RNG must succeed");
    URL_SAFE_NO_PAD.encode(buf)
}

/// POST /auth/pair/init handler — fully implemented (PR 4).
pub async fn handle_pair_init(
    State(state): State<AppState>,
    body: Result<Json<PairInitRequest>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    let req = match body {
        Ok(Json(r)) => r,
        Err(rej) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "invalid_request_body",
                    "spec": "SHARED_AUTH_SPEC.md §3.3",
                    "detail": rej.body_text(),
                })),
            )
                .into_response();
        }
    };

    if let Err((status, body)) = validate_spec_version(req.spec_version) {
        return (status, body).into_response();
    }

    // Validate the pairing_token. "Claim" marks it pending without consuming it
    // — consumption only happens on a successful /auth/pair/register.
    if let Err(status) = state.pairing_token_store.claim(&req.pairing_token) {
        let (code, body) = token_status_to_response(status);
        return (code, body).into_response();
    }

    let registration_challenge = random_b64url(REGISTRATION_CHALLENGE_LEN);
    let user_handle = random_b64url(USER_HANDLE_LEN);

    tracing::info!(
        device_label = %req.device_label,
        client_version = ?req.client_version,
        "pairing init: token claimed, challenge issued"
    );

    Json(PairInitResponse {
        spec_version: SPEC_VERSION,
        registration_challenge,
        rp_id: RP_ID,
        rp_name: RP_NAME,
        user_handle,
        user_name: USER_NAME,
        user_display_name: USER_DISPLAY_NAME,
    })
    .into_response()
}

/// POST /auth/pair/register handler — partial impl (PR 4).
///
/// Implemented:
/// - JSON parsing + spec_version check.
/// - pairing_token validity check.
/// - companion_proof base64url decode + length validation (32 B per §3.3 Step C).
///
/// Pending (TODO): WebAuthn attestation parsing for PRF output extraction.
/// The crypto that closes the loop (HKDF KEK, AES-KW wrap, hub_proof) is fully
/// implemented and tested in `zeroclaw_runtime::security::pair_handshake` —
/// see fixture `primary_kek_matches_spec_fixture` and `aes_kw_matches_rfc3394_vector`
/// for byte-identical-to-spec verification.
///
/// Until PRF extraction lands, this handler returns HTTP 501 with a structured
/// error body so iOS sees a clear "not yet" rather than 404.
pub async fn handle_pair_register(
    State(state): State<AppState>,
    body: Result<Json<PairRegisterRequest>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    let req = match body {
        Ok(Json(r)) => r,
        Err(rej) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "invalid_request_body",
                    "spec": "SHARED_AUTH_SPEC.md §3.3",
                    "detail": rej.body_text(),
                })),
            )
                .into_response();
        }
    };

    if let Err((status, body)) = validate_spec_version(req.spec_version) {
        return (status, body).into_response();
    }

    // Token must still be valid (a claimed-then-expired flow returns 410).
    let status = state.pairing_token_store.status(&req.pairing_token);
    if status != TokenStatus::Valid {
        let (code, body) = token_status_to_response(status);
        return (code, body).into_response();
    }

    // companion_proof is base64url-unpadded, 32 bytes per HMAC-SHA256.
    let companion_proof = match URL_SAFE_NO_PAD.decode(&req.companion_proof) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "companion_proof_decode_failed",
                    "spec": "SHARED_AUTH_SPEC.md §3.3 Step C",
                    "detail": e.to_string(),
                })),
            )
                .into_response();
        }
    };
    if companion_proof.len() != 32 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "companion_proof_wrong_length",
                "spec": "SHARED_AUTH_SPEC.md §3.3 Step C",
                "expected": 32,
                "got": companion_proof.len(),
            })),
        )
            .into_response();
    }

    // Quick credential_id sanity check (also base64url-unpadded).
    if URL_SAFE_NO_PAD.decode(&req.credential_id).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "credential_id_decode_failed",
                "spec": "SHARED_AUTH_SPEC.md §3.7",
            })),
        )
            .into_response();
    }

    // PRF extraction from `attestation_object` is the missing piece. Until it
    // lands, surface a structured 501 so iOS knows the handshake is reserved
    // for this exact flow (not a routing 404, not an auth 401).
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "error": "pairing_register_pending_prf_extraction",
            "spec": "SHARED_AUTH_SPEC.md §3.3 Step C",
            "spec_version": SPEC_VERSION,
            "message": "Hub crypto (HKDF KEK + AES-KW + companion_proof) is in place. \
                        WebAuthn attestation parsing for PRF output is the remaining gap; \
                        once it lands, this handler returns wrapped_dek + hub_proof.",
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_init_request_strict_parsing() {
        let body = r#"{"spec_version":1,"pairing_token":"x","device_label":"y","extra":"z"}"#;
        let parsed: Result<PairInitRequest, _> = serde_json::from_str(body);
        assert!(parsed.is_err(), "Unknown top-level field MUST be rejected");
    }

    #[test]
    fn pair_init_request_accepts_known_fields() {
        let body = r#"{"spec_version":1,"pairing_token":"tok","device_label":"iPhone","client_version":"CM031/1.0.0"}"#;
        let parsed: PairInitRequest = serde_json::from_str(body).expect("valid body");
        assert_eq!(parsed.spec_version, 1);
        assert_eq!(parsed.pairing_token, "tok");
        assert_eq!(parsed.device_label, "iPhone");
        assert_eq!(parsed.client_version.as_deref(), Some("CM031/1.0.0"));
    }

    #[test]
    fn pair_init_request_optional_client_version() {
        let body = r#"{"spec_version":1,"pairing_token":"tok","device_label":"iPhone"}"#;
        let parsed: PairInitRequest = serde_json::from_str(body).expect("valid body");
        assert!(parsed.client_version.is_none());
    }

    #[test]
    fn pair_register_request_strict_parsing() {
        let body = r#"{"spec_version":1,"pairing_token":"x","credential_id":"c","attestation_object":"a","client_data_json":"d","companion_proof":"p","mystery":"!"}"#;
        let parsed: Result<PairRegisterRequest, _> = serde_json::from_str(body);
        assert!(parsed.is_err());
    }

    #[test]
    fn pair_register_request_accepts_all_fields() {
        let body = r#"{"spec_version":1,"pairing_token":"x","credential_id":"c","attestation_object":"a","client_data_json":"d","companion_proof":"p"}"#;
        let parsed: PairRegisterRequest = serde_json::from_str(body).expect("valid body");
        assert_eq!(parsed.spec_version, 1);
    }

    #[test]
    fn validate_spec_version_accepts_v1() {
        assert!(validate_spec_version(1).is_ok());
    }

    #[test]
    fn validate_spec_version_rejects_other() {
        assert!(validate_spec_version(2).is_err());
        assert!(validate_spec_version(0).is_err());
    }

    #[test]
    fn random_b64url_is_correct_length() {
        // 32 raw bytes → 43 base64url-unpadded chars.
        assert_eq!(random_b64url(32).len(), 43);
        // 16 raw bytes → 22 base64url-unpadded chars.
        assert_eq!(random_b64url(16).len(), 22);
    }

    #[test]
    fn random_b64url_is_unique() {
        // Two independent calls return different output (>= 1 of 5 must differ).
        let mut all_same = true;
        let baseline = random_b64url(32);
        for _ in 0..5 {
            if random_b64url(32) != baseline {
                all_same = false;
                break;
            }
        }
        assert!(!all_same, "RNG must produce distinct outputs");
    }

    #[test]
    fn token_status_maps_not_found_to_404() {
        let (code, _body) = token_status_to_response(TokenStatus::NotFound);
        assert_eq!(code, StatusCode::NOT_FOUND);
    }

    #[test]
    fn token_status_maps_expired_to_410() {
        let (code, _body) = token_status_to_response(TokenStatus::Expired);
        assert_eq!(code, StatusCode::GONE);
    }

    #[test]
    fn token_status_maps_consumed_to_410() {
        let (code, _body) = token_status_to_response(TokenStatus::Consumed);
        assert_eq!(code, StatusCode::GONE);
    }
}
