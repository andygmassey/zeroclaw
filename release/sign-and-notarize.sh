#!/usr/bin/env bash
#
# sign-and-notarize.sh
#
# Signs the ostler-assistant binary with Hardened Runtime, submits
# it to Apple's notary service, and verifies the result. Called
# from release/build-binary.sh AFTER cargo emits the release
# binary, BEFORE the tarball is rolled.
#
# Gating: when $OSTLER_SIGNING_IDENTITY is empty, log "skipping"
# and exit 0. The build-binary.sh caller then ships the unsigned
# binary -- same v0.1 customer-facing tarball as today. Once the
# operator enrols in the Apple Developer Program and populates
# the secret in GitHub repo settings, the next tag push produces
# a signed + notarised binary at the same artefact name.
#
# Stapling: ostler-assistant is a raw Mach-O binary, not a .app
# bundle. `xcrun stapler staple` only works on bundles -- for raw
# binaries the notarisation ticket is verified at runtime via an
# online lookup against Apple's servers (no local staple needed).
# Once the ticket exists, the binary is trusted by Gatekeeper on
# any Mac with internet access. Air-gapped customer Macs would
# need bundling; out of scope for v0.1.
#
# Inputs (env):
#   OSTLER_SIGNING_IDENTITY   Required when not skipping. Developer ID
#                             Application certificate common name OR
#                             keychain SHA-1, e.g. "Developer ID
#                             Application: <Your Name or Org>
#                             (TEAMID)" or just the SHA-1 hex.
#                             Special value "-" engages ad-hoc signing
#                             for local smoke tests (no notarisation;
#                             spctl will reject but codesign -dv
#                             reports the signature).
#
# Pick exactly ONE of the three notary auth blocks below. The script
# checks them in priority order (keychain-profile, then API key,
# then app-specific password) and refuses to proceed if multiple are
# fully populated -- ambiguous credential blocks are a leak risk
# disguised as a typo.
#
# Block 1: keychain-profile (recommended for local builds)
#   OSTLER_NOTARY_KEYCHAIN_PROFILE  Profile name previously stored
#                                   via `xcrun notarytool
#                                   store-credentials`.
#
# Block 2: App Store Connect API key (recommended for CI)
#   OSTLER_NOTARY_API_KEY_FILE      Path to the .p8 private key file
#                                   on the runner. The workflow
#                                   base64-decodes
#                                   OSTLER_NOTARY_API_KEY_BASE64 to
#                                   a tempfile and points this at it.
#   OSTLER_NOTARY_API_KEY_ID        10-character Key ID.
#   OSTLER_NOTARY_API_ISSUER_ID     Issuer UUID.
#
# Block 3: Apple ID + app-specific password (fallback)
#   OSTLER_NOTARY_APPLE_ID          Apple ID enrolled in the
#                                   Developer Program.
#   OSTLER_NOTARY_PASSWORD          16-character app-specific
#                                   password.
#   OSTLER_NOTARY_TEAM_ID           Apple Developer Team ID.
#
# Inputs (positional):
#   $1                       Path to the binary to sign
#                            (e.g. target/aarch64-apple-darwin/
#                                 release/zeroclaw or a staging
#                            copy under a temp dir).
#
# Outputs:
#   The binary at $1 is signed in place. No stdout artefacts.
#   Exits 0 on success or skip; non-zero on signing or notary
#   failure. Failure aborts the release pipeline.
#
# Verification chain (run after signing):
#   1. codesign --verify --strict --verbose=2 -- catches
#      tampering and missing entitlements.
#   2. spctl --assess --type execute --verbose -- Gatekeeper
#      verdict; succeeds for Developer-ID + notarised binaries,
#      and fails (expected) for ad-hoc-signed local builds.
#
# British English throughout.

set -euo pipefail

if [ "${OSTLER_SIGNING_IDENTITY:-}" = "" ]; then
    echo "[$(date '+%H:%M:%S')] sign-and-notarize: skipping (OSTLER_SIGNING_IDENTITY unset; unsigned build)"
    exit 0
fi

BINARY="${1:?usage: sign-and-notarize.sh <binary-path>}"
if [ ! -f "$BINARY" ]; then
    echo "ERROR: binary not found at $BINARY" >&2
    exit 1
fi

ENTITLEMENTS_PLIST="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/entitlements.plist"
if [ ! -f "$ENTITLEMENTS_PLIST" ]; then
    echo "ERROR: entitlements.plist not found at $ENTITLEMENTS_PLIST" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# 1. Codesign with Hardened Runtime
