//! G9 subscription-state gate for cron job dispatch.
//!
//! Per the locked Ostler v1.0 pricing decision
//! (memory `project-pricing-v1-0-locked-2026-05-27`) and the gating
//! design (`feedback-subscription-gating-v1`), the Hub ships Ostler Pro
//! as a per-customer subscription. Some scheduled work -- daily-brief
//! composition, brief delivery, Reminders push -- should pause when the
//! customer's subscription is inactive, while infrastructure jobs
//! (backups, heartbeat, log-rotate, health-checks) must keep running.
//!
//! ## Declarative-per-job opt-in (Option 3)
//!
//! Rather than encoding which job-ids are gated in the scheduler, every
//! cron job declares its own gating posture via the `subscription_gated`
//! field on `CronJob` / `CronJobDecl`. Future job authors set
//! `subscription_gated = true` alongside the cron definition without
//! touching this module.
//!
//! ## State file contract (matches CM051's `subscription_gate.py`)
//!
//! Source of truth: `~/.ostler/state/subscription_state.json`
//! (overridable via `OSTLER_SUBSCRIPTION_STATE` for tests). Schema:
//!
//! ```json
//! {
//!   "status": "active" | "grace" | "inactive",
//!   "expires_at": "<RFC3339>",            // optional
//!   "grace_period_end": "<RFC3339>",      // optional
//!   "last_validated_at": "<RFC3339>",     // optional
//!   "source": "first_month_free" | "companion" | "hub_initial" | "default",
//!   "receipt": "<base64>"                 // optional
//! }
//! ```
//!
//! ## Gate decision (mirrors Python helper byte-for-byte)
//!
//! `gate_allows_run` returns `true` when:
//!
//! 1. `status == "active"`, OR
//! 2. `status == "grace"` AND `grace_period_end` is in the future, OR
//! 3. Offline-grace fail-open: `last_validated_at` is within the last
//!    30 days regardless of `status` (Apple-restraint: never punish a
//!    legitimate customer on infrastructure failure we cannot observe).
//!
//! And returns `false` otherwise (genuinely inactive customer with no
//! recent Companion contact).
//!
//! ## Fail-open on read errors
//!
//! Missing file, unreadable file, malformed JSON, IO error -- every
//! error path runs the job and emits a single `tracing::warn!`. Never
//! crash the scheduler; never silently lock the customer out. This
//! matches the Python helper's `_load()` semantics (corrupt files
//! degrade to `status=inactive,source=default`, but the offline-grace
//! branch is the safety net for legitimate customers).

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::Deserialize;
use std::path::PathBuf;

const SUBSCRIPTION_STATE_ENV: &str = "OSTLER_SUBSCRIPTION_STATE";
const SUBSCRIPTION_STATE_REL: &[&str] = &[".ostler", "state", "subscription_state.json"];

/// Matches the locked CM051 helper constant.
const OFFLINE_GRACE_DAYS: i64 = 30;

const STATUS_ACTIVE: &str = "active";
const STATUS_GRACE: &str = "grace";

/// Parsed subset of the subscription state file. Only the fields the
/// gate needs to make a decision are deserialised; extra fields
/// (`source`, `receipt`, `expires_at`) are tolerated.
#[derive(Debug, Deserialize, Default)]
struct SubscriptionState {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    grace_period_end: Option<String>,
    #[serde(default)]
    last_validated_at: Option<String>,
}

/// Resolve the state-file path. `OSTLER_SUBSCRIPTION_STATE` overrides
/// the default, mirroring the Python helper for round-trip test parity.
fn state_file_path() -> PathBuf {
    if let Ok(override_path) = std::env::var(SUBSCRIPTION_STATE_ENV)
        && !override_path.is_empty()
    {
        return PathBuf::from(override_path);
    }
    let home = std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut path = home;
    for segment in SUBSCRIPTION_STATE_REL {
        path.push(segment);
    }
    path
}

