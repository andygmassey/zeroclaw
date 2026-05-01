# Code signing + notarisation runbook

This is the operator runbook for enrolling `ostler-assistant`
in Apple's code-signing and notarisation programme. Follow it
once per Apple Developer Program identity. Once the GitHub
secrets are populated, every `v*` tag push produces a signed +
notarised binary at the same artefact name; CM051's installer
auto-detects the signed binary and skips the unsigned-binary
quarantine workaround.

The pipeline ships behind feature gating today. With
`OSTLER_SIGNING_IDENTITY` unset, the build produces the same
unsigned tarball as before; the runbook below is what you do
when you're ready to flip the switch.

## Apple Developer Program tier

You need the **Apple Developer Program** membership
($99/year). Apple's Enterprise Program ($299/year) is for
in-house distribution within an organisation and is NOT
appropriate for a customer-facing distributed binary.

The personal/individual tier is sufficient for v0.1. If you
later split out Ostler Ltd as a legal entity for B2B
distribution, you can transfer the Developer ID to the
organisation; certificates issued under the individual identity
remain valid until they expire (5 years).

## Certificate generation

1. Sign in to the [Apple Developer portal](https://developer.apple.com/account)
   with the Apple ID enrolled in the Developer Program.

2. Navigate to **Certificates, Identifiers & Profiles** then
   **Certificates** then **+** (add).

3. Pick **Developer ID Application**. This is the
   distribution-ready certificate; the **Mac Development**
   certificate is for sandboxed App Store apps and won't satisfy
   Gatekeeper outside the Mac App Store.

4. Apple wants a Certificate Signing Request (CSR). Generate one
   on the build Mac:

   ```
   /Applications/Utilities/Keychain Access.app
   ```

   Menu: **Keychain Access** then **Certificate Assistant** then
   **Request a Certificate from a Certificate Authority**.

   - Email: the Apple ID you enrolled with.
   - Common Name: any (this stays on your Mac).
   - CA Email: leave blank.
   - Saved to disk: **Yes**.

5. Upload the resulting `.certSigningRequest` file to the Apple
   portal. Download the issued `developerID_application.cer` and
   double-click to import into your login keychain.

6. The certificate's Common Name will look like:

   ```
   Developer ID Application: <Your Name or Org> (XXXXXXXXXX)
   ```

   The 10-character `XXXXXXXXXX` is your **Team ID**. Make a
   note of it; it goes into the GitHub secret block below.

7. Verify the certificate is in your login keychain:

   ```
   security find-identity -v -p codesigning
   ```

   You should see one or more `Developer ID Application` lines.

## Notarytool authentication: App Store Connect API key (recommended)

`xcrun notarytool` accepts two authentication paths. **Pick the
API key path** unless you have a specific reason not to:

- An API key is a per-key credential you can revoke
  independently of any Apple ID.
- The Developer access role gives notarytool exactly the
  capabilities it needs and no more.
- App-specific passwords are tied to an Apple ID; revoking the
  password means the human's whole Apple ID flow ends up in
  the same revocation event, which is messier.

The app-specific-password path is documented below as a
fallback for operators who can't reach App Store Connect for
some reason.

### Generate the API key

1. Sign in to
   [App Store Connect](https://appstoreconnect.apple.com/access/integrations/api).
2. Navigate to **Keys** then click **+** to add a new key.
3. **Access role: Developer**. This is the lowest role that
   covers notarytool. Do NOT pick Admin -- it grants more
   surface than the build pipeline needs and a leaked key
   would be a bigger problem.
4. Click **Generate**.
5. **Download the `.p8` private key file ONE TIME ONLY.** Apple
   does not let you re-download. If you lose it, you must
   generate a new key and update every secret that references it.
   Save it to a passphrase-protected file vault.
6. Note the **Key ID** (10 hex chars) and the **Issuer ID**
   (UUID, visible at the top of the Keys page). Both are
   needed alongside the `.p8` for every notarytool invocation.

### Local: keychain-profile

Easiest for repeated local builds. Once you've stored the
profile, every subsequent invocation just references it by
name:

```
xcrun notarytool store-credentials "ostler-assistant" \
    --key /path/to/AuthKey_KEYID.p8 \
    --key-id KEYID \
    --issuer ISSUER-UUID
```

Then export the profile name for the signing script:

```
export OSTLER_NOTARY_KEYCHAIN_PROFILE='ostler-assistant'
```

The signing script picks up the profile and shells out as
`xcrun notarytool submit ... --keychain-profile ostler-assistant`.

### CI: base64-encoded GitHub secret

GitHub Actions secrets store strings, not files. Base64-encode
the `.p8` once on your Mac:

```
base64 -i /path/to/AuthKey_KEYID.p8 | pbcopy
```

Paste the result into a new GitHub repo secret called
`OSTLER_NOTARY_API_KEY_BASE64`. Then add the Key ID and Issuer
ID alongside it (see the **GitHub repo secrets** table below).

The build workflow decodes the base64 secret to a tempfile on
the runner and passes the file path to notarytool via `--key`.
The decoded file is in a runner-local mktemp dir and is wiped
when the runner shuts down.

## App-specific password for notarytool (alternative)

Use this path only when the API key path is not available
(e.g. you don't have App Store Connect access). It works, but
it ties notary auth to an Apple ID which is harder to revoke
cleanly.

1. Sign in to [appleid.apple.com](https://appleid.apple.com).
2. **Sign-In and Security** then **App-Specific Passwords** then
   **Generate an app-specific password**.
3. Label it `ostler-notarytool` (any string; keeps the audit
   trail clear if you generate more later).
4. Copy the 16-character password (`xxxx-xxxx-xxxx-xxxx`). You
   cannot retrieve it again; if lost, generate a new one and
   revoke the old.

## GitHub repo secrets

Populate these on
`https://github.com/ostler-ai/ostler-assistant/settings/secrets/actions`.

The signing identity is required either way. **Pick exactly
one of the two notary auth blocks below** -- the build script
checks the API key block first, falls back to the app-specific
password block, and errors if neither is fully populated.

| Secret name                  | Value                                                      | Always required |
|------------------------------|------------------------------------------------------------|-----------------|
| `OSTLER_SIGNING_IDENTITY`    | `Developer ID Application: <Your Name or Org> (TEAMID)`   | yes             |

**Notary auth block A: API key (recommended)**

| Secret name                       | Value                                                  |
|-----------------------------------|--------------------------------------------------------|
| `OSTLER_NOTARY_API_KEY_BASE64`    | `base64 -i AuthKey_KEYID.p8` output                   |
| `OSTLER_NOTARY_API_KEY_ID`        | The 10-character Key ID                                |
| `OSTLER_NOTARY_API_ISSUER_ID`     | The Issuer UUID                                        |

**Notary auth block B: app-specific password (fallback)**

| Secret name                  | Value                                                      |
|------------------------------|------------------------------------------------------------|
| `OSTLER_NOTARY_APPLE_ID`     | The Apple ID enrolled in the Developer Program            |
| `OSTLER_NOTARY_PASSWORD`     | The 16-character app-specific password                     |
| `OSTLER_NOTARY_TEAM_ID`      | The 10-character Team ID                                   |

Once the signing identity plus exactly one notary auth block
is set, the next `v*` tag push emits a signed + notarised
tarball. The release notes auto-generate with
`Signing status: signed`.

## Local-Mac smoke test

Before populating GitHub secrets and pushing a real tag, verify
the pipeline works on your Mac:

1. Install the Developer ID certificate (steps above).
2. Export the signing identity, then either the API key
   keychain profile (recommended) or the app-specific password
   block:

   ```
   export OSTLER_SIGNING_IDENTITY='Developer ID Application: <Your Name or Org> (XXXXXXXXXX)'

   # Path A: API key via keychain-profile (recommended)
   export OSTLER_NOTARY_KEYCHAIN_PROFILE='ostler-assistant'

   # Path B: app-specific password (alternative; leave Path A
   # vars unset if using this)
   export OSTLER_NOTARY_APPLE_ID='you@example.test'
   export OSTLER_NOTARY_PASSWORD='xxxx-xxxx-xxxx-xxxx'
   export OSTLER_NOTARY_TEAM_ID='XXXXXXXXXX'
   ```

3. Run the release builder:

   ```
   release/build-binary.sh 0.1.0-rc1
   ```

   Expected: cargo builds, the signing script logs codesign +
   notary submission + `Accepted` verdict + spctl pass + final
   `codesign -dv` output. The tarball at the repo root contains
   the signed binary.

4. Verify out-of-band:

   ```
   tar xzf ostler-assistant-aarch64-apple-darwin-v0.1.0-rc1.tar.gz
   codesign --verify --strict --verbose=2 ostler-assistant
   spctl -a -vvv -t execute ostler-assistant
   ```

   Both should report `accepted` plus a `Developer ID Application`
   identity.

## Ad-hoc local test (no Developer ID, no notarisation)

To verify the Hardened Runtime + entitlements wiring without a
paid Developer ID certificate:

```
OSTLER_SIGNING_IDENTITY="-" release/build-binary.sh 0.1.0-rc1
```

The `-` identity engages ad-hoc signing. `codesign -dv` will
show the signature applied; `spctl` will reject (expected; ad-hoc
binaries are not Gatekeeper-trusted). Useful for catching
entitlements typos or Hardened-Runtime-incompatible code paths
before paying for the certificate.

## When to bump entitlements

`release/entitlements.plist` enumerates the Hardened Runtime
restrictions the daemon currently complies with. If you add any
of the following code paths, flip the corresponding key to
`true` and re-sign:

| Capability you add                              | Flip                                                       |
|-------------------------------------------------|------------------------------------------------------------|
| JIT (write+executable memory, e.g. wasmer JIT)  | `com.apple.security.cs.allow-jit`                         |
| Loading unsigned dylibs from disk               | `com.apple.security.cs.disable-library-validation`        |
| Honouring `DYLD_*` env vars                     | `com.apple.security.cs.allow-dyld-environment-variables`  |
| In-process AppleEvents (not via osascript)      | `com.apple.security.automation.apple-events`              |

If you DON'T flip these and the relevant code path runs, macOS
kills the process at runtime with `EXC_BAD_ACCESS` or
`SIGABRT`. The signing script's `codesign --verify --strict` step
catches some malformed entitlements before shipping but cannot
predict runtime needs.

## Troubleshooting

**`notarytool` says `status: Invalid`**

Fetch the log:

```
xcrun notarytool log $NOTARY_ID \
    --apple-id "$OSTLER_NOTARY_APPLE_ID" \
    --team-id "$OSTLER_NOTARY_TEAM_ID" \
    --password "$OSTLER_NOTARY_PASSWORD"
```

Common causes:
- `Hardened Runtime not enabled`: the `--options runtime` flag
  was missing from `codesign`. The script always passes it; if
  you see this, the script was bypassed.
- `Library Validation issue`: a third-party dylib was loaded
  that wasn't signed by Apple or the same team. Either ship the
  dylib bundled and signed by you, or flip
  `disable-library-validation`.
- `Secure Timestamp issue`: `--timestamp` wasn't passed (network
  issue?) or Apple's TSA was unreachable. Retry.

**`codesign` says `unknown error`**

Usually the certificate is in the wrong keychain or expired.
Check with:

```
security find-identity -v -p codesigning
```

If your Developer ID isn't listed, re-import from the keychain
the cert is in (or re-issue from the Apple portal).

**Customer Mac reports "ostler-assistant cannot be opened"**

The notary ticket lookup failed. Check:

1. Customer Mac is online (Gatekeeper does an online ticket
   lookup when there's no local staple, which is our case for
   raw Mach-O binaries).
2. The binary in their `~/.ostler/bin/` matches the signed
   binary in the release tarball:

   ```
   shasum -a 256 ~/.ostler/bin/ostler-assistant
   ```

   Compare against the release SHA-256. If different, the
   installer's quarantine-clearing path may have stripped a
   signature; investigate.

## When stapling becomes relevant

Apple's `xcrun stapler staple` writes the notary ticket into
the file's resource fork or into the bundle's
`Contents/_CodeSignature/`. For a raw Mach-O binary like
`ostler-assistant` there's no place to staple to; Gatekeeper
performs an online lookup at first run instead.

If we ever ship a `.app` wrapper (e.g. for a menu bar UI),
stapling becomes possible AND important: customer Macs without
internet access at first launch would otherwise fail. Add
stapling to `sign-and-notarize.sh` at that point:

```
xcrun stapler staple Ostler.app
```

## Migration timeline

- Today: unsigned tarball; CM051 installer clears the macOS
  quarantine xattr after SHA-256 verification.
- After secrets populated: signed + notarised tarball; CM051
  detects the signature via `codesign -dv` and skips the
  xattr clearing automatically. Same artefact name, same
  installer, same upgrade path.
- Future (if app-bundle wrapper ships): stapled notary
  ticket, no online lookup needed.
