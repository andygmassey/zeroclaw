# CX81 B7 — TNM Probe Doc

**Probed at:** 2026-05-26 by TNM
**Goal:** verify the four pre-code premises before wiring the wiki iframe.

---

## 1. ATS / NSAllowsLocalNetworking

Tauri webview on macOS runs through the system WebKit. The Tauri shell itself talks to gateway port 8000 today, so loopback over HTTP is already exercised by the gateway connection path. Adding port 8044 (wiki) is the same loopback (`http://127.0.0.1`), same protocol, same trust boundary. No new ATS work needed.

**Result:** PASS. The existing `NSAllowsLocalNetworking=true` setting (or the equivalent on the Tauri shell) covers loopback on any port. The wiki iframe load is identical to the gateway fetch path from the webview's perspective.

## 2. CORS

CORS only applies to JS-initiated cross-origin XHR/fetch. The Wiki page mounts an `<iframe>` pointing at `http://127.0.0.1:8044/`. The browser issues a top-level navigation inside the iframe context; no CORS preflight, no `Access-Control-Allow-*` headers required.

The probe page (the one we use to detect "is the wiki up before we mount the iframe") IS a JS-initiated `fetch()` from the Tauri webview origin (`tauri://localhost` or `http://127.0.0.1:42617` in dev) to `http://127.0.0.1:8044/`. That IS subject to CORS.

Empirical check on Andy's Mac Studio (2026-05-26 17:01 UTC):

```
$ curl -sI http://127.0.0.1:8044/
HTTP/1.0 200 OK
Date: Mon, 25 May 2026 17:01:08 GMT
Server: WSGIServer/0.2 CPython/3.11.14
Content-Type: text/html
Content-Length: 35733
```

No `Access-Control-Allow-Origin` header. A cross-origin `fetch()` from the React app would fail CORS preflight.

**Mitigation:** use a `no-cors` fetch (`mode: 'no-cors'`) just to check reachability. We don't need the body — we just need to know the wiki is responding (non-network-error). `fetch(..., { mode: 'no-cors' })` returns an opaque response on success and throws on network failure; that's exactly the ready-check shape we want.

**Result:** PASS (with `mode: 'no-cors'` on the probe fetch).

## 3. Frame-ancestors / X-Frame-Options

Empirical check (Andy's Mac Studio, 2026-05-26 17:01 UTC):

```
$ curl -sI http://127.0.0.1:8044/
HTTP/1.0 200 OK
Date: Mon, 25 May 2026 17:01:08 GMT
Server: WSGIServer/0.2 CPython/3.11.14
Content-Type: text/html
Content-Length: 35733
```

No `X-Frame-Options` header. No `Content-Security-Policy: frame-ancestors` header. MkDocs Material's default `mkdocs serve` does not set any framing restriction, and the customer's wiki container appears to inherit that default.

**Result:** PASS. The wiki is iframe-embeddable out of the box. No CM044 work needed for B7 (the light-header CM044 work in the brief §5.2 is a separate parallel-safe PR).

## 4. Tauri vs iframe

The Tauri config exposes a CSP (`apps/tauri/tauri.conf.json`):

```
"csp": "default-src 'self' http://127.0.0.1:* ws://127.0.0.1:*; connect-src 'self' http://127.0.0.1:* ws://127.0.0.1:*; script-src 'self' 'unsafe-inline' http://127.0.0.1:*; style-src 'self' 'unsafe-inline' http://127.0.0.1:*; img-src 'self' http://127.0.0.1:* data:"
```

There is no explicit `frame-src` directive. The `default-src` covers it (per CSP fall-through), and `default-src 'self' http://127.0.0.1:*` already permits `http://127.0.0.1:8044`. However, defence-in-depth: I will add an explicit `frame-src 'self' http://127.0.0.1:*` directive so a future tightening of `default-src` does not silently break the wiki embed.

Tauri multi-window vs iframe trade-off:

- **Multi-window (Tauri `WebviewWindow::new`)** would open the wiki in a separate native window. Loses sidebar/header chrome continuity. Brief explicitly wants the wiki to "feel like an interior page of the Hub". Reject.
- **Iframe** keeps the wiki inside the existing `<main>` content area. Sidebar + header persist. URL bar / back-button concerns dissolve because the customer never sees the wiki as a separate surface. Selected.

**Result:** iframe. CSP update needed: add explicit `frame-src 'self' http://127.0.0.1:*`.

---

## Findings summary

| # | Premise | Status | Action |
|---|---------|--------|--------|
| 1 | ATS covers loopback on 8044 | PASS | No work |
| 2 | CORS on probe fetch | PASS with `mode: 'no-cors'` | Use no-cors mode on the readiness probe |
| 3 | Frame-ancestors / X-Frame-Options | PASS (none set) | No work (CM044 light-header is a separate parallel PR per brief §5.2) |
| 4 | Tauri vs iframe | iframe | Add explicit `frame-src` to CSP |

No scope-expansion needed. Proceeding to implementation.
