//! PWG wing-resolution bridge.
//!
//! Polls a file-backed queue produced by the PWG hub gateway
//! (`personal-world-graph/services/gateway/src/routes/wing_resolution_queue.py`,
//! merged in PWG #5), formats each ask per the CM031 #28 ask_mode
//! contract, and posts the prompt to the configured iMessage
//! recipient via any [`Channel`] implementation.
//!
//! Why a file-based queue rather than HTTP webhook: the
//! ostler-assistant daemon runs on the customer Mac next to PWG
//! storage, while the gateway runs in the same network segment.
//! A bind-mounted directory or NFS share crosses the trust
//! boundary without standing up a second auth surface. PWG #5
//! writes one JSON file per queued ask; this bridge consumes
//! them.
//!
//! Wire shape (per defib + the gateway emitter):
//!
//! ```jsonc
//! {
//!   "queued_id":          "9c1f...",
//!   "session_id":         "remote-2026-05-01-23-15",
//!   "candidates": [
//!     { "label": "speaker_a", "person_id": "p_43",
//!       "display_name": "Alice", "confidence": 0.71,
//!       "reasoning": "voice match + last seen Tue" }
//!   ],
//!   "calendar_attendees": ["alice@example.com"],
//!   "ask_mode":           "now",          // now | later | never
//!   "queued_at":          "2026-05-01T23:15:00Z"
//! }
//! ```
//!
//! `ask_mode` handling:
//! - `now`     -- format and DM immediately, then consume the file.
//! - `later`   -- leave the file in place; the next tick will
//!                reconsider once the operator's settings allow
//!                proactive prompts. Open question (PR body):
//!                whether to roll a `later/` subdir to avoid
//!                re-formatting on every poll.
//! - `never`   -- consume without sending. The operator dialled
//!                the assistant out of proactive resolution and
//!                expects the queue to drain silently.
//!
//! The bridge is feature-gated behind `pwg-wing-resolution` so
//! the upstream zeroclaw distribution does not pull in PWG
//! product surface by default.
//!
//! British English throughout.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::fs;
use zeroclaw_api::channel::{Channel, SendMessage};

/// One queued wing-resolution ask, as emitted by the PWG gateway.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct QueuedAsk {
    pub queued_id: String,
    pub session_id: String,
    pub candidates: Vec<SpeakerCandidate>,
    #[serde(default)]
    pub calendar_attendees: Vec<String>,
    pub ask_mode: AskMode,
    pub queued_at: String,
}

/// Candidate speaker label resolved (or attempted) by the hub LLM.
///
/// Mirrors the wire shape from CM041 PR #2's
/// `/api/v1/speakers/identify` response (see also CM042's
/// `SpeakerCandidate` struct in `RemoteCapture/PWG/SpeakerResolution.swift`).
/// `person_id` and `display_name` are optional because the LLM
/// may fail to resolve a label to a known person.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SpeakerCandidate {
    pub label: String,
    #[serde(default)]
    pub person_id: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    pub confidence: f64,
    #[serde(default)]
    pub reasoning: Option<String>,
}

/// Operator's "ask me later / never / now" dial (CM031 #28).
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AskMode {
    Now,
    Later,
    Never,
}

/// Counters returned by [`WingResolutionBridge::run_once`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RunReport {
    pub scanned: usize,
    pub delivered: usize,
    pub deferred: usize,
    pub skipped: usize,
    pub malformed: usize,
}

/// Default queue directory used when `WING_RESOLUTION_QUEUE_DIR`
/// is unset. Matches the gateway's default in PWG #5.
pub const DEFAULT_QUEUE_DIR: &str = "/var/lib/pwg/wing-resolution-queue";

/// Subdirectory inside the queue dir where malformed payloads are
/// rotated. The bridge never deletes a payload it could not
/// parse: an operator may want to inspect the file later, and a
/// silent delete would mask gateway emitter bugs.
const QUARANTINE_SUBDIR: &str = "malformed";

/// File-poll bridge from the PWG wing-resolution queue to a
/// [`Channel`] (typically iMessage).
#[derive(Debug, Clone)]
pub struct WingResolutionBridge {
    queue_dir: PathBuf,
    recipient: String,
    poll_interval: Duration,
}

impl WingResolutionBridge {
    /// Build a bridge with explicit queue dir and recipient.
    pub fn new(queue_dir: impl Into<PathBuf>, recipient: impl Into<String>) -> Self {
        Self {
            queue_dir: queue_dir.into(),
            recipient: recipient.into(),
            poll_interval: Duration::from_secs(30),
        }
    }

    /// Override the poll interval used by the (caller-driven) loop.
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Read configuration from environment.
    ///
    /// `WING_RESOLUTION_QUEUE_DIR` (default
    /// [`DEFAULT_QUEUE_DIR`]) is the queue directory; the
    /// recipient must come from the caller because the assistant
    /// channel config knows the operator's address, not this
    /// bridge.
    pub fn from_env(recipient: impl Into<String>) -> Self {
        let queue_dir = std::env::var("WING_RESOLUTION_QUEUE_DIR")
            .unwrap_or_else(|_| DEFAULT_QUEUE_DIR.to_owned());
        Self::new(queue_dir, recipient)
    }

