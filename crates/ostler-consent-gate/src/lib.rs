//! Ostler consent gate.
//!
//! Privacy-sensitive Ostler subsystems (whatsapp-bridge, cm041 voice
//! ingestion) MUST refuse to start without a current "accepted"
//! consent record at `~/.ostler/posture/consent.json`. The Hub
//! installer + the Rust `ostler-assistant setup channels` CLI write
//! these records via the Python helper at
//! `ostler_security.consent_cli`; this crate is the read-side gate
//! the daemons call at startup.
//!
//! Wording-hash drift detection is built in: each known tickbox has
//! a SHA-256 of the bundled wording string compiled in; if the stored
//! record's hash differs, [`require`] returns
//! [`GateError::WordingDrift`] so Doctor / the daemon can flag amber
//! and request a renewal flow.
//!
//! ## Wiring
//!
//! ```ignore
//! // In whatsapp-bridge listen() at the top:
//! ostler_consent_gate::require(ostler_consent_gate::Tickbox::WhatsAppUnofficialRisk)?;
//! ```
//!
//! ```ignore
//! // In cm041 voice ingestion startup, after region detection:
//! if region == "eu" {
//!     ostler_consent_gate::require(ostler_consent_gate::Tickbox::VoiceSpeakerIdEu)?;
//! }
//! ```
//!
//! ## File layout
//!
//! `~/.ostler/posture/consent.json` (note: `posture/`, NOT
//! `security-posture/`) shaped as:
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "records": {
//!     "whatsapp_unofficial_risk": {
//!       "tickbox_id": "...",
//!       "wording_hash": "<sha256-hex>",
//!       "decision": "accepted" | "declined",
//!       ...
//!     }
//!   }
//! }
//! ```
//!
//! The full schema is documented in
//! `HR015/ostler_security/consent.py`. This crate only needs the
//! fields read here.
//!
//! ## Wording bundling
//!
//! The compiled-in wording strings here MUST match
//! `HR015/legal/consent_strings.py` byte-for-byte. The Python side is
//! the source of truth; if you change wording there, regenerate the
//! constants here via the helper:
//!
//! ```sh
//! python3 -m legal.dump_hashes  # planned helper; today: copy by hand
//! ```

#![deny(missing_docs)]
#![deny(rust_2018_idioms)]

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Bundled wording strings keyed by tickbox id.
///
/// MUST match `HR015/legal/consent_strings.py` byte-for-byte. The
/// SHA-256 of each string is what we compare against the stored
/// record's `wording_hash`. A mismatch means the user agreed to old
/// wording and a renewal is required.
mod wording;

/// Identifiers for the v0.1 consent tickboxes.
///
/// More tickboxes can be added without renaming existing ones –
/// renames break round-trip lookup for users with stored records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tickbox {
    /// EU-only Article 9 explicit consent for processing
    /// special-category data.
    Article9SpecialCategory,
    /// WhatsApp unofficial-Web risk acknowledgement.
    WhatsAppUnofficialRisk,
    /// EU voice speaker-identification gate (NOT emotion).
    VoiceSpeakerIdEu,
}

impl Tickbox {
    /// Stable identifier matching `HR015/legal/consent_strings.py`.
    pub fn id(self) -> &'static str {
        match self {
            Self::Article9SpecialCategory => "article_9_special_category_consent",
            Self::WhatsAppUnofficialRisk => "whatsapp_unofficial_risk",
            Self::VoiceSpeakerIdEu => "voice_speaker_id_eu",
        }
    }

    /// SHA-256 hex of the currently-bundled wording string.
    pub fn bundled_wording_hash(self) -> String {
        let text = match self {
            Self::Article9SpecialCategory => wording::ARTICLE_9_EU_CONSENT,
            Self::WhatsAppUnofficialRisk => wording::WHATSAPP_UNOFFICIAL_RISK_CONSENT,
            Self::VoiceSpeakerIdEu => wording::EU_VOICE_SPEAKER_ID_CONSENT,
        };
        let mut hasher = Sha256::new();
        hasher.update(text.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

/// Reasons the gate may refuse to open.
#[derive(Debug, thiserror::Error)]
pub enum GateError {
    /// No record exists for the requested tickbox at all. The user
    /// has not been shown the consent screen.
    #[error(
        "consent record missing for tickbox '{0}': run install.sh or `ostler-assistant setup channels --interactive`"
    )]
    Missing(String),

    /// The user explicitly declined. The daemon must not start.
    #[error("consent declined for tickbox '{0}'")]
    Declined(String),

    /// The stored wording hash differs from the currently-bundled
    /// wording. Renewal flow needed before the gate opens.
    #[error(
        "consent wording drift for tickbox '{tickbox}': stored hash {stored} != bundled hash {bundled}; user must re-confirm new wording"
    )]
    WordingDrift {
        /// Tickbox id.
        tickbox: String,
        /// SHA-256 hex from the stored record.
        stored: String,
        /// SHA-256 hex of the currently-bundled wording.
        bundled: String,
    },

    /// The consent.json file is malformed. Treat as missing for
    /// safety – daemon refuses to start.
    #[error("consent registry malformed at {0}: {1}")]
    Malformed(PathBuf, String),

    /// An I/O error other than "file not found" reading the
    /// consent file. (File-not-found is reported as
    /// [`GateError::Missing`].)
    #[error("could not read consent registry at {0}: {1}")]
    Io(PathBuf, std::io::Error),
}

