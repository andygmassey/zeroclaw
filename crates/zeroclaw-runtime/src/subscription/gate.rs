//! Per-pipeline subscription gate (Phase 1b).
//!
//! New-data ingestion call sites use [`gate_new_data_ingestion`] to decide
//! whether to process or drop an incoming message. This is the wrapper
//! around the cache predicate ([`super::is_active_or_grace`]) and the
//! offline-grace fail-open layer ([`super::state_freshness`]).
//!
//! Per `feedback_subscription_gating_v1.md`:
//!
//! 1. Active / Grace / post-lapse-14d → Allow, no warning.
//! 2. Stale cache (>30d since last Companion sync) → Allow, Doctor warning
//!    ("offline grace") — punished customers on holiday is the failure mode
//!    we're avoiding.
//! 3. Anything else → Block, ingestion call site drops the message + logs.
//!
//! The "Hub partially cripples without active subscription" behavior is
//! THIS gate, applied to every channel's new-message entry point. Read /
//! existing-data flows are NOT gated — only new-data writes.

use chrono::{DateTime, Utc};
use zeroclaw_config::schema::SubscriptionConfig;

use super::{is_active_or_grace, state_freshness, StalenessVerdict};

/// The reason a [`GateDecision`] resolved as it did. Surfaced into logs +
/// the Doctor view via the channel call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateReason {
    /// Subscription is `Active` (the normal state).
    Active,
    /// Subscription is `Grace` and `grace_period_end > now`.
    InGrace,
    /// Subscription is `Expired` but within the 14d post-lapse window.
    PostLapseSoftLanding,
    /// Subscription is past the 30d offline-grace cutoff but we fail open
    /// to avoid penalising customers on holiday. The channel call site
    /// MUST surface a Doctor warning when receiving this verdict.
    OfflineGraceFailOpen,
    /// Subscription is `Inactive` — block.
    Inactive,
    /// Subscription `Expired` more than 14d ago AND last sync was within
    /// the 30d window — block (the customer is reachable but lapsed).
    LapsedBeyondPostLapseWindow,
}

/// The verdict returned by [`gate_new_data_ingestion`]. Channel call sites
/// pattern-match: `Allow` → process the message; `Block` → drop the
/// message + log + (optionally) emit a Doctor signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// Process the incoming message. If `doctor_warn` is true, the call
    /// site should emit a "subscription state stale" warning so the
    /// operator sees it in Doctor.
    Allow {
        reason: GateReason,
        doctor_warn: bool,
    },
    /// Drop the message + log. Customer-visible effect: new-data
    /// ingestion paused. Existing data remains accessible via
    /// non-ingestion code paths.
    Block { reason: GateReason },
}

impl GateDecision {
    /// `true` iff the call site should process the message.
    #[must_use]
    pub fn allow(self) -> bool {
        matches!(self, Self::Allow { .. })
    }

    /// The reason enum, regardless of allow/block.
    #[must_use]
    pub fn reason(self) -> GateReason {
        match self {
            Self::Allow { reason, .. } | Self::Block { reason } => reason,
        }
    }

    /// `true` iff the call site should emit a Doctor warning.
    #[must_use]
    pub fn doctor_warn(self) -> bool {
        matches!(
            self,
            Self::Allow {
                doctor_warn: true,
                ..
            }
        )
    }
}