    pub fn queue_dir(&self) -> &Path {
        &self.queue_dir
    }

    pub fn recipient(&self) -> &str {
        &self.recipient
    }

    pub fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    /// Process every `*.json` file in the queue directory once.
    ///
    /// Errors during single-file handling (send failure, I/O on
    /// quarantine) propagate out and abort the tick; a future
    /// caller-driven loop is responsible for retry policy. A
    /// missing queue directory is NOT an error: the gateway may
    /// not have queued anything yet.
    pub async fn run_once(&self, channel: &dyn Channel) -> Result<RunReport> {
        let mut report = RunReport::default();

        let mut entries = match fs::read_dir(&self.queue_dir).await {
            Ok(it) => it,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(
                    queue_dir = %self.queue_dir.display(),
                    "wing-resolution queue dir absent; nothing to do"
                );
                return Ok(report);
            }
            Err(e) => {
                return Err(anyhow::Error::from(e)).with_context(|| {
                    format!(
                        "failed to read wing-resolution queue dir {}",
                        self.queue_dir.display()
                    )
                });
            }
        };

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if !is_json_file(&path) {
                continue;
            }
            report.scanned += 1;

            let bytes = fs::read(&path).await.with_context(|| {
                format!("failed to read wing-resolution payload {}", path.display())
            })?;

            let ask: QueuedAsk = match serde_json::from_slice(&bytes) {
                Ok(a) => a,
                Err(err) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "malformed wing-resolution payload; quarantining"
                    );
                    self.quarantine(&path).await?;
                    report.malformed += 1;
                    continue;
                }
            };

            match ask.ask_mode {
                AskMode::Now => {
                    let body = format_prompt(&ask);
                    let msg = SendMessage::new(body, &self.recipient);
                    channel.send(&msg).await.with_context(|| {
                        format!(
                            "wing-resolution iMessage send failed for queued_id {}",
                            ask.queued_id
                        )
                    })?;
                    self.consume(&path).await?;
                    report.delivered += 1;
                }
                AskMode::Later => {
                    // Open question (PR body): batching strategy.
                    // Leaving the file in place re-formats on every
                    // poll; rotating into a `later/` subdir would
                    // amortise. For v0.1 keep it simple and let the
                    // operator's dial decide when "later" promotes
                    // to "now".
                    tracing::debug!(
                        queued_id = %ask.queued_id,
                        "wing-resolution ask deferred (ask_mode=later)"
                    );
                    report.deferred += 1;
                }
                AskMode::Never => {
                    // Operator has dialled proactive prompts off.
                    // Consume so the queue drains rather than
                    // growing forever.
                    self.consume(&path).await?;
                    report.skipped += 1;
                }
            }
        }

        Ok(report)
    }

    async fn consume(&self, path: &Path) -> Result<()> {
        fs::remove_file(path)
            .await
            .with_context(|| format!("failed to consume {}", path.display()))?;
        Ok(())
    }

    async fn quarantine(&self, path: &Path) -> Result<()> {
        let dest_dir = self.queue_dir.join(QUARANTINE_SUBDIR);
        fs::create_dir_all(&dest_dir).await.with_context(|| {
            format!("failed to ensure quarantine dir {}", dest_dir.display())
        })?;
        let name = path
            .file_name()
            .map(std::ffi::OsStr::to_os_string)
            .unwrap_or_else(|| std::ffi::OsString::from("unknown.json"));
        let dest = dest_dir.join(name);
        fs::rename(path, &dest).await.with_context(|| {
            format!(
                "failed to move malformed payload {} to {}",
                path.display(),
                dest.display()
            )
        })?;
        Ok(())
    }
}

fn is_json_file(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "json") && path.is_file()
}

/// Format the iMessage prompt body.
///
/// Per Phase 0.5 spec the wording is tight:
/// `Your <time> call -- A) <name1> B) <name2> C) Other?`
///
/// `<time>` is derived from `queued_at`. We format as the local
/// `HH:MM` of the timestamp so the operator immediately knows
/// which call the prompt is about. If `queued_at` is unparseable
/// (gateway emitter bug) we fall back to `latest` rather than
/// blocking the prompt.
///
/// When fewer than two named candidates are available we still
/// render whatever we have plus the `Other?` option -- a single
/// candidate is rare but possible (e.g. the LLM produced one
/// strong match with no runner-up).
pub fn format_prompt(ask: &QueuedAsk) -> String {
    let time_label = format_time_label(&ask.queued_at);

    // Take up to the first two candidates with a usable display
    // name. A candidate without display_name falls back to its
    // label so the operator has *something* to recognise.
    let named: Vec<&str> = ask
        .candidates
        .iter()
        .map(|c| {
            c.display_name
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| c.label.as_str())
        })
        .take(2)
        .collect();

    match named.as_slice() {
        [a, b] => format!("Your {time_label} call -- A) {a} B) {b} C) Other?"),
        [a] => format!("Your {time_label} call -- A) {a} B) Other?"),
        _ => format!("Your {time_label} call -- who was on this one?"),
    }
}

fn format_time_label(queued_at: &str) -> String {
    queued_at
        .parse::<DateTime<Utc>>()
        .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_else(|_| "latest".to_owned())
}

#[cfg(test)]
mod tests;
