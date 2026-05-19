# iOS Companion ↔ Hub QR pairing — implementation tracker

**Status:** v0.3.1 (foundation PR landed 2026-05-09; full handshake landed in PR #40 on 2026-05-19; wire-shape contract tests added 2026-05-20).
**Owner:** ostler-ai/ostler-assistant maintainers.
**Spec:** `SHARED_AUTH_SPEC.md` (HR015 repo, frozen 2026-04-23, three-party signoff).
**Audit:** `/tmp/QR_PAIRING_AUDIT_2026-05-09.md` — full forensic at branch
freeze; copy in HR015.

## Status update 2026-05-20 (post-F12 trio + Windows-compat)

The earlier "Hub gap" table below predates the F12 trio that landed tonight (2026-05-19 / 2026-05-20):

- HR015 PR #119 (merged) — SHARED_AUTH_SPEC.md §3.3 patch with `prf_output` on the wire + `~/.lifeline/security/` → `~/.ostler/security/` path rebrand.
- ostler-assistant PR #40 (merged at `de725fa3`) — full `handle_pair_register` Step C: `pairing_token` validation, `prf_output` decode, primary-KEK re-derive via HKDF-SHA256, `companion_proof` HMAC verification, DEK generate-or-reload, AES-KW wrap to 40-byte output, `hub_proof` HMAC, atomic mode-0600 persistence at `${OSTLER_DIR}/security/passkey.json`. Routes wired in `crates/zeroclaw-gateway/src/lib.rs:1108-1111`.
- CM031 PR #91 (merged) — Swift `prfOutput: Data` field on `PairRegisterRequest` + encoder + `runPairRegister` plumbing, so iOS sends the wire shape `api_auth_pair::PairRegisterRequest` expects.
- ostler-assistant PR #41 (merged at `373b07d5`) — added `webauthn` to default features so release builds resolve `pair_handshake`.
- ostler-assistant PR #42 (merged at `afd60ac9`) — gated `OpenOptionsExt::mode(0o600)` with `#[cfg(unix)]` so `persist_passkey_file` builds on the `x86_64-pc-windows-msvc` matrix.
- ostler-assistant PR #26 (merged at `6693c824`) — end-to-end integration tests for the Step C crypto round-trip + tamper detection.

CM031's strict-decoder key sets (`HubPairingAPI.swift:127-130` for init, `HubPairingAPI.swift:152-154` for register) and Hub's serde structs (`PairInitResponse`, `PairRegisterResponse`) align byte-for-byte. The cross-repo audit's WSA-001 + WSA-002 findings were generated against pre-F12-trio state and are no longer accurate; they should be marked RESOLVED.

Wire-shape contract tests now in `crates/zeroclaw-gateway/src/api_auth_pair.rs` `mod tests`:
- `pair_init_response_emits_exact_ios_expected_keys`
- `pair_register_response_emits_exact_ios_expected_keys`
- `pair_init_response_field_types_match_ios_decoder`
- `pair_register_response_field_types_match_ios_decoder`

These pin the serialised shape against the iOS strict-decoder expected key set + base64url byte counts. A future serde rename or response-field drift fails CI here rather than at first customer install.

## Why this exists

The iOS Companion app (CM031) was built to the SHARED_AUTH_SPEC pairing
protocol. The Hub side was never implemented; it still runs the pre-spec
6-digit terminal-code pairing. The 16-day divergence wasn't caught
because there is no end-to-end pairing test in CI.

Customer-visible impact: iOS Companion cannot pair with the Hub. iOS POSTs
to `/auth/pair/init` and gets HTTP 404.

This tracker captures the protocol divergence, the remediation plan, and
the PR sequence.

## Spec summary (the contract iOS expects)

QR payload (single JSON object, UTF-8):

```json
{
  "v": 1,
  "hub_addr": "host:port",
  "rp_id": "creativemachines.ai",
  "pairing_token": "<base64url 32B>",
  "expires_at": <unix_seconds>
}
```

Handshake:

1. iOS scans QR.
2. `POST /auth/pair/init` with `{pairing_token, device_label}` →
   `{registration_challenge, rp_id, rp_name, user_handle, ...}`.
3. iOS performs WebAuthn passkey registration (PRF extension, `rp_id =
   creativemachines.ai`).
4. `POST /auth/pair/register` with `{attestation, companion_proof}` →
   `{wrapped_dek (40B AES-KW RFC 3394), thread_id, hub_proof}`.
5. iOS persists wrapped DEK in Keychain; Hub persists wrapped DEK +
   credential_id in `~/.lifeline/security/passkey.json`.

Crypto invariants (§1 of spec):

- HKDF-SHA256 throughout, salt `"creativemachines/auth/v1"` (24 ASCII bytes).
- Primary KEK `info`: `"creativemachines/kek/primary/v1/{thread_id}"`.
- PRF input: `"creativemachines/prf/v1"` (23 ASCII bytes).
- AES-KW wrap: 32-byte DEK + 32-byte KEK → 40-byte wrapped output, no IV
  field (default RFC 3394 IV `A6A6A6A6A6A6A6A6`).
- `companion_proof` = HMAC-SHA256(KEK, `"creativemachines/pair/proof/v1"`).
- `hub_proof` = HMAC-SHA256(KEK, `"creativemachines/pair/proof/v1/hub"`).

## Hub gap as of 2026-05-09 (audit findings — SUPERSEDED, see status update above)

| Spec requirement                              | Hub source | v0.2.0 binary |
|-----------------------------------------------|------------|---------------|
| `POST /auth/pair/init`                        | present (lib.rs:1108, api_auth_pair.rs:185) | v0.3.1 |
| `POST /auth/pair/register`                    | present (lib.rs:1110, api_auth_pair.rs:236) | v0.3.1 |
| `pairing_token` (32B base64url, 10-min)       | absent (uses 6-digit code) | absent |
| WebAuthn registration verification            | code exists, feature off in release CI | absent |
| PRF extension                                 | absent     | absent        |
| AES-KW DEK wrapping (40B)                     | absent     | absent        |
| `companion_proof` HMAC-SHA256                 | absent     | absent        |
| `hub_proof` return                            | absent     | absent        |
| QR emission for iOS pairing                   | absent     | absent        |
| `rp_id = creativemachines.ai`                 | default `"localhost"` | absent |
| `hub_addr` auto-discovery                     | absent (manual config only) | n/a |

## PR sequence

### PR 1 — Foundation (this PR, 2026-05-09)
- Enable `webauthn` feature in `.github/workflows/release-stable-manual.yml`
  release matrix (and Android override).
- Verify the existing `zeroclaw-runtime` webauthn module compiles cleanly
  under the flag (it does; `cargo check -p zeroclaw-gateway --features
  webauthn` passes locally on Rust 1.95.0).
- Add this tracking document.
- **No new endpoints, no behavioural change to running Hubs.** Pairing
  still uses 6-digit terminal codes.

### PR 2 — Route surface (next)
- Add stub `POST /auth/pair/init` and `POST /auth/pair/register`
  handlers in `crates/zeroclaw-gateway/`. Handlers accept the spec'd
  request shape and return a structured 501 Not Implemented response so
  iOS hits a typed error instead of a 404.
- Wire to the axum route table at `lib.rs:948-1003`.

### PR 3 — QR emission
- Emit a JSON QR payload per `SHARED_AUTH_SPEC.md` §3.2 alongside (not
  replacing) the existing 6-digit code. The 6-digit flow stays as a
  fallback for terminal-only operators and for the existing web
  dashboard click-to-pair UX.
- Reuse the existing `qrcode` crate (currently feature-gated behind
  `whatsapp-web`; will need to be made available to the gateway).
- Wire `pairing_token` (32 random bytes, base64url-unpadded, 10-min
  TTL, single-use) generator into `crates/zeroclaw-config/src/pairing.rs`.

### PR 4 — Real handshake (the security work)
- Implement `/auth/pair/init`: validate `pairing_token`, generate
  WebAuthn registration challenge with PRF extension, return `rp_id`
  + challenge.
- Implement `/auth/pair/register`: verify WebAuthn attestation, extract
  PRF output, derive primary KEK via HKDF-SHA256, wrap sync DEK with
  AES-KW (RFC 3394), generate `hub_proof`, return `{wrapped_dek,
  hub_proof, expires_at}`.
- Update `default_webauthn_rp_id` in `crates/zeroclaw-config/src/schema.rs`
  to read from config so v1 can stay `"localhost"` during testing while
  production wires `creativemachines.ai`.
- **Open implementation question:** the existing webauthn module in
  `zeroclaw-runtime/src/security/webauthn.rs` is a hand-rolled impl
  using `ring` for signature verification. The brief that scoped this
  work assumed `webauthn-rs` would be available — it isn't a workspace
  dep. Two paths:
  1. Extend the hand-rolled impl with PRF support. Pros: no new dep,
     consistent with existing code. Cons: hand-rolled WebAuthn
     attestation parsing is security-critical and the existing impl is
     ~1,400 LOC; PRF + DEK wrapping pushes it further.
  2. Add `webauthn-rs` (battle-tested crate) for the pairing flow,
     keep the hand-rolled impl for the existing `/api/webauthn/*`
     routes. Pros: less attestation-parsing surface to audit. Cons:
     adds ~50 transitive deps; two implementations of the same protocol
     in one binary.
  Recommend (2) on security grounds, subject to Lester's view at the
  Mon 2026-05-11 10am HKT call.
- `aes-kw` crate from RustCrypto (`aes-kw = "0.2"`) — verify
  toolchain compatibility.
- `hmac` and `sha2` are already deps.

### PR 5 — Integration test (non-negotiable)
- CI test that boots both sides (Hub binary + iOS protocol mock)
  through the full handshake: emit QR → scan → init → WebAuthn ceremony
  → register → verify wrapped_dek round-trips correctly.
- This is the missing test that let the protocol divergence persist
  16 days. **Must merge before any production claim that pairing
  works.**

## Hard rules across this PR set

- No PR in this series merges without Andy + Lester review.
- WebAuthn changes are security-critical. Use battle-tested crates;
  do not roll new attestation parsing.
- All key material lives in memory only during pairing, then in
  Keychain (iOS) / encrypted SecretStore (Hub) — never plaintext on
  disk.
- AES-KW: use a battle-tested crate (RustCrypto `aes-kw`). RFC 3394
  test vector from spec §2.4 must pass on both sides.
- HMAC-SHA256 for proofs uses the existing `hmac` crate.

## Stop conditions surfaced in PR 1 (for reference)

- The hand-rolled WebAuthn impl is ~1,400 LOC and predates the
  PRF-extension requirement. Adding PRF support without breaking
  existing `/api/webauthn/*` consumers is non-trivial; the right call
  may be to introduce `webauthn-rs` for the new flow only.
- The 6-digit code path is load-bearing for the existing web-dashboard
  click-to-pair flow (`web/src/App.tsx:91-188`). PR 3 keeps it; do
  not propose deleting it.
- `webauthn-rs` (or any equivalent) compatibility with workspace Rust
  version (1.93) needs verification at PR 4.
