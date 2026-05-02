//! Bundled consent-wording strings.
//!
//! These MUST match `HR015/legal/consent_strings.py` byte-for-byte.
//! The text files in `wording_data/` were generated from that
//! Python module on 2026-05-03; regenerate them whenever the Python
//! constants change (the consent gate detects drift via SHA-256 and
//! refuses to start, which is intentional but disruptive – keep the
//! sources in sync).
//!
//! Generation recipe:
//!
//! ```sh
//! python3 -c "
//! import sys
//! sys.path.insert(0, '/Users/andy/Documents/Projects/HR015 - Gaming PC')
//! from legal import (
//!     ARTICLE_9_EU_CONSENT,
//!     WHATSAPP_UNOFFICIAL_RISK_CONSENT,
//!     EU_VOICE_SPEAKER_ID_CONSENT,
//! )
//! open('article_9.txt', 'w').write(ARTICLE_9_EU_CONSENT.text)
//! open('whatsapp.txt', 'w').write(WHATSAPP_UNOFFICIAL_RISK_CONSENT.text)
//! open('voice.txt',    'w').write(EU_VOICE_SPEAKER_ID_CONSENT.text)
//! "
//! ```

/// EU-only Article 9 explicit-consent screen wording.
pub const ARTICLE_9_EU_CONSENT: &str = include_str!("wording_data/article_9.txt");

/// WhatsApp unofficial-Web risk-allocation tickbox wording.
pub const WHATSAPP_UNOFFICIAL_RISK_CONSENT: &str =
    include_str!("wording_data/whatsapp.txt");

/// EU voice speaker-ID gate wording (NOT emotion).
pub const EU_VOICE_SPEAKER_ID_CONSENT: &str = include_str!("wording_data/voice.txt");
