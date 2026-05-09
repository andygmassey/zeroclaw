//! Subscription gate helpers (new-data-gate v1.0).
//!
//! See `SUBSCRIPTION_GATE_ARCH_2026-05-09.md` for the full spec.
//!
//! The Hub never validates Apple receipts. The iOS Companion is the source
//! of truth: it validates StoreKit 2 receipts locally and POSTs derived
//! state to `/api/v1/subscription/sync`. This module exposes:
//!
//! * [`is_active_or_grace`] — the per-pipeline gate predicate.
//! * [`state_freshness`] — Doctor-banner staleness verdict (Fresh / Stale /
//!   `OfflineGrace` / Inactive).
//! * [`parse_rfc3339`] — small parsing helper used by both predicates.
//!
//! The cached state lives in
//! [`zeroclaw_config::schema::SubscriptionConfig`].

use chrono::{DateTime, Duration, Utc};
use zeroclaw_config::schema::{SubscriptionConfig, SubscriptionStatus};

/// Post-lapse soft-landing window: after a sub goes Expired we still let
/// new-data ingestion through for this long. Matches
/// `feedback_subscription_gating_v1.md`.
pub const POST_LAPSE_GRACE: Duration = Duration::days(14);

/// Fresh-window cutoff: Companion sync younger than this is "Fresh"; older
/// is "Stale". Matches the 7-day re-validation cadence in the spec.
pub const FRESH_CUTOFF: Duration = Duration::days(7);

/// Offline-grace cutoff: after this long without a Companion sync the gate
/// fails OPEN to avoid punishing customers on holiday. Doctor still warns.
pub const OFFLINE_GRACE_CUTOFF: Duration = Duration::days(30);

/// Doctor-banner staleness verdict for the cached state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StalenessVerdict {
    /// Companion sync within `FRESH_CUTOFF`.
    Fresh,
    /// Older than `FRESH_CUTOFF` but within `OFFLINE_GRACE_CUTOFF`.
    Stale,
    /// Older than `OFFLINE_GRACE_CUTOFF` — gate fails open, Doctor warns.
    OfflineGrace,
    /// `status == Inactive` regardless of age.
    Inactive,
}

