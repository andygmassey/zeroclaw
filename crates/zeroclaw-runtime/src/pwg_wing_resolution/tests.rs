//! Unit tests for the PWG wing-resolution bridge.
//!
//! Coverage:
//! - `ask_mode = now`   formats and posts via the channel, then consumes the file.
//! - `ask_mode = later` leaves the file in place and increments `deferred`.
//! - `ask_mode = never` consumes the file without posting.
//! - Empty queue dir (or absent dir) returns an empty report cleanly.
//! - Malformed payload is rotated to `malformed/` rather than panicking
//!   or being silently deleted.
//! - `format_prompt` produces the tight Phase 0.5 wording for two
//!   candidates and degrades gracefully with fewer.

use super::*;

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use tempfile::TempDir;
use tokio::sync::mpsc;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

// ── Test channel ──────────────────────────────────────────────────────

#[derive(Default, Clone)]
struct CapturingChannel {
    sends: Arc<Mutex<Vec<SendMessage>>>,
    fail_next: Arc<Mutex<bool>>,
}

impl CapturingChannel {
    fn sent(&self) -> Vec<SendMessage> {
        self.sends.lock().clone()
    }

    fn fail_next(&self) {
        *self.fail_next.lock() = true;
    }
}

#[async_trait]
impl Channel for CapturingChannel {
    fn name(&self) -> &str {
        "test-capture"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        if std::mem::take(&mut *self.fail_next.lock()) {
            anyhow::bail!("simulated send failure");
        }
        self.sends.lock().push(message.clone());
        Ok(())
    }

    async fn listen(&self, _tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────

fn write_payload(dir: &Path, name: &str, payload: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, payload).expect("write payload");
    path
}

fn now_payload(queued_id: &str) -> String {
    serde_json::json!({
        "queued_id":          queued_id,
        "session_id":         "remote-2026-05-01-23-15",
        "candidates": [
            { "label": "speaker_a", "person_id": "p_43",
              "display_name": "Alice", "confidence": 0.71 },
            { "label": "speaker_b", "person_id": "p_88",
              "display_name": "Bob", "confidence": 0.55 }
        ],
        "calendar_attendees": ["alice@example.com"],
        "ask_mode":           "now",
        "queued_at":          "2026-05-01T23:15:00Z"
    })
    .to_string()
}

fn later_payload(queued_id: &str) -> String {
    serde_json::json!({
        "queued_id":          queued_id,
        "session_id":         "remote-2026-05-01-23-15",
        "candidates": [
            { "label": "speaker_a", "display_name": "Alice", "confidence": 0.61 }
        ],
        "calendar_attendees": [],
        "ask_mode":           "later",
        "queued_at":          "2026-05-01T23:15:00Z"
    })
    .to_string()
}

fn never_payload(queued_id: &str) -> String {
    serde_json::json!({
        "queued_id":          queued_id,
        "session_id":         "remote-2026-05-01-23-15",
        "candidates": [],
        "calendar_attendees": [],
        "ask_mode":           "never",
        "queued_at":          "2026-05-01T23:15:00Z"
    })
    .to_string()
}

// ── ask_mode dispatch ─────────────────────────────────────────────────

#[tokio::test]
async fn now_posts_dm_and_consumes_file() {
    let dir = TempDir::new().unwrap();
    let path = write_payload(dir.path(), "ask-now.json", &now_payload("q1"));

    let channel = CapturingChannel::default();
    let bridge = WingResolutionBridge::new(dir.path(), "+15551234567");

    let report = bridge.run_once(&channel).await.unwrap();
    assert_eq!(report.scanned, 1);
    assert_eq!(report.delivered, 1);
    assert_eq!(report.deferred, 0);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.malformed, 0);

    let sent = channel.sent();
    assert_eq!(sent.len(), 1, "one DM expected");
    assert_eq!(sent[0].recipient, "+15551234567");
    assert!(
        sent[0].content.contains("Alice"),
        "DM body should contain candidate name: {}",
        sent[0].content
    );
    assert!(
        sent[0].content.contains("Bob"),
        "DM body should contain second candidate: {}",
        sent[0].content
    );
    assert!(
        sent[0].content.contains("Other?"),
        "DM body should offer the Other option: {}",
        sent[0].content
    );

    assert!(!path.exists(), "now-mode payload should be consumed");
}

