//! iMessage TCC (Transparency, Consent, and Control) probe.
//!
//! macOS gates AppleEvents to Messages.app behind the user's
//! explicit consent (System Settings → Privacy & Security →
//! Automation). When the operator has not granted permission,
//! `osascript`-based delivery (see `zeroclaw-channels::imessage`)
//! fails with error -1743 (`errAEEventNotPermitted`) and the
//! brief silently never reaches the operator.
//!
//! This module performs a non-destructive probe by issuing a
//! minimal AppleEvent (`tell application "Messages" to count of
//! accounts`) and classifying the result. The probe does not send
//! a message; it only asks for a property that requires the same
//! Automation permission a real send would.
//!
//! Used by:
//! - `zeroclaw doctor` (synchronous probe; emits a DiagItem).
//! - Daemon startup (one-shot probe; marks the `imessage-tcc`
//!   health component so the wiki posture surface and gateway
//!   `/health` endpoint reflect TCC state without the operator
//!   waiting for a brief to be silently dropped).
//!
//! See piece D of
//! /tmp/tnm_brief_zeroclaw_cron_diagnosis_2026-05-02.md
//! and task #278 (iMessage TCC bundle).

/// Outcome of an `imessage-tcc` probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImessageTccStatus {
    /// AppleEvents to Messages.app are authorised; delivery should
    /// succeed (subject to network + Apple ID state).
    Authorized,
    /// User has denied (or has not granted) Automation permission
    /// for the daemon's Messages.app target. The string carries the
    /// raw stderr fragment so doctor / log surfaces can include it
    /// verbatim for support contexts.
    Denied(String),
    /// Probe ran but the result did not match the known
    /// success/denied shapes -- typically transient (`osascript`
    /// not on PATH, Messages.app not installed, AppleScript syntax
    /// regression). Treat as warn, not error.
    Unknown(String),
    /// Non-macOS host. Probe always returns this on Linux/Windows.
    NotMacOs,
}

impl ImessageTccStatus {
    pub fn as_health_status(&self) -> Option<&'static str> {
        match self {
            ImessageTccStatus::Authorized => Some("ok"),
            ImessageTccStatus::Denied(_) => Some("error"),
            // Unknown + NotMacOs do not move the marker -- doctor
            // surfaces them as warnings rather than a hard error.
            _ => None,
        }
    }
}

/// Probe AppleEvents permission for Messages.app on macOS.
/// Synchronous; spawns `osascript` and waits up to ~5 seconds.
///
/// Non-macOS callers always get `ImessageTccStatus::NotMacOs`.
#[cfg(target_os = "macos")]
pub fn probe() -> ImessageTccStatus {
    use std::process::Command;
    use std::time::Duration;

    // Minimal Apple Event that requires the same Automation
    // permission a real send would. Returns a number on success.
    let script = r#"tell application "Messages" to count of accounts"#;

    let mut child = match Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            return ImessageTccStatus::Unknown(format!("failed to spawn osascript: {err}"));
        }
    };

    let pid = child.id();
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() > Duration::from_secs(5) => {
                let _ = child.kill();
                return ImessageTccStatus::Unknown(format!(
                    "osascript probe timed out after 5s (pid {pid})"
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(err) => {
                return ImessageTccStatus::Unknown(format!("osascript wait failed: {err}"));
            }
        }
    }

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(err) => {
            return ImessageTccStatus::Unknown(format!("osascript output read failed: {err}"));
        }
    };

    if output.status.success() {
        return ImessageTccStatus::Authorized;
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    classify_stderr(&stderr)
}

#[cfg(not(target_os = "macos"))]
pub fn probe() -> ImessageTccStatus {
    ImessageTccStatus::NotMacOs
}

/// Classify osascript stderr into the canonical TCC error class.
/// Pulled out so unit tests can lock the parsing without spawning
/// `osascript`.
fn classify_stderr(stderr: &str) -> ImessageTccStatus {
    let lower = stderr.to_ascii_lowercase();
    if stderr.contains("-1743")
        || lower.contains("not allowed to send apple events")
        || lower.contains("not authorised to send apple events")
        || lower.contains("not authorized to send apple events")
    {
        return ImessageTccStatus::Denied(stderr.trim().to_string());
    }
    ImessageTccStatus::Unknown(stderr.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_stderr_recognises_minus_1743() {
        let raw = "execution error: Messages got an error: \
            Not authorised to send Apple events to Messages. (-1743)";
        match classify_stderr(raw) {
            ImessageTccStatus::Denied(captured) => {
                assert!(captured.contains("-1743"));
            }
            other => panic!("expected Denied, got {other:?}"),
        }
    }

    #[test]
    fn classify_stderr_recognises_us_spelling() {
        let raw = "Not authorized to send Apple events to Messages.";
        assert!(matches!(classify_stderr(raw), ImessageTccStatus::Denied(_)));
    }

    #[test]
    fn classify_stderr_returns_unknown_for_other_errors() {
        let raw = "syntax error: A unknown token. (-2741)";
        assert!(matches!(
            classify_stderr(raw),
            ImessageTccStatus::Unknown(_)
        ));
    }

    #[test]
    fn as_health_status_only_maps_authorized_and_denied() {
        assert_eq!(ImessageTccStatus::Authorized.as_health_status(), Some("ok"));
        assert_eq!(
            ImessageTccStatus::Denied("x".into()).as_health_status(),
            Some("error")
        );
        assert!(
            ImessageTccStatus::Unknown("x".into())
                .as_health_status()
                .is_none()
        );
        assert!(ImessageTccStatus::NotMacOs.as_health_status().is_none());
    }
}