/// Parse an RFC-3339 / ISO-8601 timestamp tolerating the `Z` suffix
/// the Python helper writes.
fn parse_rfc3339(raw: &str) -> Option<DateTime<Utc>> {
    let normalised = raw.trim();
    if normalised.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(normalised)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Decide whether a subscription-gated job is allowed to dispatch now.
///
/// Wraps the four-branch resolution from the locked design with
/// fail-open semantics on every error path. Pure -- takes the parsed
/// state and the current time so unit tests can construct any state
/// without touching the disk.
fn allow_from_state(state: &SubscriptionState, now: DateTime<Utc>) -> bool {
    let status = state.status.as_deref().unwrap_or("inactive");

    if status == STATUS_ACTIVE {
        return true;
    }

    if status == STATUS_GRACE
        && let Some(grace_end) = state.grace_period_end.as_deref().and_then(parse_rfc3339)
        && grace_end > now
    {
        return true;
    }

    // Offline-grace fail-open: even if status is "inactive" (or "grace"
    // with grace_end already past), recent Companion validation means
    // the customer is legitimate. Hub never blocks on infrastructure
    // failure it cannot observe.
    if let Some(last_validated) = state.last_validated_at.as_deref().and_then(parse_rfc3339) {
        let age = now - last_validated;
        if age < ChronoDuration::days(OFFLINE_GRACE_DAYS) {
            return true;
        }
    }

    false
}

/// Outcome of the subscription gate check. Distinguishes "ran the gate
/// and it allowed the job" from "ran the gate and it denied the job"
/// from "fail-open path (state unreadable / corrupt)" so the
/// `tracing::warn!` only fires on the last branch and tests can assert
/// the exact axis without reading log output.
#[derive(Debug, PartialEq, Eq)]
pub enum GateDecision {
    /// Subscription state is active / in-grace / within offline grace.
    Allowed,
    /// Subscription state is genuinely inactive -- skip this dispatch.
    Skipped,
    /// State file missing or unreadable -- fail open and run the job.
    FailOpenMissing,
    /// State file present but malformed -- fail open and run the job.
    FailOpenCorrupt,
}

impl GateDecision {
    /// Whether this decision means the job should run.
    pub fn allows_run(&self) -> bool {
        !matches!(self, Self::Skipped)
    }
}

/// Read the state file and decide. Public entry point for the
/// scheduler. Never panics; always returns a definite decision.
pub fn check_gate(now: DateTime<Utc>) -> GateDecision {
    let path = state_file_path();

    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Pre-install / pre-first-month-free -- fail open. The
            // install.sh `activate_first_month_free` writer creates the
            // file; before that the customer is in the pre-activation
            // window and the scheduler must not block them.
            return GateDecision::FailOpenMissing;
        }
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "subscription_gate: failed to read state file; failing open"
            );
            return GateDecision::FailOpenMissing;
        }
    };

    let state: SubscriptionState = match serde_json::from_str(&raw) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "subscription_gate: failed to parse state file; failing open"
            );
            return GateDecision::FailOpenCorrupt;
        }
    };

    if allow_from_state(&state, now) {
        GateDecision::Allowed
    } else {
        GateDecision::Skipped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::sync::Mutex;
    use tempfile::TempDir;

    // Serialise tests that touch the OSTLER_SUBSCRIPTION_STATE env var.
    // cargo runs tests in parallel by default; without this, two tests
    // that both rewrite the override would race.
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: Mutex<()> = Mutex::new(());
        &LOCK
    }

    fn write_state(tmp: &TempDir, body: &str) -> PathBuf {
        let path = tmp.path().join("subscription_state.json");
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn allow_from_state_active_returns_true() {
        let state = SubscriptionState {
            status: Some("active".into()),
            ..Default::default()
        };
        let now = Utc.with_ymd_and_hms(2026, 6, 1, 9, 0, 0).unwrap();
        assert!(allow_from_state(&state, now));
    }

    #[test]
    fn allow_from_state_grace_in_window_returns_true() {
        let now = Utc.with_ymd_and_hms(2026, 6, 1, 9, 0, 0).unwrap();
        let grace_end = now + ChronoDuration::days(3);
        let state = SubscriptionState {
            status: Some("grace".into()),
            grace_period_end: Some(grace_end.to_rfc3339()),
            ..Default::default()
        };
        assert!(allow_from_state(&state, now));
    }

    #[test]
    fn allow_from_state_grace_expired_falls_through_to_offline_grace_check() {
        let now = Utc.with_ymd_and_hms(2026, 6, 1, 9, 0, 0).unwrap();
        // grace_end in the past, no recent Companion validation
        let grace_end = now - ChronoDuration::days(1);
        let state = SubscriptionState {
            status: Some("grace".into()),
            grace_period_end: Some(grace_end.to_rfc3339()),
            last_validated_at: None,
        };
        assert!(!allow_from_state(&state, now));
    }

    #[test]
    fn allow_from_state_offline_grace_inside_window_returns_true() {
        let now = Utc.with_ymd_and_hms(2026, 6, 1, 9, 0, 0).unwrap();
        let state = SubscriptionState {
            status: Some("inactive".into()),
            last_validated_at: Some((now - ChronoDuration::days(15)).to_rfc3339()),
            ..Default::default()
        };
        assert!(
            allow_from_state(&state, now),
            "offline-grace fail-open should allow inactive customer within 30d window"
        );
    }

    #[test]
    fn allow_from_state_offline_grace_outside_window_returns_false() {
        let now = Utc.with_ymd_and_hms(2026, 6, 1, 9, 0, 0).unwrap();
        let state = SubscriptionState {
            status: Some("inactive".into()),
            last_validated_at: Some((now - ChronoDuration::days(45)).to_rfc3339()),
            ..Default::default()
        };
        assert!(!allow_from_state(&state, now));
    }

    #[test]
    fn allow_from_state_default_inactive_returns_false() {
        let state = SubscriptionState::default();
        let now = Utc.with_ymd_and_hms(2026, 6, 1, 9, 0, 0).unwrap();
        assert!(!allow_from_state(&state, now));
    }

    #[test]
    fn check_gate_active_state_file_allows_run() {
        let _guard = env_lock().lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let path = write_state(&tmp, r#"{"status":"active"}"#);
        // SAFETY: tests serialised via env_lock; single-threaded mutation.
        unsafe { std::env::set_var(SUBSCRIPTION_STATE_ENV, &path) };
        let now = Utc.with_ymd_and_hms(2026, 6, 1, 9, 0, 0).unwrap();
        assert_eq!(check_gate(now), GateDecision::Allowed);
        unsafe { std::env::remove_var(SUBSCRIPTION_STATE_ENV) };
    }

    #[test]
    fn check_gate_inactive_state_file_skips() {
        let _guard = env_lock().lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let path = write_state(&tmp, r#"{"status":"inactive"}"#);
        unsafe { std::env::set_var(SUBSCRIPTION_STATE_ENV, &path) };
        let now = Utc.with_ymd_and_hms(2026, 6, 1, 9, 0, 0).unwrap();
        assert_eq!(check_gate(now), GateDecision::Skipped);
        unsafe { std::env::remove_var(SUBSCRIPTION_STATE_ENV) };
    }

    #[test]
    fn check_gate_missing_state_file_fails_open() {
        let _guard = env_lock().lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("does_not_exist.json");
        unsafe { std::env::set_var(SUBSCRIPTION_STATE_ENV, &missing) };
        let now = Utc.with_ymd_and_hms(2026, 6, 1, 9, 0, 0).unwrap();
        let decision = check_gate(now);
        assert_eq!(decision, GateDecision::FailOpenMissing);
        assert!(
            decision.allows_run(),
            "missing state must fail-open (legitimate pre-install customer)"
        );
        unsafe { std::env::remove_var(SUBSCRIPTION_STATE_ENV) };
    }

    #[test]
    fn check_gate_corrupt_state_file_fails_open() {
        let _guard = env_lock().lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let path = write_state(&tmp, "{not valid json");
        unsafe { std::env::set_var(SUBSCRIPTION_STATE_ENV, &path) };
        let now = Utc.with_ymd_and_hms(2026, 6, 1, 9, 0, 0).unwrap();
        let decision = check_gate(now);
        assert_eq!(decision, GateDecision::FailOpenCorrupt);
        assert!(decision.allows_run());
        unsafe { std::env::remove_var(SUBSCRIPTION_STATE_ENV) };
    }

    #[test]
    fn gate_decision_allows_run_is_only_false_for_skipped() {
        assert!(GateDecision::Allowed.allows_run());
        assert!(GateDecision::FailOpenMissing.allows_run());
        assert!(GateDecision::FailOpenCorrupt.allows_run());
        assert!(!GateDecision::Skipped.allows_run());
    }
}
