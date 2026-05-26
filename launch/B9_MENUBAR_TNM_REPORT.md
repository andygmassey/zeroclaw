# B9 menu bar Variant A v2 -- TNM report

Andy's pick: **Variant A v2** (heavier outer ring, larger inner O).

Locked asset source: `/private/tmp/cm051-cx25-brew-tarball-2026-05-24/launch/menubar/variant_a.svg` (24-unit viewBox, `r=9.5`, `stroke-width=3.5`, black on transparent).

## Premise probe (completed)

| premise | finding |
|---|---|
| Which target owns the macOS menu bar item? | `ostler-assistant/apps/tauri/` (the rebranded ZeroClaw Tauri app -- ships as `/Applications/Ostler.app`, install.sh §5142 confirms) |
| Does Tauri config already declare a system tray? | Yes -- created in code via `TrayIconBuilder` in `src/tray/mod.rs`, no `tauri.conf.json` tray block needed |
| Existing tray icons? | 4 state PNGs (`tray-idle.png` / `tray-working.png` / `tray-error.png` / `tray-disconnected.png`), 22x22 RGBA -- being replaced by the canonical Variant A v2 mark |
| Tauri version supports `icon_as_template`? | Yes -- Tauri 2.0, `TrayIconBuilder::icon_as_template(bool)` is the standard macOS template image switch |
| `/api/v1/recording/active` endpoint exists? | **No** -- task #404 not yet landed. "Show Recording State" menu item skipped per anti-scope "DO NOT add ABI surfaces"; can be added in follow-up once #404 ships |
| Customer-facing tooltips already use Ostler branding? | Yes -- "Ostler - Disconnected / Idle / Working / Error" (post-rebrand, commits `333b5764` + `8bd4123a`) |
| Menu already has "Quit Ostler"? | Yes -- `src/tray/menu.rs` line 16 |

No premise wobble. No `B9_MENUBAR_PROBE_FOLLOWUP.md` needed.

## File list

### Assets added (`apps/tauri/icons/`)

- `menubar-variant-a-v2.svg` -- canonical Variant A v2 vector source (24-unit viewBox, r=9.5, stroke-width=3.5, black on transparent)
- `menubar-variant-a-v2-22.png` -- 22x22 RGBA, the size loaded into the macOS tray at runtime
- `menubar-variant-a-v2-44.png` -- 44x44 RGBA, retina (2x) sibling
- `menubar-variant-a-v2-18.png` -- 18x18 RGBA (vendored from canonical render dir for completeness)
- `menubar-variant-a-v2-36.png` -- 36x36 RGBA (vendored from canonical render dir for completeness)
- `menubar-variant-a-v2-72.png` -- 72x72 RGBA (vendored from canonical render dir for completeness)

The 22-px PNG is the one actually `include_bytes!()`'d at compile time; the others are committed for future-proofing (e.g. retina handoff, dock badge experiments) and so the canonical render set lives next to the SVG source.

### Code (`apps/tauri/src/tray/`)

- `mod.rs` -- added `.icon_as_template(true)` to the `TrayIconBuilder` chain so macOS auto-tints for light + dark menu bar themes
- `icon.rs` -- swapped the four state-specific PNG constants for a single canonical `ICON_VARIANT_A_V2` constant; `icon_for_state()` signature preserved (still called by the health poller in `health.rs`), state info now lives only in the tooltip + menu status line per macOS HIG. Added `embedded_icon_is_22x22` test that decodes the PNG IHDR to assert the dimensions never regress

### Legacy assets (not touched in this PR)

`tray-idle.png`, `tray-working.png`, `tray-error.png`, `tray-disconnected.png` remain on disk but are no longer referenced from code. Held back from deletion to keep this PR pure-swap; a follow-up cleanup PR can drop them.

## Build + test

```
cargo check -p zeroclaw-desktop  -> Finished
cargo test  -p zeroclaw-desktop --lib tray::icon  -> ok. 6 passed; 0 failed
```

The 9 `gateway_client` test failures observed in a wider `cargo test --lib` run reproduce on `main` (rustls "No provider set" in test env) -- unrelated to this PR.

## Acceptance checklist

- [x] 1 DRAFT PR (ostler-assistant)
- [x] Variant A v2 SVG + PNGs vendored in the repo (under `apps/tauri/icons/`)
- [x] Tauri tray configured to use Variant A v2 with `icon_as_template(true)` for macOS auto-tint
- [x] Light + dark menu bar preview (`launch/menubar-light-dark-preview.png`)
- [x] Tooltip wired (already "Ostler - <state>" from prior rebrand commit; preserved)
- [x] Menu wired (already "Show Dashboard / Agent Chat / Status / Quit Ostler"; preserved)
- [x] `launch/B9_MENUBAR_TNM_REPORT.md` (this file)

## Anti-scope compliance

- No new Tauri commands.
- No new HTTP endpoints; "Show Recording State" deferred until `/api/v1/recording/active` (task #404) lands.
- No iOS touched.
- No V1.1 brand reskin touched.
- Only Variant A v2 bundled (other variants in `/private/tmp/cm051-cx25-brew-tarball-2026-05-24/launch/menubar/` left out per "A v2 only").
- Dock icon (`icon.icns` / `128x128.png`) not touched -- this PR only swaps the NSStatusBar tray item.

## Visual confirmation

`launch/menubar-light-dark-preview.png` shows the icon rendering crisp on both light (#F6F6F6) and dark (#2C2C2C) menu bar backgrounds, simulating macOS template-image auto-tint. Ring is solid at the perimeter, hollow at the centre, corners fully transparent (verified via PNG alpha sampling at corners + centre).

For an in-system screenshot Andy can grab on first launch of the next DMG, the icon will appear top-right of the macOS menu bar between any other tray items (Spotlight / Control Centre / clock); left-click opens the Ostler dashboard, right-click opens the existing tray menu.
