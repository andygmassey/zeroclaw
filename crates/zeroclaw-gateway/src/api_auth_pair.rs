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
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
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
///
/// `prf_output` carries the 32-byte PRF `first` output the Companion received
/// from its passkey in Step B. It is transported on the wire because the
/// WebAuthn L3 PRF eval result for `create()` lives in `clientExtensionResults`
/// on iOS rather than inside the `attestation_object`, so the Hub cannot
/// recover it by parsing CBOR. Sending it as a dedicated field is the clean
/// approach; correctness is enforced by `companion_proof` verification, which
/// will reject any value of `prf_output` the Companion did not actually
/// derive its KEK from.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairRegisterRequest {
    pub spec_version: u8,
    pub pairing_token: String,
    pub credential_id: String,
    pub attestation_object: String,
    pub client_data_json: String,
    pub prf_output: String,
    pub companion_proof: String,
}

/// Response body for POST /auth/pair/register (SHARED_AUTH_SPEC.md §3.3 Step C).
#[derive(Debug, Serialize)]
pub struct PairRegisterResponse {
    pub spec_version: u8,
    pub wrapped_dek: String,
    pub thread_id: &'static str,
    pub hub_proof: String,
}

/// On-disk shape of `${OSTLER_DIR}/security/passkey.json`.
///
/// Per SHARED_AUTH_SPEC.md §3.3 the Hub persists `(credential_id, user_handle,
/// wrapped_dek)` to a single file. `raw_dek_b64u` is a Hub-private addition
/// not on the wire: the Hub needs the raw DEK to re-wrap on subsequent
/// registrations (re-pair flow). The whole file is mode 0600; FileVault
/// protects it at rest. Keychain migration is post-launch hardening.
#[derive(Debug, Serialize, Deserialize)]
struct PasskeyFile {
    schema_version: u8,
    thread_id: String,
    credential_id: String,
    user_handle: String,
    wrapped_dek: String,
    raw_dek_b64u: String,
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

/// POST /auth/pair/register handler — full implementation (SHARED_AUTH_SPEC.md §3.3 Step C).
///
/// Flow:
///   1. Validate request shape + spec_version + token validity.
///   2. Base64url-decode `prf_output` and `companion_proof`; both MUST be 32 bytes.
///   3. Quick credential_id sanity check (decodes as base64url).
///   4. Derive the primary KEK from `prf_output` via §1.3.
///   5. Verify the companion_proof against the freshly-derived KEK. If it does
///      not match, the Companion either lied about `prf_output` or used a
///      different KEK derivation. Either way, reject with HTTP 401 and a
///      structured error body so the iOS Companion can surface a clear
///      pairing-failed message.
///   6. Look up the existing `sync DEK` from `${OSTLER_DIR}/security/passkey.json`
///      if a prior pairing produced one; otherwise generate a fresh 32-byte DEK.
///      v1 has exactly one DEK, `thread_id = "default"` (spec §3.3).
///   7. Wrap the DEK under the primary KEK via AES-KW (§2) -> 40 B wrapped_dek.
///   8. Compute the hub_proof for return-trip verification (§3.3, HMAC over
///      `"creativemachines/pair/proof/v1/hub"` keyed with the KEK).
///   9. Persist `(credential_id, wrapped_dek, raw_dek_b64u)` to disk for the
///      re-pair flow. user_handle is left empty in v1 (the spec persists it
///      but no current Hub surface reads it; future multi-account support
///      will plumb it through `PairingTokenStore` so Step A's issued
///      user_handle survives until Step C).
///  10. Mark the pairing_token as consumed (single-use per §3.4).
///  11. Respond 200 with `{ spec_version, wrapped_dek, thread_id: "default",
///      hub_proof }`.
///
/// Failure mapping:
///   - Body parse / decode failure         -> 400
///   - Spec version mismatch                -> 426
///   - Token unknown / expired / consumed   -> 404 / 410 / 410 per §3.3.4 / §3.2 / §3.4
///   - companion_proof verification failure -> 401
///   - DEK file read failure / write failure (disk IO) -> 500 with structured detail
///
/// No request body, response body, prf_output bytes, KEK bytes, raw DEK bytes,
/// or wrapped DEK bytes are logged. The handler emits at most a single
/// structured `tracing::info!` with `credential_id_prefix` (first 8 chars of
/// the b64url id, deterministic, low-sensitivity) + spec citation when it
/// succeeds. Failure logs include only the error code, not the offending
/// material.
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