#[tokio::test]
async fn later_leaves_file_and_skips_send() {
    let dir = TempDir::new().unwrap();
    let path = write_payload(dir.path(), "ask-later.json", &later_payload("q2"));

    let channel = CapturingChannel::default();
    let bridge = WingResolutionBridge::new(dir.path(), "+15551234567");

    let report = bridge.run_once(&channel).await.unwrap();
    assert_eq!(report.scanned, 1);
    assert_eq!(report.delivered, 0);
    assert_eq!(report.deferred, 1);
    assert!(channel.sent().is_empty(), "later-mode must not send");
    assert!(path.exists(), "later-mode payload must remain on disk");
}

#[tokio::test]
async fn never_consumes_without_send() {
    let dir = TempDir::new().unwrap();
    let path = write_payload(dir.path(), "ask-never.json", &never_payload("q3"));

    let channel = CapturingChannel::default();
    let bridge = WingResolutionBridge::new(dir.path(), "+15551234567");

    let report = bridge.run_once(&channel).await.unwrap();
    assert_eq!(report.scanned, 1);
    assert_eq!(report.skipped, 1);
    assert_eq!(report.delivered, 0);
    assert!(channel.sent().is_empty(), "never-mode must not send");
    assert!(!path.exists(), "never-mode payload must drain");
}

// ── Empty / missing queue ────────────────────────────────────────────

#[tokio::test]
async fn empty_queue_returns_empty_report() {
    let dir = TempDir::new().unwrap();
    let channel = CapturingChannel::default();
    let bridge = WingResolutionBridge::new(dir.path(), "+15551234567");

    let report = bridge.run_once(&channel).await.unwrap();
    assert_eq!(report, RunReport::default());
    assert!(channel.sent().is_empty());
}

#[tokio::test]
async fn missing_queue_dir_is_not_an_error() {
    // Gateway has not queued anything yet; bridge runs before
    // PWG #5's emitter spawns the dir. This must not panic and
    // must not error out: the daemon's tick loop calls run_once
    // every poll interval and a noisy error per tick would
    // saturate logs.
    let dir = TempDir::new().unwrap();
    let absent = dir.path().join("does-not-exist");
    let channel = CapturingChannel::default();
    let bridge = WingResolutionBridge::new(&absent, "+15551234567");

    let report = bridge.run_once(&channel).await.unwrap();
    assert_eq!(report, RunReport::default());
}

// ── Malformed payload ────────────────────────────────────────────────

#[tokio::test]
async fn malformed_payload_is_quarantined_not_deleted() {
    let dir = TempDir::new().unwrap();
    let bad = write_payload(dir.path(), "bad.json", "{ not valid json");
    let good = write_payload(dir.path(), "good.json", &now_payload("q-good"));

    let channel = CapturingChannel::default();
    let bridge = WingResolutionBridge::new(dir.path(), "+15551234567");

    let report = bridge.run_once(&channel).await.unwrap();
    assert_eq!(report.malformed, 1);
    assert_eq!(report.delivered, 1);

    // Quarantined, not deleted: an operator may want to inspect
    // the bad payload, and a silent delete would mask gateway
    // emitter bugs.
    assert!(!bad.exists(), "malformed file should leave the queue dir");
    let quarantined = dir.path().join("malformed").join("bad.json");
    assert!(
        quarantined.exists(),
        "malformed file should be in quarantine: {}",
        quarantined.display()
    );

    // Good payload still processed in the same tick.
    assert!(!good.exists(), "good payload should be consumed");
    assert_eq!(channel.sent().len(), 1);
}

// ── Format ───────────────────────────────────────────────────────────

#[test]
fn format_prompt_two_candidates_uses_phase_0_5_wording() {
    let ask: QueuedAsk = serde_json::from_str(&now_payload("q")).unwrap();
    let body = format_prompt(&ask);
    assert!(body.starts_with("Your "), "wording: {body}");
    assert!(body.contains(" call -- "), "delimiter: {body}");
    assert!(body.contains("A) Alice"), "first option: {body}");
    assert!(body.contains("B) Bob"), "second option: {body}");
    assert!(body.contains("C) Other?"), "third option: {body}");
}

#[test]
fn format_prompt_one_candidate_still_offers_other_option() {
    let ask: QueuedAsk = serde_json::from_str(&later_payload("q")).unwrap();
    let body = format_prompt(&ask);
    assert!(body.contains("A) Alice"));
    assert!(body.contains("B) Other?"));
    assert!(
        !body.contains("C) "),
        "no third option without runner-up: {body}"
    );
}

