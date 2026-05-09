//! Thin-data brief detection.
//!
//! The cron-driven daily brief composes a "what's on for today"
//! summary by pulling from calendar, recent messages, email
//! inbox, and the people graph. For a fresh customer (Day 1) or
//! a quiet-day customer, those signals are simultaneously thin —
//! and asking the LLM to generate a brief from nothing tends to
//! produce hallucinated to-dos and synthetic urgency. Worse: it
//! makes the assistant feel like junk-mail on day one, and Andy
//! has been clear that the launch-week experience must not be
//! that.
//!
//! Strategy:
//!
//! - Score five signals (calendar, outbound traffic, inbound
//!   email, people-graph freshness, install age) into a
//!   [`ThinnessSignals`] aggregate.
//! - A brief is "thin" if **3 or more** of those signals fire.
//!   Lower than that and the brief composer has enough material
//!   for a normal data-rich brief.
//! - The reasons that fired are returned to the caller so the
//!   composer can pick the right thin-data prompt template.
//!
//! Threshold derivation: 3-of-5 catches Day 1 (every signal
//! fires), quiet weekends (calendar empty, install old —
//! 1 signal — *not* thin), and dormancy patterns (calendar
//! empty + thin email + stale people graph = 3-of-5, thin).
//! See module tests for the worked examples.
//!
//! British English throughout.

use chrono::{DateTime, Duration, Utc};

/// Raw counts the cron composer collects from the live data
/// sources before classification. All fields are observation-
/// only — the classifier doesn't mutate them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThinnessSignals {
    /// Calendar events scheduled for the brief's target day,
    /// counting both timed events and all-day commitments.
    pub calendar_events_today: u32,

    /// Number of outbound messages the customer sent across all
    /// channels (iMessage / WhatsApp / email) in the last 48h.
    /// Captures "the customer is on a screen and using their
    /// life" — a strong signal that there's something to brief
    /// about even if the calendar is empty.
    pub outbound_messages_48h: u32,

    /// New email messages received overnight (since the
    /// previous evening's wrap brief). 0–2 inbound messages =
    /// quiet inbox; 3+ = there's news to surface.
    pub inbound_email_overnight: u32,

    /// Age in days of the freshest entry in the customer's
    /// people-graph (CM048-derived storage). On a Day-1
    /// install this is large (no entries, treated as +inf-ish);
    /// once contact ingest has had a few days to land it
    /// drops to under 7.
    pub people_graph_freshness_days: u32,

    /// Time since the customer's `~/Documents/Ostler/`
    /// directory was first created. The directory landing
    /// during install is the canonical "Day 1 start" marker.
    /// Less than 48h => the install is fresh and we should
    /// be gentle; more than 48h => warm-up complete.
    pub install_age: Duration,
}

impl ThinnessSignals {
    /// Convenience constructor for a "Day 1, no data" customer.
    /// Mostly useful as a baseline in tests and as the
    /// fallback signals when one of the live data probes fails
    /// (e.g. CalDAV unreachable). Failing probes shouldn't
    /// turn a real brief thin — the composer should re-probe
    /// or fall back to "skip the thin classifier" — but having
    /// a stable Day-1-ish default keeps `is_thin` honest in
    /// the degraded case.
    pub fn day_one() -> Self {
        Self {
            calendar_events_today: 0,
            outbound_messages_48h: 0,
            inbound_email_overnight: 0,
            people_graph_freshness_days: 365,
            install_age: Duration::hours(1),
        }
    }
}

/// Reasons a brief was classified thin. The composer maps
/// these to the right thin-data prompt template (e.g. a
/// `DayOne` reason picks the gentle "your assistant just woke
/// up" copy, while `EmptyCalendar` picks the observational
/// weather hook).
///
/// Order is stable — the variant order matches the priority
/// the composer should apply when picking copy: the most
/// specific reason first.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ThinReason {
    /// The install is less than 48 hours old. Fresh customers
    /// get the "your assistant just woke up; give it a few
    /// days" copy.
    DayOne,

    /// No timed events and no all-day commitments on the
    /// brief's target day.
    EmptyCalendar,

    /// The customer hasn't sent more than a handful of
    /// outbound messages in the last 48 hours. Often
    /// correlates with travel / illness / annual leave.
    QuietOutbound,

    /// Inbound email overnight is sparse (0–2). No news to
    /// surface, so the brief shouldn't pretend otherwise.
    QuietInbox,

    /// The people graph hasn't had a fresh entry in over a
    /// week. Either the customer's CM048 ingest pipeline is
    /// quiet or the install is mid-warm-up.
    StalePeopleGraph,
}