    // prf_output is base64url-unpadded, exactly 32 bytes (SHARED_AUTH_SPEC.md
    // §3.3 Step C). Decode + length-check before any crypto.
    let prf_output = match URL_SAFE_NO_PAD.decode(&req.prf_output) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "prf_output_decode_failed",
                    "spec": "SHARED_AUTH_SPEC.md §3.3 Step C",
                    "detail": e.to_string(),
                })),
            )
                .into_response();
        }
    };
    if prf_output.len() != zeroclaw_runtime::security::pair_handshake::PRF_OUTPUT_LEN {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "prf_output_wrong_length",
                "spec": "SHARED_AUTH_SPEC.md §3.3 Step C",
                "expected": zeroclaw_runtime::security::pair_handshake::PRF_OUTPUT_LEN,
                "got": prf_output.len(),
            })),
        )
            .into_response();
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
    if companion_proof.len() != zeroclaw_runtime::security::pair_handshake::PROOF_LEN {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "companion_proof_wrong_length",
                "spec": "SHARED_AUTH_SPEC.md §3.3 Step C",
                "expected": zeroclaw_runtime::security::pair_handshake::PROOF_LEN,
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

    // Derive the primary KEK from prf_output via §1.3. thread_id is the v1
    // fixed value "default" (spec §3.3: "In v1 there is exactly one DEK,
    // thread_id = 'default'").
    let kek = match zeroclaw_runtime::security::pair_handshake::PrimaryKek::derive(
        &prf_output,
        zeroclaw_runtime::security::pair_handshake::DEFAULT_THREAD_ID,
    ) {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!(error = %e, "pair-register: KEK derivation failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "kek_derivation_failed",
                    "spec": "SHARED_AUTH_SPEC.md §1.3",
                })),
            )
                .into_response();
        }
    };

    // Verify the companion proof. A mismatch means the Companion either lied
    // about prf_output, used a different KEK derivation, or computed the proof
    // over a different HMAC input. Any of those = reject with 401.
    if !zeroclaw_runtime::security::pair_handshake::verify_companion_proof(&kek, &companion_proof) {
        tracing::warn!("pair-register: companion_proof verification failed");
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "companion_proof_mismatch",
                "spec": "SHARED_AUTH_SPEC.md §3.3 Step C",
                "detail": "companion_proof does not match the KEK derived from prf_output; \
                           the Companion did not supply the PRF output it actually computed \
                           its proof over.",
            })),
        )
            .into_response();
    }

    // Look up an existing DEK on disk, else generate a fresh one. v1 keeps a
    // single DEK keyed by thread_id = "default" in the passkey file.
    let workspace_dir = {
        let cfg = state.config.lock();
        cfg.workspace_dir.clone()
    };
    let security_dir = workspace_dir.join("security");
    let passkey_path = security_dir.join("passkey.json");

    let dek = match load_or_generate_dek(&passkey_path) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "pair-register: DEK load/generate failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "dek_io_failed",
                    "spec": "SHARED_AUTH_SPEC.md §3.3 Step C",
                    "detail": e.to_string(),
                })),
            )
                .into_response();
        }
    };

    let wrapped_dek = zeroclaw_runtime::security::pair_handshake::wrap_dek(&kek, &dek);
    let hub_proof = zeroclaw_runtime::security::pair_handshake::compute_hub_proof(&kek);

    // Persist the public material + the raw DEK (Hub-private). The whole file
    // is mode 0600 because raw_dek_b64u is sensitive.
    if let Err(e) = persist_passkey_file(
        &security_dir,
        &passkey_path,
        &req.credential_id,
        &URL_SAFE_NO_PAD.encode(wrapped_dek),
        &URL_SAFE_NO_PAD.encode(dek),
    ) {
        tracing::warn!(error = %e, "pair-register: passkey file write failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "passkey_persist_failed",
                "spec": "SHARED_AUTH_SPEC.md §3.3 Step C",
                "detail": e.to_string(),
            })),
        )
            .into_response();
    }

    // Token is consumed only after the full Step C succeeded (§3.4).
    if let Err(status) = state.pairing_token_store.consume(&req.pairing_token) {
        // Token went stale between status() check and consume() (e.g. expiry
        // race). Map to the spec'd HTTP code and bail.
        let (code, body) = token_status_to_response(status);
        return (code, body).into_response();
    }

    let credential_id_prefix = req.credential_id.chars().take(8).collect::<String>();
    tracing::info!(
        credential_id_prefix = %credential_id_prefix,
        thread_id = %zeroclaw_runtime::security::pair_handshake::DEFAULT_THREAD_ID,
        "pair-register: completed (SHARED_AUTH_SPEC.md §3.3 Step C)"
    );

    Json(PairRegisterResponse {
        spec_version: SPEC_VERSION,
        wrapped_dek: URL_SAFE_NO_PAD.encode(wrapped_dek),
        thread_id: zeroclaw_runtime::security::pair_handshake::DEFAULT_THREAD_ID,
        hub_proof: URL_SAFE_NO_PAD.encode(hub_proof),
    })
    .into_response()
}

