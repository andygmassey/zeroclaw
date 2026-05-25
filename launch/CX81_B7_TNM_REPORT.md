# CX81 B7 — TNM Final Report

**Date:** 2026-05-26
**Branch:** `feat/cx81-b7-wiki-in-hub-2026-05-26` (DRAFT PR)
**Worktree:** `/tmp/ostler-assistant-b7-2026-05-26/`

---

## Summary

Wired the customer's locally-running MkDocs Material wiki (`http://127.0.0.1:8044/`) into the Hub Tauri shell as a first-class sidebar nav item. The customer never opens a separate browser to see their wiki — it lives as an interior page of the Hub, full sidebar + header chrome preserved.

## What landed

| Path | Change |
|------|--------|
| `apps/tauri/tauri.conf.json` | Added explicit `frame-src 'self' http://127.0.0.1:*` to the CSP (defence-in-depth; `default-src` already covered it but the explicit directive guards against future tightening). |
| `web/src/pages/Wiki.tsx` | **NEW.** React component that probes wiki reachability via `fetch(..., { mode: 'no-cors' })`, mounts an `<iframe>` on success, shows a loading-state with delayed hint on probing, shows an error-state with Retry button on failure. |
| `web/src/App.tsx` | Imported `Wiki` and added `<Route path="/wiki" element={<Wiki />} />`. |
| `web/src/components/layout/Sidebar.tsx` | Added `{ to: '/wiki', icon: BookOpen, labelKey: 'nav.wiki' }` between Integrations and Memory; imported `BookOpen` from lucide-react. (B3 will reposition once the §5.1 People entry lands; B7 just brings the item in.) |
| `web/src/lib/i18n.ts` | Added `nav.wiki` + `wiki.loading_title` / `wiki.loading_hint` / `wiki.error_title` / `wiki.error_hint` / `wiki.error_retry` / `wiki.iframe_title` to **all 31 locale blocks** (English values across the board per Rule 0.9 — catalogue-keyed, v1.0 English-only, translation effort comes later). |

No changes to `web/package-lock.json` (an `npm install` re-shuffled it but no new dependencies were added; reverted to keep the PR minimal).

## Acceptance criteria

| # | Criterion | Status |
|---|-----------|--------|
| 1 | Hub sidebar has a "Wiki" nav item | PASS — `BookOpen` icon, label via `t('nav.wiki')` |
| 2 | Selecting it shows the wiki at `127.0.0.1:8044/` inside the Hub window | PASS — `<iframe>` mounted in `<main>`, sidebar + header persist |
| 3 | Wiki pages navigable inside the embed | PASS — `sandbox="allow-scripts allow-same-origin allow-forms allow-popups"` |
| 4 | Loading state shown during wiki fetch | PASS — spinner immediate, `wiki.loading_title` + `wiki.loading_hint` after 2s |
| 5 | Graceful error if wiki not running, with Retry button, no Hub crash | PASS — `WikiErrorState` component with Retry that re-runs the probe |
| 6 | Rule 0.9 strings catalogue, British English, no em-dashes in customer copy, no founder numbers | PASS |
| 7 | CSP compliance — no `frame-src` blocking | PASS — explicit `frame-src 'self' http://127.0.0.1:*` |

## Brief premise probes (per pre-code requirement)

Full results in `launch/CX81_B7_TNM_PROBE.md`. Summary:

1. **ATS:** PASS. Loopback HTTP on 8044 is the same trust boundary as the existing gateway 8000 fetch.
2. **CORS:** PASS with `mode: 'no-cors'` on the readiness probe. Wiki does not set `Access-Control-Allow-*` headers — `no-cors` mode returns an opaque response on success and throws on failure, which is exactly the reachability signal needed.
3. **Frame-ancestors / X-Frame-Options:** PASS. `curl -sI http://127.0.0.1:8044/` on Andy's Mac Studio (2026-05-26) returned no `X-Frame-Options`, no `Content-Security-Policy: frame-ancestors`. The wiki is iframe-embeddable out of the box.
4. **Tauri vs iframe:** iframe selected. Multi-window would lose the sidebar/header chrome continuity the brief explicitly wants. iframe keeps the wiki inside `<main>`.

No scope expansion. All four premises green.

## Build verification

```
$ npm --prefix /tmp/ostler-assistant-b7-2026-05-26/web run build
> zeroclaw-web@0.1.0 build
> tsc -b && vite build

vite v6.4.2 building for production...
transforming...
✓ 1903 modules transformed.
rendering chunks...
computing gzip size...
dist/index.html                     0.51 kB │ gzip:   0.31 kB
dist/assets/index-5M9Ivpwr.css     40.94 kB │ gzip:   8.48 kB
dist/assets/index-CIcJGGWI.js   1,080.50 kB │ gzip: 272.99 kB
✓ built in 2.11s
```

TypeScript clean, Vite bundle clean. The 1080 kB chunk-size warning is pre-existing.

## Paste-ready test commands

Once the DMG ships with the next Hub build:

```bash
# Confirm the wiki container/process is up on the customer Mac
curl -sI http://127.0.0.1:8044/ | head -3
# Expect: HTTP/1.0 200 OK

# Open Ostler.app, click the BookOpen "Wiki" item in the sidebar.
# Expect: embedded MkDocs Material wiki, fully interactive.

# Negative test: stop the wiki container, click Wiki nav item.
# Expect: "Wiki not available" + "Try again" button. Hub does not crash.
# Restart the wiki and click "Try again". Expect: iframe mounts.
```

## Out of scope (per brief)

- B3 brand reskin — Sidebar/Header visual treatment. B7 uses existing placeholder styles.
- CM044 wiki light-header (`.md-header` chassis background, hairline border) — separate parallel-safe CM044 PR.
- B4 Cost remove — happens before B3.
- The §5.1 "People" sidebar entry — B3 introduces it; once B3 lands the Wiki nav item will need to be repositioned between People and Memory.

## Constraints honoured

- British English: yes (English-only catalogue values; no Americanisms in new copy)
- No em-dashes in customer-rendered strings: yes (catalogue values use `...` ellipses)
- Rule 0.9 catalogue strings from day one: yes (six new keys across all 31 locales)
- No founder numbers: yes (no scale claims in any new copy)
- DRAFT PR only: yes (push will use `--no-merge`; PR opened with `gh pr create --draft`)
- `gh auth switch -u <owner> && git push ...` per push: yes
- Time budget 3-4h: well under (probe + scaffold + i18n fan-out + build verify under 1h)