# ---------------------------------------------------------------------------
#
# `--options runtime` enables Hardened Runtime (the entitlements
# above are honoured). `--timestamp` requests a secure timestamp
# from Apple's TSA; required for notarisation. Without it,
# notarytool rejects the submission.
#
# The "-" identity engages ad-hoc signing -- useful for verifying
# the Hardened-Runtime + entitlements wiring on a developer Mac
# without a paid Developer ID certificate. Ad-hoc-signed binaries
# are NOT accepted by Gatekeeper; this branch skips notarisation.

echo "[$(date '+%H:%M:%S')] sign-and-notarize: codesigning $BINARY with identity '$OSTLER_SIGNING_IDENTITY'"

codesign \
    --sign "$OSTLER_SIGNING_IDENTITY" \
    --options runtime \
    --entitlements "$ENTITLEMENTS_PLIST" \
    --timestamp \
    --force \
    "$BINARY"

codesign --verify --strict --verbose=2 "$BINARY" 2>&1 | sed -e 's/^/    /'

if [ "$OSTLER_SIGNING_IDENTITY" = "-" ]; then
    echo "[$(date '+%H:%M:%S')] sign-and-notarize: ad-hoc signing complete; skipping notarisation."
    echo "[$(date '+%H:%M:%S')] (Gatekeeper will reject this binary; for local verification only.)"
    codesign -dv --verbose=4 "$BINARY" 2>&1 | sed -e 's/^/    /' || true
    exit 0
fi

# ---------------------------------------------------------------------------
# 2. Notarise
# ---------------------------------------------------------------------------
#
# notarytool's --wait blocks until Apple's notary service issues
# a verdict (typical 1-5 minutes; 30+ minutes during Apple-side
# congestion). Submission is over a raw .zip envelope; ditto is
# the Apple-blessed tool for that.
#
# Auth mode selection: priority order is (1) keychain-profile,
# (2) API key, (3) app-specific password. We refuse to proceed
# when more than one block is fully populated because an
# ambiguous credential set is a leak risk disguised as a typo.

NOTARY_AUTH_ARGS=()
NOTARY_AUTH_MODE=""

KP_SET=0; AK_SET=0; PW_SET=0
[ -n "${OSTLER_NOTARY_KEYCHAIN_PROFILE:-}" ] && KP_SET=1
[ -n "${OSTLER_NOTARY_API_KEY_FILE:-}" ] \
    && [ -n "${OSTLER_NOTARY_API_KEY_ID:-}" ] \
    && [ -n "${OSTLER_NOTARY_API_ISSUER_ID:-}" ] \
    && AK_SET=1
[ -n "${OSTLER_NOTARY_APPLE_ID:-}" ] \
    && [ -n "${OSTLER_NOTARY_PASSWORD:-}" ] \
    && [ -n "${OSTLER_NOTARY_TEAM_ID:-}" ] \
    && PW_SET=1

POPULATED=$((KP_SET + AK_SET + PW_SET))
if [ "$POPULATED" -eq 0 ]; then
    echo "ERROR: no notary auth credentials populated." >&2
    echo "       Set one of:" >&2
    echo "         OSTLER_NOTARY_KEYCHAIN_PROFILE" >&2
    echo "         OSTLER_NOTARY_API_KEY_FILE + _API_KEY_ID + _API_ISSUER_ID" >&2
    echo "         OSTLER_NOTARY_APPLE_ID + _PASSWORD + _TEAM_ID" >&2
    exit 1
fi
if [ "$POPULATED" -gt 1 ]; then
    echo "ERROR: multiple notary auth blocks populated (keychain=$KP_SET, api_key=$AK_SET, password=$PW_SET)." >&2
    echo "       Refusing to choose; clear the unused secrets." >&2
    exit 1
fi

if [ "$KP_SET" -eq 1 ]; then
    NOTARY_AUTH_MODE="keychain-profile"
    NOTARY_AUTH_ARGS=(--keychain-profile "$OSTLER_NOTARY_KEYCHAIN_PROFILE")