/// Load the existing raw DEK from the passkey file, or generate a fresh one
/// if no file exists yet.
///
/// Returns the 32-byte raw DEK ready for `wrap_dek` / sync use.
///
/// On first-ever pairing: file does not exist -> generate a fresh DEK via the
/// pair_handshake helper, hand it back, defer write to `persist_passkey_file`
/// (which writes the full file atomically once the wrapped_dek is also known).
/// On re-pair (subsequent pairing, same or different device): file exists,
/// read raw_dek_b64u, decode, return. The DEK is invariant across pairings
/// in v1 so the Companion's wrapped DEK stays unwrappable across re-pair.
///
/// Errors are bubbled as `std::io::Error` for handler-side mapping to 500.
fn load_or_generate_dek(
    passkey_path: &std::path::Path,
) -> std::io::Result<[u8; zeroclaw_runtime::security::pair_handshake::DEK_LEN]> {
    if !passkey_path.exists() {
        return Ok(zeroclaw_runtime::security::pair_handshake::generate_dek());
    }
    let body = std::fs::read_to_string(passkey_path)?;
    let parsed: PasskeyFile = serde_json::from_str(&body).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("passkey.json parse failed: {e}"),
        )
    })?;
    let raw = URL_SAFE_NO_PAD.decode(&parsed.raw_dek_b64u).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("raw_dek_b64u decode failed: {e}"),
        )
    })?;
    let mut dek = [0u8; zeroclaw_runtime::security::pair_handshake::DEK_LEN];
    if raw.len() != dek.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "raw_dek_b64u decoded to {} bytes, expected {}",
                raw.len(),
                dek.len()
            ),
        ));
    }
    dek.copy_from_slice(&raw);
    Ok(dek)
}