impl GateError {
    /// Convert into a stable string identifier suitable for posture
    /// markers / structured logs. The `Display` impl is the long
    /// human-readable form; this is the short machine-readable form.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Missing(_) => "consent_missing",
            Self::Declined(_) => "consent_declined",
            Self::WordingDrift { .. } => "consent_stale_wording",
            Self::Malformed(..) => "consent_registry_malformed",
            Self::Io(..) => "consent_registry_io_error",
        }
    }
}

/// Refuse to proceed unless the user has a current "accepted"
/// record for `tickbox`.
///
/// The default registry path is
/// `${OSTLER_HOME:-$HOME/.ostler}/posture/consent.json`. Tests
/// should use [`require_at`] to point at a temp dir.
pub fn require(tickbox: Tickbox) -> Result<(), GateError> {
    require_at(tickbox, &default_consent_path())
}

/// Same as [`require`] but takes an explicit path. Plumbing for tests.
pub fn require_at(tickbox: Tickbox, path: &Path) -> Result<(), GateError> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(GateError::Missing(tickbox.id().to_string()));
        }
        Err(e) => return Err(GateError::Io(path.to_path_buf(), e)),
    };

    let registry: Registry = serde_json::from_str(&text)
        .map_err(|e| GateError::Malformed(path.to_path_buf(), e.to_string()))?;

    let record = registry
        .records
        .get(tickbox.id())
        .ok_or_else(|| GateError::Missing(tickbox.id().to_string()))?;

    if record.decision != "accepted" {
        return Err(GateError::Declined(tickbox.id().to_string()));
    }

    let bundled = tickbox.bundled_wording_hash();
    if record.wording_hash != bundled {
        return Err(GateError::WordingDrift {
            tickbox: tickbox.id().to_string(),
            stored: record.wording_hash.clone(),
            bundled,
        });
    }

    Ok(())
}

/// Default location of the consent registry file. Honours
/// `OSTLER_HOME` and falls back to `$HOME/.ostler`.
pub fn default_consent_path() -> PathBuf {
    let base = std::env::var("OSTLER_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".ostler")
        });
    base.join("posture").join("consent.json")
}

#[derive(Deserialize)]
struct Registry {
    #[serde(default)]
    records: std::collections::HashMap<String, Record>,
}

#[derive(Deserialize)]
struct Record {
    #[serde(default)]
    wording_hash: String,
    #[serde(default)]
    decision: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_registry(dir: &Path, body: &str) -> PathBuf {
        let posture = dir.join("posture");
        fs::create_dir_all(&posture).unwrap();
        let path = posture.join("consent.json");
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn missing_file_is_missing_error() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("does-not-exist.json");
        let err = require_at(Tickbox::WhatsAppUnofficialRisk, &path).unwrap_err();
        assert!(matches!(err, GateError::Missing(_)));
        assert_eq!(err.reason(), "consent_missing");
    }

    #[test]
    fn record_missing_is_missing_error() {
        let td = tempfile::tempdir().unwrap();
        let path = write_registry(td.path(), r#"{"schema_version":1,"records":{}}"#);
        let err = require_at(Tickbox::WhatsAppUnofficialRisk, &path).unwrap_err();
        assert!(matches!(err, GateError::Missing(_)));
    }

    #[test]
    fn declined_record_is_declined_error() {
        let td = tempfile::tempdir().unwrap();
        let body = format!(
            r#"{{"schema_version":1,"records":{{"whatsapp_unofficial_risk":{{"wording_hash":"{}","decision":"declined"}}}}}}"#,
            Tickbox::WhatsAppUnofficialRisk.bundled_wording_hash(),
        );
        let path = write_registry(td.path(), &body);
        let err = require_at(Tickbox::WhatsAppUnofficialRisk, &path).unwrap_err();
        assert!(matches!(err, GateError::Declined(_)));
        assert_eq!(err.reason(), "consent_declined");
    }

    #[test]
    fn wording_drift_is_drift_error() {
        let td = tempfile::tempdir().unwrap();
        let body = r#"{"schema_version":1,"records":{"whatsapp_unofficial_risk":{"wording_hash":"deadbeef","decision":"accepted"}}}"#;
        let path = write_registry(td.path(), body);
        let err = require_at(Tickbox::WhatsAppUnofficialRisk, &path).unwrap_err();
        assert!(matches!(err, GateError::WordingDrift { .. }));
        assert_eq!(err.reason(), "consent_stale_wording");
    }

    #[test]
    fn current_accepted_record_passes() {
        let td = tempfile::tempdir().unwrap();
        let body = format!(
            r#"{{"schema_version":1,"records":{{"whatsapp_unofficial_risk":{{"wording_hash":"{}","decision":"accepted"}}}}}}"#,
            Tickbox::WhatsAppUnofficialRisk.bundled_wording_hash(),
        );
        let path = write_registry(td.path(), &body);
        require_at(Tickbox::WhatsAppUnofficialRisk, &path).expect("gate should open");
    }

    #[test]
    fn malformed_registry_is_malformed_error() {
        let td = tempfile::tempdir().unwrap();
        let path = write_registry(td.path(), "{not json");
        let err = require_at(Tickbox::WhatsAppUnofficialRisk, &path).unwrap_err();
        assert!(matches!(err, GateError::Malformed(..)));
        assert_eq!(err.reason(), "consent_registry_malformed");
    }

    #[test]
    fn each_tickbox_has_distinct_hash() {
        let a = Tickbox::Article9SpecialCategory.bundled_wording_hash();
        let b = Tickbox::WhatsAppUnofficialRisk.bundled_wording_hash();
        let c = Tickbox::VoiceSpeakerIdEu.bundled_wording_hash();
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
        assert_eq!(a.len(), 64);
        assert_eq!(b.len(), 64);
        assert_eq!(c.len(), 64);
    }
}
