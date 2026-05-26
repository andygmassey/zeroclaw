//! Tray icon management.
//!
//! v1.0 uses the canonical Ostler menu bar mark (Variant A v2) for every
//! state. macOS auto-tints the icon via the template-image flag (set on the
//! tray builder), so light + dark menu bar themes render correctly without
//! shipping per-theme assets. State context (Idle / Working / Error /
//! Disconnected) is conveyed via the tooltip + the status line in the tray
//! menu, not via icon variation.
//!
//! The legacy four-state PNGs (`tray-idle.png`, `tray-working.png`,
//! `tray-error.png`, `tray-disconnected.png`) remain on disk for now to
//! keep this PR scoped to a pure icon swap; they are no longer referenced
//! from code and can be removed in a follow-up cleanup.

use crate::state::AgentStatus;
use tauri::image::Image;

/// Embedded Ostler menu bar mark (Variant A v2, 22x22 RGBA, black on
/// transparent). Rendered as a macOS template image so the system tints it
/// for light/dark menu bar themes automatically.
const ICON_VARIANT_A_V2: &[u8] = include_bytes!("../../icons/menubar-variant-a-v2-22.png");

/// Resolve the tray icon for the current state.
///
/// All states currently return the canonical Variant A v2 mark; state
/// information lives in the tooltip and tray menu instead. The signature is
/// preserved so callers (e.g. the health poller) do not need to change.
pub fn icon_for_state(_connected: bool, _status: AgentStatus) -> Image<'static> {
    Image::from_bytes(ICON_VARIANT_A_V2).expect("embedded tray icon is a valid PNG")
}

/// Tooltip text for the current state.
pub fn tooltip_for_state(connected: bool, status: AgentStatus) -> &'static str {
    if !connected {
        return "Ostler - Disconnected";
    }
    match status {
        AgentStatus::Idle => "Ostler - Idle",
        AgentStatus::Working => "Ostler - Working",
        AgentStatus::Error => "Ostler - Error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_disconnected_returns_canonical_mark() {
        // Should not panic; icon bytes are a valid PNG.
        let _img = icon_for_state(false, AgentStatus::Idle);
        let _img = icon_for_state(false, AgentStatus::Working);
        let _img = icon_for_state(false, AgentStatus::Error);
    }

    #[test]
    fn icon_connected_variants_return_canonical_mark() {
        let _idle = icon_for_state(true, AgentStatus::Idle);
        let _working = icon_for_state(true, AgentStatus::Working);
        let _error = icon_for_state(true, AgentStatus::Error);
    }

    #[test]
    fn tooltip_disconnected() {
        assert_eq!(
            tooltip_for_state(false, AgentStatus::Idle),
            "Ostler - Disconnected"
        );
        // Agent status is irrelevant when disconnected.
        assert_eq!(
            tooltip_for_state(false, AgentStatus::Working),
            "Ostler - Disconnected"
        );
        assert_eq!(
            tooltip_for_state(false, AgentStatus::Error),
            "Ostler - Disconnected"
        );
    }

    #[test]
    fn tooltip_connected_variants() {
        assert_eq!(tooltip_for_state(true, AgentStatus::Idle), "Ostler - Idle");
        assert_eq!(
            tooltip_for_state(true, AgentStatus::Working),
            "Ostler - Working"
        );
        assert_eq!(
            tooltip_for_state(true, AgentStatus::Error),
            "Ostler - Error"
        );
    }

    #[test]
    fn embedded_icon_is_valid_png() {
        // PNG file signature: first 8 bytes.
        let png_sig: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        assert!(
            ICON_VARIANT_A_V2.starts_with(png_sig),
            "Variant A v2 icon not valid PNG"
        );
    }

    #[test]
    fn embedded_icon_is_22x22() {
        // Verify the PNG IHDR width/height. IHDR sits at bytes 8..29 of a
        // PNG; width is big-endian u32 at bytes 16..20, height at 20..24.
        let w = u32::from_be_bytes([
            ICON_VARIANT_A_V2[16],
            ICON_VARIANT_A_V2[17],
            ICON_VARIANT_A_V2[18],
            ICON_VARIANT_A_V2[19],
        ]);
        let h = u32::from_be_bytes([
            ICON_VARIANT_A_V2[20],
            ICON_VARIANT_A_V2[21],
            ICON_VARIANT_A_V2[22],
            ICON_VARIANT_A_V2[23],
        ]);
        assert_eq!(w, 22, "Variant A v2 icon width must be 22 for macOS tray");
        assert_eq!(h, 22, "Variant A v2 icon height must be 22 for macOS tray");
    }
}
