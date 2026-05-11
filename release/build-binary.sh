#!/usr/bin/env bash
#
# build-binary.sh
#
# Local reproduction of `.github/workflows/build-macos.yml`. Run on
# any Apple Silicon Mac to produce the same `ostler-assistant-
# aarch64-apple-darwin-<version>.tar.gz` artefact GitHub Actions
# publishes on a `v*` tag push.
#
# Use cases:
#   - Smoke a build before tagging (avoid pushing a tag that breaks
#     the release pipeline).
#   - Produce a tarball locally for offline / air-gapped install
#     bring-up.
#   - Bisect a regression against the same toolchain pinning.
#
# Usage:
#   release/build-binary.sh [VERSION]
#
# `VERSION` defaults to the workspace version from Cargo.toml. The
# resulting tarball lands at the repo root.
#
# Prerequisites:
#   - macOS arm64
#   - Rust toolchain matching the workflow pin (rust-toolchain or
#     rustup default; the workflow uses 1.93.0)
#   - aarch64-apple-darwin target installed (`rustup target add
#     aarch64-apple-darwin`)
#   - Node.js 22 or later (for the web/ dashboard build)
#
# British English throughout.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

TARGET="aarch64-apple-darwin"

# Resolve the version: explicit arg wins, otherwise workspace
# package version. Strip any leading `v` so the user can pass either
# form.
if [ "${1:-}" != "" ]; then
    VERSION="${1#v}"
else
    VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
fi
if [ -z "$VERSION" ]; then
    echo "ERROR: could not resolve version (pass as arg or set Cargo.toml version)" >&2
    exit 1
fi

echo "[$(date '+%H:%M:%S')] Building ostler-assistant v${VERSION} for ${TARGET}"

# Build web dashboard first; the gateway feature serves these
# assets at runtime. Stays in sync with the workflow's behaviour.
echo "[$(date '+%H:%M:%S')] Building web dashboard"
(cd web && npm ci --silent && npm run build --silent)

echo "[$(date '+%H:%M:%S')] cargo build --release --locked (native arm64)"
cargo build --release --locked

BIN_PATH="target/release/zeroclaw"
if [ ! -x "$BIN_PATH" ]; then
    echo "ERROR: expected built binary at $BIN_PATH" >&2
    exit 1
fi

echo "[$(date '+%H:%M:%S')] Verifying binary"
"$BIN_PATH" --version

# Customer-facing rename happens at tar time; the Cargo.toml
# `[[bin]]` is left as `zeroclaw` to avoid cascading edits into
# upstream Dockerfiles + install.sh + AUR/Scoop pipelines.
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

cp "$BIN_PATH" "$STAGE/ostler-assistant"
strip "$STAGE/ostler-assistant"
chmod 755 "$STAGE/ostler-assistant"

# Sign + notarise BEFORE tarring. Strip rewrites the Mach-O so
# any signature applied earlier would be invalidated; signing
# after strip is the only correct order.
#
# When OSTLER_SIGNING_IDENTITY is unset the script logs "skipping"
# and exits 0, so the local build keeps producing the same v0.1
# unsigned tarball as today. Once a Developer ID certificate is
# enrolled and the env vars are populated, this same script
# produces a signed + notarised binary at the same artefact name
# (downstream install.sh untouched, customer-facing tarball name
# unchanged).
"$REPO_ROOT/release/sign-and-notarize.sh" "$STAGE/ostler-assistant"

ARCHIVE="ostler-assistant-${TARGET}-v${VERSION}.tar.gz"
tar czf "$ARCHIVE" -C "$STAGE" ostler-assistant
shasum -a 256 "$ARCHIVE" | tee "${ARCHIVE}.sha256"

echo
echo "[$(date '+%H:%M:%S')] Done."
echo "  Archive:  $REPO_ROOT/$ARCHIVE"
echo "  Checksum: $REPO_ROOT/${ARCHIVE}.sha256"