/// Decide whether to let an incoming new-data ingestion call through.
///
/// Channel name is logged + surfaced in Doctor; otherwise unused. Pass
/// the channel's stable identifier (`"imessage"`, `"gmail"`, `"whatsapp"`,
/// `"safari_capture"`, etc.).
///
/// `now` is injected so unit tests can pin time. Production callers pass
/// `Utc::now()`.
#[must_use]
pub fn gate_new_data_ingestion(
    channel_name: &str,
    state: &SubscriptionConfig,
    now: DateTime<Utc>,
) -> GateDecision {
    let _ = channel_name; // reserved for future per-channel policy
    use zeroclaw_config::schema::SubscriptionStatus;

    // Layer 1: pure cache predicate.
    if is_active_or_grace(state, now) {
        let reason = match state.status {
            SubscriptionStatus::Active => GateReason::Active,
            SubscriptionStatus::Grace => GateReason::InGrace,
            SubscriptionStatus::Expired => GateReason::PostLapseSoftLanding,
            SubscriptionStatus::Inactive => unreachable!(
                "is_active_or_grace returned true for Inactive — predicate invariant"
            ),
        };
        return GateDecision::Allow {
            reason,
            doctor_warn: false,
        };
    }

    // Layer 2: offline-grace fail-open. If the cache says we're not
    // active/grace, but the cache itself is stale beyond 30d, we let
    // ingestion through to avoid punishing customers whose Companion
    // simply hasn't synced (on holiday, phone dead, etc.).
    if state_freshness(state, now) == StalenessVerdict::OfflineGrace
        && state.status != SubscriptionStatus::Inactive
    {
        return GateDecision::Allow {
            reason: GateReason::OfflineGraceFailOpen,
            doctor_warn: true,
        };
    }

    // Layer 3: block. Differentiate "fresh-but-inactive" from
    // "lapsed-more-than-14d" so logs are useful.
    let reason = match state.status {
        SubscriptionStatus::Inactive => GateReason::Inactive,
        SubscriptionStatus::Expired => GateReason::LapsedBeyondPostLapseWindow,
        // Active/Grace can only land here if both predicates returned
        // false, which means the timestamps are malformed. Treat as
        // Inactive (defensive default).
        _ => GateReason::Inactive,
    };
    GateDecision::Block { reason }
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

    fn cfg(
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

    // ── Active path ──────────────────────────────────────────────

    #[test]
    fn active_with_fresh_sync_allows_no_warning() {
        let now = fixed_now();
        let state = cfg(SubscriptionStatus::Active, now - chrono::Duration::days(1), None);
        let d = gate_new_data_ingestion("imessage", &state, now);
        assert!(d.allow());
        assert_eq!(d.reason(), GateReason::Active);
        assert!(!d.doctor_warn());
    }

    #[test]
    fn active_with_old_sync_still_allows_via_layer_1() {
        // Active overrides freshness — a paid sub that hasn't re-synced is
        // still paid.
        let now = fixed_now();
        let state = cfg(
            SubscriptionStatus::Active,
            now - chrono::Duration::days(60),
            None,
        );
        let d = gate_new_data_ingestion("imessage", &state, now);
        assert!(d.allow());
        assert_eq!(d.reason(), GateReason::Active);
        assert!(!d.doctor_warn());
    }

    // ── Grace path ───────────────────────────────────────────────

    #[test]
    fn grace_within_window_allows() {
        let now = fixed_now();
        let state = cfg(
            SubscriptionStatus::Grace,
            now - chrono::Duration::days(1),
            Some(now + chrono::Duration::days(7)),
        );
        let d = gate_new_data_ingestion("imessage", &state, now);
        assert!(d.allow());
        assert_eq!(d.reason(), GateReason::InGrace);
        assert!(!d.doctor_warn());
    }

    #[test]
    fn grace_past_window_blocks() {
        let now = fixed_now();
        let state = cfg(
            SubscriptionStatus::Grace,
            now - chrono::Duration::days(1),
            Some(now - chrono::Duration::hours(1)),
        );
        let d = gate_new_data_ingestion("imessage", &state, now);
        assert!(!d.allow());
    }

    // ── Expired (post-lapse soft landing) ────────────────────────

    #[test]
    fn expired_within_14d_allows_post_lapse_soft_landing() {
        let now = fixed_now();
        let state = cfg(
            SubscriptionStatus::Expired,
            now - chrono::Duration::days(7),
            None,
        );
        let d = gate_new_data_ingestion("imessage", &state, now);
        assert!(d.allow());
        assert_eq!(d.reason(), GateReason::PostLapseSoftLanding);
        assert!(!d.doctor_warn());
    }

    #[test]
    fn expired_beyond_14d_blocks_with_lapsed_reason() {
        let now = fixed_now();
        let state = cfg(
            SubscriptionStatus::Expired,
            now - chrono::Duration::days(20),
            None,
        );
        let d = gate_new_data_ingestion("imessage", &state, now);
        assert!(!d.allow());
        assert_eq!(d.reason(), GateReason::LapsedBeyondPostLapseWindow);
    }

    // ── Offline-grace fail-open ──────────────────────────────────

    #[test]
    fn expired_with_31d_old_sync_fails_open_with_doctor_warn() {
        // Customer was Expired, then phone went dark for 31 days. We let
        // ingestion through (fail-open) but Doctor warns.
        let now = fixed_now();
        let state = cfg(
            SubscriptionStatus::Expired,
            now - chrono::Duration::days(31),
            None,
        );
        let d = gate_new_data_ingestion("whatsapp", &state, now);
        assert!(d.allow());
        assert_eq!(d.reason(), GateReason::OfflineGraceFailOpen);
        assert!(d.doctor_warn());
    }

    #[test]
    fn grace_with_old_sync_past_30d_fails_open_with_doctor_warn() {
        // Grace status with stale cache — fail open + warn.
        let now = fixed_now();
        let state = cfg(
            SubscriptionStatus::Grace,
            now - chrono::Duration::days(45),
            Some(now - chrono::Duration::days(40)), // grace expired too
        );
        let d = gate_new_data_ingestion("imessage", &state, now);
        assert!(d.allow());
        assert_eq!(d.reason(), GateReason::OfflineGraceFailOpen);
        assert!(d.doctor_warn());
    }

    // ── Inactive blocks regardless of age ────────────────────────

    #[test]
    fn inactive_with_fresh_sync_blocks() {
        let now = fixed_now();
        let state = cfg(
            SubscriptionStatus::Inactive,
            now - chrono::Duration::days(1),
            None,
        );
        let d = gate_new_data_ingestion("imessage", &state, now);
        assert!(!d.allow());
        assert_eq!(d.reason(), GateReason::Inactive);
    }

    #[test]
    fn inactive_with_30d_old_sync_still_blocks_no_fail_open() {
        // Spec is explicit: Inactive short-circuits regardless of age.
        let now = fixed_now();
        let state = cfg(
            SubscriptionStatus::Inactive,
            now - chrono::Duration::days(35),
            None,
        );
        let d = gate_new_data_ingestion("imessage", &state, now);
        assert!(!d.allow());
        assert_eq!(d.reason(), GateReason::Inactive);
    }

    // ── Edge cases ───────────────────────────────────────────────

    #[test]
    fn malformed_grace_with_malformed_last_validated_fails_open() {
        // Malformed timestamps in non-Inactive states intentionally fail
        // open via the offline-grace layer — the policy comment in
        // state_freshness is explicit: "Malformed timestamp on disk —
        // treat as offline-grace to avoid hard-blocking legitimate users
        // on a parsing bug." That's the right behavior; ingestion
        // continues + Doctor warns + the operator can correct.
        let now = fixed_now();
        let mut state = cfg(SubscriptionStatus::Grace, now, None);
        state.last_validated_at = String::from("not-a-date");
        state.grace_period_end = String::from("also-not-a-date");
        let d = gate_new_data_ingestion("imessage", &state, now);
        assert!(d.allow());
        assert_eq!(d.reason(), GateReason::OfflineGraceFailOpen);
        assert!(d.doctor_warn());
    }

    #[test]
    fn malformed_last_validated_at_expired_falls_through_to_offline_grace_layer() {
        // state_freshness treats malformed as OfflineGrace, so the layer-2
        // fail-open kicks in for non-Inactive statuses.
        let now = fixed_now();
        let mut state = cfg(SubscriptionStatus::Expired, now, None);
        state.last_validated_at = String::from("");
        let d = gate_new_data_ingestion("imessage", &state, now);
        assert!(d.allow());
        assert_eq!(d.reason(), GateReason::OfflineGraceFailOpen);
        assert!(d.doctor_warn());
    }

    // ── GateDecision API ─────────────────────────────────────────

    #[test]
    fn allow_method_returns_true_for_all_allow_variants() {
        let allow_active = GateDecision::Allow {
            reason: GateReason::Active,
            doctor_warn: false,
        };
        let allow_offline = GateDecision::Allow {
            reason: GateReason::OfflineGraceFailOpen,
            doctor_warn: true,
        };
        let block = GateDecision::Block {
            reason: GateReason::Inactive,
        };
        assert!(allow_active.allow());
        assert!(allow_offline.allow());
        assert!(!block.allow());
    }

    #[test]
    fn doctor_warn_only_true_when_offline_fail_open() {
        let warn = GateDecision::Allow {
            reason: GateReason::OfflineGraceFailOpen,
            doctor_warn: true,
        };
        let no_warn = GateDecision::Allow {
            reason: GateReason::Active,
            doctor_warn: false,
        };
        assert!(warn.doctor_warn());
        assert!(!no_warn.doctor_warn());
    }
}
