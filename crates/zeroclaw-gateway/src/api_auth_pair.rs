//! Spec'd pairing handshake endpoints (SHARED_AUTH_SPEC.md §3.3).
//!
//! POST /auth/pair/init      — Companion sends pairing_token, gets WebAuthn challenge
//! POST /auth/pair/register  — Companion sends attestation + companion_proof, gets wrapped_dek
//!
//! These handlers are STUBS (PR 2). They return a structured 501-shaped error so
//! that iOS clients hitting the endpoint get a parseable error instead of a 404,
//! enabling clearer "your Hub is too old" UX while the real implementation lands
//! in PR 4.
//!
//! Real implementation lives in PR 4: WebAuthn registration with PRF, AES-KW
//! DEK wrapping, companion_proof / hub_proof HMAC verification.

use axum::{Json, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};

/// Spec version — every v1 message MUST carry this in the JSON body.
/// SHARED_AUTH_SPEC.md §6.2.
pub const SPEC_VERSION: u8 = 1;

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

/// Standardised "not yet implemented" error body.
#[derive(Debug, Serialize)]
struct NotImplementedBody {
    error: &'static str,
    spec: &'static str,
    spec_version: u8,
    message: &'static str,
}

fn not_implemented_response(message: &'static str) -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(NotImplementedBody {
            error: "pairing_protocol_v1_not_yet_implemented",
            spec: "SHARED_AUTH_SPEC.md",
            spec_version: SPEC_VERSION,
            message,
        }),
    )
}

/// Reject an obviously-malformed request (wrong spec_version, etc.) with a
/// structured error before we hand it on to the not-implemented response.
fn validate_spec_version(got: u8) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if got != SPEC_VERSION {
        // §6.2: v1 implementations MUST reject other versions with HTTP 426.
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

/// POST /auth/pair/init handler — STUB (PR 2). Real impl in PR 4.
pub async fn handle_pair_init(
    body: Result<Json<PairInitRequest>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    let req = match body {
        Ok(Json(r)) => r,
        Err(rej) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "invalid_request_body",
                    "spec": "SHARED_AUTH_SPEC.md",
                    "detail": rej.body_text(),
                })),
            )
                .into_response();
        }
    };

    if let Err((status, body)) = validate_spec_version(req.spec_version) {
        return (status, body).into_response();
    }

    // Stubbed: the real handler will validate `pairing_token` against PairingTokenStore,
    // generate a 32-byte registration_challenge, and return the WebAuthn registration
    // options per SHARED_AUTH_SPEC.md §3.3 Step A response.
    not_implemented_response(
        "POST /auth/pair/init is reserved for SHARED_AUTH_SPEC.md §3.3 Step A. \
         Real implementation arrives in a follow-up PR.",
    )
    .into_response()
}

/// POST /auth/pair/register handler — STUB (PR 2). Real impl in PR 4.
pub async fn handle_pair_register(
    body: Result<Json<PairRegisterRequest>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    let req = match body {
        Ok(Json(r)) => r,
        Err(rej) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "invalid_request_body",
                    "spec": "SHARED_AUTH_SPEC.md",
                    "detail": rej.body_text(),
                })),
            )
                .into_response();
        }
    };

    if let Err((status, body)) = validate_spec_version(req.spec_version) {
        return (status, body).into_response();
    }

    // Stubbed: the real handler will verify the WebAuthn attestation, extract PRF
    // output, derive the primary KEK via HKDF-SHA256, recompute and verify
    // `companion_proof`, AES-KW wrap the DEK, and return wrapped_dek + hub_proof
    // per SHARED_AUTH_SPEC.md §3.3 Step C response.
    not_implemented_response(
        "POST /auth/pair/register is reserved for SHARED_AUTH_SPEC.md §3.3 Step C. \
         Real implementation arrives in a follow-up PR.",
    )
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_init_request_strict_parsing() {
        // Unknown field rejected (§3.7).
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
}
