//! Wording-drift guard for the bundled mirror of HR015's
//! `legal/consent_strings.py`.
//!
//! Asserts that the SHA-256 of every `wording_data/*.txt` file
//! compiled into this crate matches the value pinned in
//! `tests/consent_wording_hashes.json`. The JSON file is designed to
//! be byte-identical to HR015's `tests/consent_wording_hashes.json`
//! (manual sync for v0.1; later a pre-commit hook can do it).
//!
//! Failure modes:
//!
//! - drift: a `wording_data/*.txt` file was edited without bumping
//!   the JSON. Fix: regenerate the txt from HR015 source AND bump
//!   the JSON in BOTH repos.
//! - missing-in-json: a new `Tickbox` variant was added with its
//!   bundled wording but the JSON wasn't updated. Fix: add the
//!   pinned hash entry in BOTH repos.
//! - dangling-in-json: the JSON has a `tickbox_id` that no longer
//!   matches any `Tickbox` variant. Fix: remove the entry in BOTH
//!   repos.
//!
//! This is the Rust-side half of the cross-repo guard. The Python
//! half lives in `HR015/legal/check_wording.py` (run via
//! `make check-consent-wording`).

use std::collections::HashMap;

use ostler_consent_gate::Tickbox;

/// Compile-time pinned hashes JSON, byte-identical to HR015's
/// `tests/consent_wording_hashes.json`. Embedding it via
/// `include_str!` means there is no runtime filesystem dependency
/// and the test is reproducible across CI runners.
const PINNED_JSON: &str = include_str!("consent_wording_hashes.json");

fn parse_pins() -> HashMap<String, String> {
    serde_json::from_str(PINNED_JSON).expect(
        "tests/consent_wording_hashes.json must be valid JSON \
         shaped {tickbox_id: sha256_hex}",
    )
}

/// Single source of truth for which `Tickbox` variants we expect to
/// be pinned. Adding a new variant in `lib.rs` requires adding it
/// here AND in the JSON file in both repos. The compiler will not
/// catch the omission for you because Rust enums do not auto-iterate;
/// this list is the manual gate.
fn known_tickboxes() -> &'static [Tickbox] {
    &[
        Tickbox::Article9SpecialCategory,
        Tickbox::WhatsAppUnofficialRisk,
        Tickbox::VoiceSpeakerIdEu,
    ]
}

#[test]
fn bundled_wording_matches_pinned_hashes() {
    let pinned = parse_pins();
    let mut drift = Vec::new();
    let mut missing_in_json = Vec::new();

    for tb in known_tickboxes() {
        let id = tb.id();
        let bundled = tb.bundled_wording_hash();
        match pinned.get(id) {
            None => missing_in_json.push((id.to_string(), bundled)),
            Some(pin) if pin != &bundled => drift.push((id.to_string(), pin.clone(), bundled)),
            Some(_) => {}
        }
    }

    let dangling_in_json: Vec<String> = pinned
        .keys()
        .filter(|k| !known_tickboxes().iter().any(|tb| tb.id() == *k))
        .cloned()
        .collect();

    if drift.is_empty() && missing_in_json.is_empty() && dangling_in_json.is_empty() {
        return;
    }

    let mut msg = String::from("consent-wording drift detected\n");
    if !drift.is_empty() {
        msg.push_str("  Wording drift (txt edited; hash differs from pinned):\n");
        for (id, pin, actual) in &drift {
            msg.push_str(&format!(
                "    - {id}\n        pinned:  {pin}\n        bundled: {actual}\n"
            ));
        }
    }
    if !missing_in_json.is_empty() {
        msg.push_str("  Tickbox variants compiled in but not pinned in JSON:\n");
        for (id, actual) in &missing_in_json {
            msg.push_str(&format!("    - {id}: {actual}\n"));
        }
    }
    if !dangling_in_json.is_empty() {
        msg.push_str(
            "  Pinned tickbox ids with no matching Tickbox variant \
             (variant removed/renamed):\n",
        );
        for id in &dangling_in_json {
            msg.push_str(&format!("    - {id}\n"));
        }
    }
    msg.push_str(
        "\n  If wording was bumped intentionally:\n\
         \x20   1. regenerate `crates/ostler-consent-gate/src/wording_data/*.txt`\n\
         \x20      from HR015's `legal/consent_strings.py` (recipe in wording.rs);\n\
         \x20   2. bump `tests/consent_wording_hashes.json` HERE and in HR015\n\
         \x20      (the two files are designed to be byte-identical);\n\
         \x20   3. run `make check-consent-wording` in HR015 to confirm sync.\n",
    );
    panic!("{msg}");
}

#[test]
fn pinned_json_matches_hr015_layout() {
    // Defensive: the JSON shape is `{tickbox_id: sha256_hex}`; if a
    // future edit accidentally turns it into `{"hashes": {...}}` or
    // similar, the parse above still succeeds (HashMap ignores extra
    // keys via the dummy match), but downstream consumers (HR015
    // `check_wording.py`, future pre-commit sync scripts) expect the
    // flat layout. This test pins the shape.
    let parsed: serde_json::Value = serde_json::from_str(PINNED_JSON).unwrap();
    let obj = parsed
        .as_object()
        .expect("hashes file must be a JSON object at the top level");
    for (k, v) in obj {
        let s = v
            .as_str()
            .unwrap_or_else(|| panic!("value for {k:?} must be a JSON string"));
        assert_eq!(
            s.len(),
            64,
            "value for {k:?} must be a 64-char SHA-256 hex string, got {} chars",
            s.len(),
        );
        assert!(
            s.chars().all(|c| c.is_ascii_hexdigit()),
            "value for {k:?} must be lowercase hex, got {s:?}",
        );
    }
}