/// Parse an RFC3339 timestamp string into `DateTime<Utc>`.
///
/// Empty strings are returned as `None`; this is the on-disk representation
/// for "absent" optional fields (we use `String` instead of
/// `Option<String>` to keep the `Configurable` derive happy).
pub fn parse_rfc3339(raw: &str) -> Option<DateTime<Utc>> {
    if raw.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Returns `true` when the subscription gate should let a new-data
/// ingestion call through. Per the v1.0 spec:
///
/// 1. `Active` → always pass.
/// 2. `Grace` and `grace_period_end > now` → pass.
/// 3. `Expired` and `now - last_validated_at < 14d` → pass (post-lapse soft
///    landing).
/// 4. Anything else → block.
///
/// The 30-day OfflineGrace fail-open is layered on top of this in the
/// per-pipeline call sites — when [`state_freshness`] reports
/// `OfflineGrace`, callers should still let work through and surface a
/// Doctor warning. (Keeping that out of `is_active_or_grace` keeps the
/// predicate purely about cached state.)
#[must_use]
pub fn is_active_or_grace(state: &SubscriptionConfig, now: DateTime<Utc>) -> bool {
    match state.status {
        SubscriptionStatus::Active => true,
        SubscriptionStatus::Grace => parse_rfc3339(&state.grace_period_end)
            .map(|end| end > now)
            .unwrap_or(false),
        SubscriptionStatus::Expired => parse_rfc3339(&state.last_validated_at)
            .map(|last| now - last < POST_LAPSE_GRACE)
            .unwrap_or(false),
        SubscriptionStatus::Inactive => false,
    }
}

/// Doctor-banner staleness verdict for the cached state.
///
/// `Inactive` short-circuits regardless of age. Otherwise the verdict is
/// purely a function of `last_validated_at` vs `now`.
#[must_use]
pub fn state_freshness(state: &SubscriptionConfig, now: DateTime<Utc>) -> StalenessVerdict {
    if state.status == SubscriptionStatus::Inactive {
        return StalenessVerdict::Inactive;
    }
    let Some(last) = parse_rfc3339(&state.last_validated_at) else {
        // Malformed timestamp on disk — treat as offline-grace to avoid
        // hard-blocking legitimate users on a parsing bug.
        return StalenessVerdict::OfflineGrace;
    };
    let age = now - last;
    if age < FRESH_CUTOFF {
        StalenessVerdict::Fresh
    } else if age < OFFLINE_GRACE_CUTOFF {
        StalenessVerdict::Stale
    } else {
        StalenessVerdict::OfflineGrace
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::{SubscriptionConfig, SubscriptionSource, SubscriptionStatus};

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-09T12:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn state(
        status: SubscriptionStatus,
        last_validated_at: DateTime<Utc>,
        grace_end: Option<DateTime<Utc>>,
    ) -> SubscriptionConfig {
        SubscriptionConfig {
            status,
            last_validated_at: last_validated_at.to_rfc3339(),
            expires_at: String::new(),
            grace_period_end: grace_end.map(|d| d.to_rfc3339()).unwrap_or_default(),
            source: SubscriptionSource::Companion,
            apple_transaction_id: String::new(),
            apple_receipt_hash: String::new(),
        }
    }

    // ── is_active_or_grace ──────────────────────────────────

    #[test]
    fn active_always_passes() {
        let now = fixed_now();
        let s = state(SubscriptionStatus::Active, now, None);
        assert!(is_active_or_grace(&s, now));
    }

    #[test]
    fn active_passes_even_when_last_validated_is_ancient() {
        let now = fixed_now();
        let s = state(
            SubscriptionStatus::Active,
            now - Duration::days(365),
            None,
        );
        assert!(is_active_or_grace(&s, now));
    }

    #[test]
    fn grace_with_future_grace_end_passes() {
        let now = fixed_now();
        let s = state(
            SubscriptionStatus::Grace,
            now,
            Some(now + Duration::days(5)),
        );
        assert!(is_active_or_grace(&s, now));
    }

    #[test]
    fn grace_with_past_grace_end_blocks() {
        let now = fixed_now();
        let s = state(
            SubscriptionStatus::Grace,
            now,
            Some(now - Duration::days(1)),
        );
        assert!(!is_active_or_grace(&s, now));
    }

    #[test]
    fn grace_with_missing_grace_end_blocks() {
        let now = fixed_now();
        let s = state(SubscriptionStatus::Grace, now, None);
        assert!(!is_active_or_grace(&s, now));
    }

    #[test]
    fn expired_within_14d_passes() {
        let now = fixed_now();
        let s = state(
            SubscriptionStatus::Expired,
            now - Duration::days(13),
            None,
        );
        assert!(is_active_or_grace(&s, now));
    }

    #[test]
    fn expired_at_14d_boundary_blocks() {
        let now = fixed_now();
        let s = state(
            SubscriptionStatus::Expired,
            now - Duration::days(14),
            None,
        );
        // 14d post-lapse grace is `< 14d`, not `<= 14d`.
        assert!(!is_active_or_grace(&s, now));
    }

    #[test]
    fn expired_beyond_14d_blocks() {
        let now = fixed_now();
        let s = state(
            SubscriptionStatus::Expired,
            now - Duration::days(20),
            None,
        );
        assert!(!is_active_or_grace(&s, now));
    }

    #[test]
    fn inactive_always_blocks() {
        let now = fixed_now();
        let s = state(SubscriptionStatus::Inactive, now, Some(now + Duration::days(365)));
        assert!(!is_active_or_grace(&s, now));
    }

    #[test]
    fn manual_override_active_passes() {
        let now = fixed_now();
        let mut s = state(SubscriptionStatus::Active, now, None);
        s.source = SubscriptionSource::ManualOverride;
        // ManualOverride is treated identically to Companion-source.
        assert!(is_active_or_grace(&s, now));
    }

    // ── state_freshness ─────────────────────────────────────

    #[test]
    fn fresh_when_last_sync_under_7d() {
        let now = fixed_now();
        let s = state(SubscriptionStatus::Active, now - Duration::days(3), None);
        assert_eq!(state_freshness(&s, now), StalenessVerdict::Fresh);
    }

    #[test]
    fn stale_between_7d_and_30d() {
        let now = fixed_now();
        let s = state(SubscriptionStatus::Active, now - Duration::days(10), None);
        assert_eq!(state_freshness(&s, now), StalenessVerdict::Stale);
    }

    #[test]
    fn offline_grace_after_30d() {
        let now = fixed_now();
        let s = state(SubscriptionStatus::Active, now - Duration::days(45), None);
        assert_eq!(state_freshness(&s, now), StalenessVerdict::OfflineGrace);
    }

    #[test]
    fn inactive_short_circuits_freshness() {
        let now = fixed_now();
        let s = state(SubscriptionStatus::Inactive, now, None);
        assert_eq!(state_freshness(&s, now), StalenessVerdict::Inactive);
    }

    #[test]
    fn freshness_handles_malformed_timestamp() {
        let now = fixed_now();
        let mut s = state(SubscriptionStatus::Active, now, None);
        s.last_validated_at = "not a date".to_string();
        assert_eq!(state_freshness(&s, now), StalenessVerdict::OfflineGrace);
    }

    // ── parse_rfc3339 ───────────────────────────────────────

    #[test]
    fn parse_rfc3339_empty_is_none() {
        assert!(parse_rfc3339("").is_none());
    }

    #[test]
    fn parse_rfc3339_valid_round_trips() {
        let t = parse_rfc3339("2026-05-09T12:00:00+00:00").expect("valid RFC3339");
        assert_eq!(t.to_rfc3339(), "2026-05-09T12:00:00+00:00");
    }

    #[test]
    fn parse_rfc3339_malformed_is_none() {
        assert!(parse_rfc3339("definitely not a date").is_none());
    }
}
