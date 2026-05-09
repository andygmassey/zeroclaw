//! Subscription sync endpoint (`POST /api/v1/subscription/sync`).
//!
//! The iOS Companion validates StoreKit 2 receipts locally and posts the
//! derived state here. The Hub never validates Apple receipts.
//!
//! See `SUBSCRIPTION_GATE_ARCH_2026-05-09.md` for the full spec.

use super::AppState;
use crate::api::require_auth;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use zeroclaw_config::schema::{SubscriptionSource, SubscriptionStatus};

/// Request body for `POST /api/v1/subscription/sync`.
///
/// The Companion sends only Companion-derived state: status, optional
/// expiry, optional Apple transaction ID, optional sha256(receipt) hash.
/// The Hub never sees the raw receipt blob.
#[derive(Debug, Clone, Deserialize)]
pub struct SubscriptionSyncRequest {
    /// Status enum, kebab-cased: `active` / `grace` / `expired` / `inactive`.
    pub status: SubscriptionStatus,
    /// RFC3339 expiry from the StoreKit 2 receipt, if known.
    #[serde(default)]
    pub expires_at: Option<String>,
    /// Latest StoreKit 2 originalTransactionID, if known.
    #[serde(default)]
    pub apple_transaction_id: Option<String>,
    /// sha256(hex) of the receipt blob — integrity tag only.
    #[serde(default)]
    pub apple_receipt_hash: Option<String>,
}

/// Response body for `POST /api/v1/subscription/sync`.
#[derive(Debug, Clone, Serialize)]
pub struct SubscriptionSyncResponse {
    pub accepted: bool,
    /// RFC3339 timestamp at which the Hub recorded this sync.
    pub hub_validated_at: String,
}

/// Returns `true` if `s` is a valid RFC3339 timestamp (or empty / `None`).
/// Empty strings round-trip cleanly and represent "absent" on disk.
fn rfc3339_is_valid(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    DateTime::parse_from_rfc3339(s).is_ok()
}

/// `POST /api/v1/subscription/sync` — accept Companion-derived state.
///
/// Body: see [`SubscriptionSyncRequest`].
/// Response: `200 OK` with [`SubscriptionSyncResponse`] on success;
/// `400 Bad Request` on malformed RFC3339 timestamps;
/// `401 Unauthorized` when not paired;
/// `500 Internal Server Error` on config persistence failure.
pub async fn handle_subscription_sync(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SubscriptionSyncRequest>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    // Validate optional RFC3339 timestamps before mutating state.
    let expires_at = body.expires_at.unwrap_or_default();
    if !rfc3339_is_valid(&expires_at) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "expires_at is not a valid RFC3339 timestamp"
            })),
        )
            .into_response();
    }

    let now = Utc::now();
    let now_rfc3339 = now.to_rfc3339();

    // Compute a fresh grace_period_end on transitions:
    //  • Active  → clear grace (Companion is the source of truth on expiry)
    //  • Grace   → keep existing grace_period_end if set, else +30d
    //  • Expired → set 14d post-lapse grace from now
    //  • Inactive → clear grace
    let new_status = body.status;
    let mut config = state.config.lock().clone();

    let existing_grace_end = config.subscription.grace_period_end.clone();
    let new_grace_end = match new_status {
        SubscriptionStatus::Active | SubscriptionStatus::Inactive => String::new(),
        SubscriptionStatus::Grace => {
            if existing_grace_end.is_empty() {
                (now + chrono::Duration::days(30)).to_rfc3339()
            } else {
                existing_grace_end
            }
        }
        SubscriptionStatus::Expired => (now + chrono::Duration::days(14)).to_rfc3339(),
    };

    config.subscription.status = new_status;
    config.subscription.last_validated_at = now_rfc3339.clone();
    config.subscription.expires_at = expires_at;
    config.subscription.grace_period_end = new_grace_end;
    config.subscription.source = SubscriptionSource::Companion;
    config.subscription.apple_transaction_id = body.apple_transaction_id.unwrap_or_default();
    config.subscription.apple_receipt_hash = body.apple_receipt_hash.unwrap_or_default();

    // Persist atomically (existing Config::save() handles temp-file +
    // rename + permissions).
    if let Err(e) = config.save().await {
        tracing::warn!(
            error = %e,
            "subscription.sync: Failed to persist subscription state"
        );
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to persist subscription state: {e}")
            })),
        )
            .into_response();
    }

    // Update in-memory config.
    *state.config.lock() = config;

    let response = SubscriptionSyncResponse {
        accepted: true,
        hub_validated_at: now_rfc3339,
    };

    Json(response).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_validation_accepts_empty_and_valid() {
        assert!(rfc3339_is_valid(""));
        assert!(rfc3339_is_valid("2026-05-09T12:00:00+00:00"));
        assert!(rfc3339_is_valid("2026-05-09T12:00:00Z"));
    }

    #[test]
    fn rfc3339_validation_rejects_garbage() {
        assert!(!rfc3339_is_valid("not a date"));
        assert!(!rfc3339_is_valid("2026-05-09"));
        assert!(!rfc3339_is_valid("2026-13-99T99:99:99Z"));
    }

    #[test]
    fn sync_request_deserialises_active_full() {
        let body = r#"{
            "status": "active",
            "expires_at": "2026-06-09T12:00:00+00:00",
            "apple_transaction_id": "2000000123456789",
            "apple_receipt_hash": "abc123"
        }"#;
        let req: SubscriptionSyncRequest = serde_json::from_str(body).unwrap();
        assert_eq!(req.status, SubscriptionStatus::Active);
        assert_eq!(req.expires_at.as_deref(), Some("2026-06-09T12:00:00+00:00"));
        assert_eq!(
            req.apple_transaction_id.as_deref(),
            Some("2000000123456789")
        );
        assert_eq!(req.apple_receipt_hash.as_deref(), Some("abc123"));
    }

    #[test]
    fn sync_request_deserialises_minimal_inactive() {
        let body = r#"{ "status": "inactive" }"#;
        let req: SubscriptionSyncRequest = serde_json::from_str(body).unwrap();
        assert_eq!(req.status, SubscriptionStatus::Inactive);
        assert!(req.expires_at.is_none());
        assert!(req.apple_transaction_id.is_none());
        assert!(req.apple_receipt_hash.is_none());
    }

    #[test]
    fn sync_request_rejects_unknown_status() {
        let body = r#"{ "status": "platinum-tier" }"#;
        let result: Result<SubscriptionSyncRequest, _> = serde_json::from_str(body);
        assert!(result.is_err(), "unknown status should fail to deserialise");
    }

    #[test]
    fn sync_request_accepts_kebab_case_status() {
        // Sanity check that all four variants accept their kebab-case form.
        for variant in ["active", "grace", "expired", "inactive"] {
            let body = format!(r#"{{ "status": "{variant}" }}"#);
            let req: SubscriptionSyncRequest =
                serde_json::from_str(&body).expect("parse status variant");
            let _ = req.status; // exhaustive parse is enough
        }
    }

    #[test]
    fn sync_response_serialises_with_expected_fields() {
        let resp = SubscriptionSyncResponse {
            accepted: true,
            hub_validated_at: "2026-05-09T12:00:00+00:00".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"accepted\":true"));
        assert!(json.contains("\"hub_validated_at\":\"2026-05-09T12:00:00+00:00\""));
    }
}