/// Inbound email is "quiet" at fewer than this many overnight
/// messages.
const QUIET_INBOX_THRESHOLD: u32 = 3;

/// Outbound messaging is "quiet" at fewer than this many in
/// the last 48 hours. Calibrated low so a customer who's on
/// holiday but still tapping out the occasional reply doesn't
/// trip thinness on this signal alone.
const QUIET_OUTBOUND_48H_THRESHOLD: u32 = 5;

/// People-graph freshness threshold: stale at 7 days.
const STALE_PEOPLE_GRAPH_DAYS: u32 = 7;

/// Install age threshold for "Day 1": 48 hours.
const DAY_ONE_HOURS: i64 = 48;

/// Number of signals required to flip a brief into thin-data
/// mode. 3-of-5 is the calibration point — see module docs
/// for the derivation.
const THIN_THRESHOLD: usize = 3;

/// Classify the brief signals into a thin-data verdict + the
/// list of reasons that fired.
///
/// Caller contract: pass freshly-collected signals; do not
/// reuse a `ThinnessSignals` from a previous brief run, since
/// the install-age and freshness fields are time-relative.
pub fn is_thin(signals: &ThinnessSignals) -> (bool, Vec<ThinReason>) {
    let mut reasons: Vec<ThinReason> = Vec::with_capacity(5);

    if signals.install_age < Duration::hours(DAY_ONE_HOURS) {
        reasons.push(ThinReason::DayOne);
    }
    if signals.calendar_events_today == 0 {
        reasons.push(ThinReason::EmptyCalendar);
    }
    if signals.outbound_messages_48h < QUIET_OUTBOUND_48H_THRESHOLD {
        reasons.push(ThinReason::QuietOutbound);
    }
    if signals.inbound_email_overnight < QUIET_INBOX_THRESHOLD {
        reasons.push(ThinReason::QuietInbox);
    }
    if signals.people_graph_freshness_days >= STALE_PEOPLE_GRAPH_DAYS {
        reasons.push(ThinReason::StalePeopleGraph);
    }

    let is_thin = reasons.len() >= THIN_THRESHOLD;
    (is_thin, reasons)
}