/// Write the passkey file atomically with mode 0600.
///
/// `${OSTLER_DIR}/security/passkey.json` per SHARED_AUTH_SPEC.md §3.3.
/// Atomic = write to a sibling `passkey.json.tmp` first, fsync, rename. The
/// crash-safety story is the standard write-rename-fsync chain; we accept that
/// a power-loss between rename and parent-dir-fsync may leave the file
/// invisible until the next pairing (acceptable for an explicitly user-driven
/// operation that the user can re-run).
///
/// user_handle is persisted empty in v1. Spec lists it as a persisted field
/// but no current Hub surface consumes it; future multi-account support will
/// plumb the Step-A-issued user_handle through `PairingTokenStore` so Step C
/// can persist the right value. The empty-string placeholder keeps the file
/// schema forward-compatible.
fn persist_passkey_file(
    security_dir: &std::path::Path,
    passkey_path: &std::path::Path,
    credential_id: &str,
    wrapped_dek_b64u: &str,
    raw_dek_b64u: &str,
) -> std::io::Result<()> {
    use std::io::Write as _;

    std::fs::create_dir_all(security_dir)?;
    // Tighten the directory mode too (0700) so siblings the security dir picks
    // up later inherit a private parent.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut perm = std::fs::metadata(security_dir)?.permissions();
        perm.set_mode(0o700);
        std::fs::set_permissions(security_dir, perm)?;
    }

    let body = PasskeyFile {
        schema_version: 1,
        thread_id: zeroclaw_runtime::security::pair_handshake::DEFAULT_THREAD_ID.to_string(),
        credential_id: credential_id.to_string(),
        user_handle: String::new(),
        wrapped_dek: wrapped_dek_b64u.to_string(),
        raw_dek_b64u: raw_dek_b64u.to_string(),
    };
    let serialised = serde_json::to_vec_pretty(&body).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("passkey.json serialise failed: {e}"),
        )
    })?;

    let tmp_path = passkey_path.with_extension("json.tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        // The 0o600 mode is the on-disk gate that keeps the wrapped DEK
        // unreadable by other local users. Only Unix targets surface a
        // POSIX mode bit; on Windows the file inherits default ACLs.
        // In practice the Hub only runs on macOS, so the Windows branch
        // exists purely to keep the workspace compiling on the
        // x86_64-pc-windows-msvc CI matrix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut tmp = opts.open(&tmp_path)?;
        tmp.write_all(&serialised)?;
        tmp.sync_all()?;
    }
    std::fs::rename(&tmp_path, passkey_path)?;
    Ok(())
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
        let body = r#"{"spec_version":1,"pairing_token":"x","credential_id":"c","attestation_object":"a","client_data_json":"d","prf_output":"o","companion_proof":"p","mystery":"!"}"#;
        let parsed: Result<PairRegisterRequest, _> = serde_json::from_str(body);
        assert!(parsed.is_err());
    }

    #[test]
    fn pair_register_request_accepts_all_fields() {
        let body = r#"{"spec_version":1,"pairing_token":"x","credential_id":"c","attestation_object":"a","client_data_json":"d","prf_output":"o","companion_proof":"p"}"#;
        let parsed: PairRegisterRequest = serde_json::from_str(body).expect("valid body");
        assert_eq!(parsed.spec_version, 1);
        assert_eq!(parsed.prf_output, "o");
    }

    #[test]
    fn pair_register_request_rejects_missing_prf_output() {
        let body = r#"{"spec_version":1,"pairing_token":"x","credential_id":"c","attestation_object":"a","client_data_json":"d","companion_proof":"p"}"#;
        let parsed: Result<PairRegisterRequest, _> = serde_json::from_str(body);
        assert!(parsed.is_err(), "prf_output is required per §3.3 Step C");
    }

    #[test]
    fn load_or_generate_dek_creates_when_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("passkey.json");
        let dek = load_or_generate_dek(&path).expect("generate");
        assert_eq!(
            dek.len(),
            zeroclaw_runtime::security::pair_handshake::DEK_LEN
        );
        // Did not create the file (the handler writes via persist_passkey_file
        // after the wrap step).
        assert!(!path.exists());
    }

    #[test]
    fn persist_then_load_round_trips_dek() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let security_dir = tmp.path().to_path_buf();
        let passkey_path = security_dir.join("passkey.json");

        let dek = zeroclaw_runtime::security::pair_handshake::generate_dek();
        let wrapped = [0xABu8; 40];

        persist_passkey_file(
            &security_dir,
            &passkey_path,
            "cred-id-fixture",
            &URL_SAFE_NO_PAD.encode(wrapped),
            &URL_SAFE_NO_PAD.encode(dek),
        )
        .expect("persist");

        // Mode 0600 (Unix only).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&passkey_path)
                .expect("stat")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "passkey.json must be mode 0600");
        }

        let loaded = load_or_generate_dek(&passkey_path).expect("reload");
        assert_eq!(loaded, dek, "raw DEK must round-trip through passkey.json");
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