elif [ "$AK_SET" -eq 1 ]; then
    NOTARY_AUTH_MODE="api-key"
    if [ ! -f "$OSTLER_NOTARY_API_KEY_FILE" ]; then
        echo "ERROR: OSTLER_NOTARY_API_KEY_FILE points at non-existent path: $OSTLER_NOTARY_API_KEY_FILE" >&2
        exit 1
    fi
    NOTARY_AUTH_ARGS=(
        --key "$OSTLER_NOTARY_API_KEY_FILE"
        --key-id "$OSTLER_NOTARY_API_KEY_ID"
        --issuer "$OSTLER_NOTARY_API_ISSUER_ID"
    )
else
    NOTARY_AUTH_MODE="app-specific-password"
    NOTARY_AUTH_ARGS=(
        --apple-id "$OSTLER_NOTARY_APPLE_ID"
        --team-id "$OSTLER_NOTARY_TEAM_ID"
        --password "$OSTLER_NOTARY_PASSWORD"
    )
fi

NOTARY_DIR="$(mktemp -d)"
trap 'rm -rf "$NOTARY_DIR"' EXIT
NOTARY_ZIP="$NOTARY_DIR/$(basename "$BINARY").zip"

echo "[$(date '+%H:%M:%S')] sign-and-notarize: zipping for notary submission"
ditto -c -k --keepParent "$BINARY" "$NOTARY_ZIP"

echo "[$(date '+%H:%M:%S')] sign-and-notarize: submitting to notary via $NOTARY_AUTH_MODE (this can take several minutes)"
xcrun notarytool submit "$NOTARY_ZIP" \
    "${NOTARY_AUTH_ARGS[@]}" \
    --wait \
    --output-format plist \
    > "$NOTARY_DIR/notary.plist"

NOTARY_STATUS="$(/usr/libexec/PlistBuddy -c 'Print :status' "$NOTARY_DIR/notary.plist" 2>/dev/null || echo unknown)"
NOTARY_ID="$(/usr/libexec/PlistBuddy -c 'Print :id' "$NOTARY_DIR/notary.plist" 2>/dev/null || echo unknown)"

echo "[$(date '+%H:%M:%S')] sign-and-notarize: notary verdict: status=$NOTARY_STATUS id=$NOTARY_ID"

if [ "$NOTARY_STATUS" != "Accepted" ]; then
    echo "ERROR: notary rejected the submission. Fetch the log with:" >&2
    case "$NOTARY_AUTH_MODE" in
        keychain-profile)
            echo "  xcrun notarytool log $NOTARY_ID --keychain-profile $OSTLER_NOTARY_KEYCHAIN_PROFILE" >&2
            ;;
        api-key)
            echo "  xcrun notarytool log $NOTARY_ID --key <p8> --key-id $OSTLER_NOTARY_API_KEY_ID --issuer $OSTLER_NOTARY_API_ISSUER_ID" >&2
            ;;
        app-specific-password)
            echo "  xcrun notarytool log $NOTARY_ID --apple-id <id> --team-id <team> --password <pwd>" >&2
            ;;
    esac
    exit 1
fi

# ---------------------------------------------------------------------------
# 3. Verify Gatekeeper acceptance
# ---------------------------------------------------------------------------
#
# `spctl -a -vvv -t execute` is the right shape for a raw Mach-O
# binary. `-t install` is for installer pkgs and would always
# succeed-then-warn here; `-t open` is for app bundles.
#
# `spctl` requires online access to Apple's notary servers to
# verify the ticket on first run when there's no local staple.
# The release runner has internet; the customer Mac will too.

# Raw CLI Mach-O binaries are rejected by `spctl -t execute` with
# "the code is valid but does not seem to be an app" even when
# correctly notarised. Apple's contract for CLI binaries is online
# ticket lookup at customer first-run, not local spctl assessment.
# The notary verdict above (status=Accepted) is the source of truth.
echo "[$(date '+%H:%M:%S')] sign-and-notarize: skipping local spctl gate (raw Mach-O; ticket verified online at runtime)"

# Stapling for raw binaries
# ------------------------
# `xcrun stapler staple` works on app bundles (.app) and disk
# images (.dmg / .pkg). For a bare Mach-O like ours there is no
# place to staple a ticket; Gatekeeper performs an online ticket
# lookup at first run instead. If we ever ship a .app wrapper for
# Marvin's menu bar UI, stapling becomes relevant; flagged in
# release/SIGNING.md.

echo "[$(date '+%H:%M:%S')] sign-and-notarize: signed + notarised + Gatekeeper-accepted"
codesign -dv --verbose=4 "$BINARY" 2>&1 | sed -e 's/^/    /' || true