/// Helper: derive `install_age` from a `~/Documents/Ostler/`
/// creation timestamp. Wraps the chrono arithmetic so the
/// calling site reads naturally.
pub fn install_age_from(now: DateTime<Utc>, ostler_dir_created_at: DateTime<Utc>) -> Duration {
    if now < ostler_dir_created_at {
        // Clock skew: clamp to zero rather than producing a
        // negative duration. The classifier reads negative
        // durations as "younger than zero hours" which would
        // wrongly trip DayOne on a future-dated mtime.
        Duration::zero()
    } else {
        now - ostler_dir_created_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_one_install_with_no_data_is_thin_with_all_signals() {
        let signals = ThinnessSignals::day_one();
        let (thin, reasons) = is_thin(&signals);
        assert!(thin, "Day 1 install should be classified thin");
        // Every signal fires.
        assert!(reasons.contains(&ThinReason::DayOne));
        assert!(reasons.contains(&ThinReason::EmptyCalendar));
        assert!(reasons.contains(&ThinReason::QuietOutbound));
        assert!(reasons.contains(&ThinReason::QuietInbox));
        assert!(reasons.contains(&ThinReason::StalePeopleGraph));
    }

    #[test]
    fn busy_calendar_with_active_inbox_is_not_thin() {
        let signals = ThinnessSignals {
            calendar_events_today: 4,
            outbound_messages_48h: 30,
            inbound_email_overnight: 8,
            people_graph_freshness_days: 1,
            install_age: Duration::days(60),
        };
        let (thin, reasons) = is_thin(&signals);
        assert!(!thin, "Active customer should not be classified thin");
        assert!(reasons.is_empty(), "no thin reasons should fire: {reasons:?}");
    }

    #[test]
    fn empty_calendar_alone_is_not_thin_threshold_three() {
        let signals = ThinnessSignals {
            calendar_events_today: 0,
            outbound_messages_48h: 30,
            inbound_email_overnight: 8,
            people_graph_freshness_days: 1,
            install_age: Duration::days(60),
        };
        let (thin, reasons) = is_thin(&signals);
        assert!(
            !thin,
            "Empty calendar alone (1 signal) is below 3-of-5 threshold"
        );
        assert_eq!(reasons, vec![ThinReason::EmptyCalendar]);
    }

    #[test]
    fn empty_calendar_plus_quiet_outbound_plus_quiet_inbox_is_thin() {
        let signals = ThinnessSignals {
            calendar_events_today: 0,
            outbound_messages_48h: 2,
            inbound_email_overnight: 1,
            people_graph_freshness_days: 1,
            install_age: Duration::days(60),
        };
        let (thin, reasons) = is_thin(&signals);
        assert!(thin, "3 signals (calendar + outbound + inbox) is thin");
        assert_eq!(reasons.len(), 3);
        assert!(reasons.contains(&ThinReason::EmptyCalendar));
        assert!(reasons.contains(&ThinReason::QuietOutbound));
        assert!(reasons.contains(&ThinReason::QuietInbox));
    }

    #[test]
    fn day_one_marker_fires_under_48h() {
        let signals = ThinnessSignals {
            calendar_events_today: 5,
            outbound_messages_48h: 50,
            inbound_email_overnight: 10,
            people_graph_freshness_days: 0,
            install_age: Duration::hours(36),
        };
        let (thin, reasons) = is_thin(&signals);
        // Only DayOne fires; that's 1-of-5, below threshold.
        assert!(!thin);
        assert_eq!(reasons, vec![ThinReason::DayOne]);
    }

    #[test]
    fn day_one_marker_does_not_fire_above_48h() {
        let signals = ThinnessSignals {
            install_age: Duration::hours(49),
            ..ThinnessSignals::day_one()
        };
        let (thin, reasons) = is_thin(&signals);
        // 4-of-5 fires (everything except DayOne).
        assert!(thin);
        assert!(!reasons.contains(&ThinReason::DayOne));
    }

    #[test]
    fn install_age_helper_handles_clock_skew() {
        let now = chrono::DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
        let future = chrono::DateTime::<Utc>::from_timestamp(2_000, 0).unwrap();
        // Future-dated dir => clamp to zero, not a negative duration.
        let age = install_age_from(now, future);
        assert_eq!(age, Duration::zero());
    }

    #[test]
    fn install_age_helper_normal_path() {
        let earlier = chrono::DateTime::<Utc>::from_timestamp(1_000_000, 0).unwrap();
        let later = chrono::DateTime::<Utc>::from_timestamp(1_000_000 + 7200, 0).unwrap();
        // 2-hour gap.
        let age = install_age_from(later, earlier);
        assert_eq!(age, Duration::seconds(7200));
    }

    #[test]
    fn quiet_inbox_threshold_boundary() {
        // 2 messages overnight => quiet (below threshold of 3).
        let mut signals = ThinnessSignals {
            calendar_events_today: 5,
            outbound_messages_48h: 50,
            inbound_email_overnight: 2,
            people_graph_freshness_days: 1,
            install_age: Duration::days(60),
        };
        let (_, reasons) = is_thin(&signals);
        assert!(reasons.contains(&ThinReason::QuietInbox));

        // 3 messages overnight => not quiet (at threshold).
        signals.inbound_email_overnight = 3;
        let (_, reasons) = is_thin(&signals);
        assert!(!reasons.contains(&ThinReason::QuietInbox));
    }
}