#[test]
fn format_prompt_zero_candidates_falls_back_safely() {
    let ask: QueuedAsk = serde_json::from_str(&never_payload("q")).unwrap();
    let body = format_prompt(&ask);
    // A "never" payload is normally skipped, but format_prompt
    // is a pure function and should not panic on the empty case.
    assert!(body.contains("who was on this one?"));
}

#[test]
fn format_prompt_caps_at_two_named_candidates() {
    // Even if the gateway emits three candidates, the prompt
    // contract is two named + Other -- so the third must drop.
    let payload = serde_json::json!({
        "queued_id": "q",
        "session_id": "s",
        "candidates": [
            { "label": "a", "display_name": "Alice", "confidence": 0.7 },
            { "label": "b", "display_name": "Bob",   "confidence": 0.5 },
            { "label": "c", "display_name": "Carol", "confidence": 0.3 }
        ],
        "calendar_attendees": [],
        "ask_mode": "now",
        "queued_at": "2026-05-01T23:15:00Z"
    });
    let ask: QueuedAsk = serde_json::from_value(payload).unwrap();
    let body = format_prompt(&ask);
    assert!(body.contains("A) Alice"));
    assert!(body.contains("B) Bob"));
    assert!(!body.contains("Carol"), "third candidate must drop: {body}");
}

#[test]
fn format_prompt_falls_back_to_label_when_display_name_missing() {
    let payload = serde_json::json!({
        "queued_id": "q",
        "session_id": "s",
        "candidates": [
            { "label": "speaker_a", "confidence": 0.7 },
            { "label": "speaker_b", "confidence": 0.5 }
        ],
        "calendar_attendees": [],
        "ask_mode": "now",
        "queued_at": "2026-05-01T23:15:00Z"
    });
    let ask: QueuedAsk = serde_json::from_value(payload).unwrap();
    let body = format_prompt(&ask);
    assert!(body.contains("speaker_a"));
    assert!(body.contains("speaker_b"));
}

#[test]
fn format_prompt_unparseable_queued_at_falls_back_to_latest() {
    let payload = serde_json::json!({
        "queued_id": "q",
        "session_id": "s",
        "candidates": [{ "label": "a", "display_name": "Alice", "confidence": 0.7 }],
        "calendar_attendees": [],
        "ask_mode": "now",
        "queued_at": "not-a-timestamp"
    });
    let ask: QueuedAsk = serde_json::from_value(payload).unwrap();
    let body = format_prompt(&ask);
    assert!(body.contains("Your latest call"));
}

// ── Env ──────────────────────────────────────────────────────────────

#[test]
fn from_env_uses_default_when_var_unset() {
    // SAFETY: this single-test process modifies env briefly. The
    // remove_var pair brackets the read so a parallel test that
    // sets WING_RESOLUTION_QUEUE_DIR for itself does not pollute
    // the assertion. Acceptable for a single check; if more
    // env-sensitive tests land they should serialise via a
    // shared mutex.
    let prev = std::env::var("WING_RESOLUTION_QUEUE_DIR").ok();
    // SAFETY: env mutation in a single-threaded test scope.
    unsafe {
        std::env::remove_var("WING_RESOLUTION_QUEUE_DIR");
    }
    let bridge = WingResolutionBridge::from_env("+15551234567");
    assert_eq!(bridge.queue_dir(), Path::new(DEFAULT_QUEUE_DIR));
    if let Some(p) = prev {
        // SAFETY: restoring prior value.
        unsafe {
            std::env::set_var("WING_RESOLUTION_QUEUE_DIR", p);
        }
    }
}

// ── Hard-fail discipline ─────────────────────────────────────────────

#[tokio::test]
async fn send_failure_propagates_and_leaves_file_for_retry() {
    // If the iMessage send fails (Messages.app not signed in,
    // recipient invalid, etc.), the bridge must NOT consume the
    // file -- otherwise the operator silently loses prompts.
    // run_once propagates the error so the caller's retry loop
    // has the next tick to retry.
    let dir = TempDir::new().unwrap();
    let path = write_payload(dir.path(), "ask-now.json", &now_payload("q-fail"));

    let channel = CapturingChannel::default();
    channel.fail_next();
    let bridge = WingResolutionBridge::new(dir.path(), "+15551234567");

    let result = bridge.run_once(&channel).await;
    assert!(result.is_err(), "send failure must propagate");
    assert!(path.exists(), "failed-send payload must NOT be consumed");
}
