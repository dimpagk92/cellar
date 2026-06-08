# Codesigning + Notarization

Operator runbook for producing signed + notarized macOS builds of Cellar
(`Cellar.app` / `Dilipod Cellar.app` + `.dmg`).

This document covers:

1. [Why we need this](#1-why-codesigning--notarization-is-required-for-v1)
2. [What gets signed](#2-what-gets-signed)
3. [Local setup (one-time)](#3-local-setup-one-time)
4. [Producing a signed build locally](#4-producing-a-signed-build-locally)
5. [CI setup (GitHub Actions secrets)](#5-ci-setup-github-actions-secrets)
6. [Debugging a notarization rejection](#6-debugging-a-notarization-rejection)
7. [Reading `spctl --assess` errors](#7-reading-spctl---assess-errors)
8. [Entitlements explained](#8-entitlements-explained)
9. [Re-signing after losing the identity](#9-re-signing-after-losing-the-identity)

---

## 1. Why codesigning + notarization is required for v1

Per `cellar-app-v1.md` §17 decision 10, signing + notarization is **required**
for v1, not optional. The reasons:

- macOS refuses to grant the **Accessibility** permission to an unsigned binary
  on the first try. Cellar relies on the AX API for screen reads. Without
  signing, the user has to manually move Cellar out of quarantine before the
  permission grant works at all.
- The **LaunchAgent** plist refuses to load a binary that fails the
  hardened-runtime check, so the daemon won't auto-start on login.
- Once the user grants Accessibility / Automation permissions to the signed
  Cellar identity, those grants survive updates **only if** the new build is
  signed by the same Team ID. Switching identities silently invalidates all
  TCC grants.
- Distributing an unsigned `.dmg` triggers Gatekeeper's "cannot be opened
  because the developer cannot be verified" dialog on every fresh download.

## 2. What gets signed

Three artifacts, in order:

1. **`cel-cortex-daemon`** binary
   - Built by `cargo build --release -p cel-cortex-daemon`.
   - Signed standalone *before* it is bundled into `Cellar.app`, because
     `codesign --deep` does not always re-sign nested binaries reliably.
   - Entitlements: `cel-cortex-daemon/entitlements.plist` (Apple Events,
     no JIT, library validation disabled for adapter loading).
2. **`cel-mcp`** sidecar
   - Bundled into `Cellar.app/Contents/MacOS/` by `tauri-bundler`.
   - `tauri build` signs this with the app's identity (because
     `APPLE_SIGNING_IDENTITY` is exported).
3. **`Cellar.app`** bundle (and the resulting `.dmg`)
   - Signed with `--options runtime --entitlements app/src-tauri/Entitlements.plist`
     using the workspace's hardened runtime entitlements (see
     [§8 below](#8-entitlements-explained)).
   - Re-signed after `plutil` injects `NSAppleEventsUsageDescription`
     into `Info.plist` (modifying Info.plist invalidates the signature).
   - The `.dmg` is signed indirectly: notarization staples the ticket to
     the `.dmg`, which Gatekeeper then accepts.

## 3. Local setup (one-time)

Prerequisites:

- Apple Developer Program membership ($99/year).
- Xcode Command Line Tools (`xcode-select --install`).
- A "Developer ID Application" certificate, generated from
  [developer.apple.com → Certificates → +](https://developer.apple.com/account/resources/certificates/list).
  Download the `.cer`, double-click to import into Keychain Access.

### 3.1 Verify the identity is visible to `codesign`

```bash
security find-identity -v -p codesigning
# Expected output, one line:
#   1) ABCDEF1234567890 "Developer ID Application: Your Name (TEAMID12345)"
#      1 valid identities found
```

If you see "0 valid identities", the cert isn't in the login keychain or it
has expired. Re-download from developer.apple.com and re-import.

### 3.2 Create a notarization keychain profile

`xcrun notarytool` needs auth on every submission. We store the credentials
in the keychain once, then reference them by profile name.

You need an **app-specific password** for the Apple ID associated with your
team (generate at https://appleid.apple.com → Sign-In and Security →
App-Specific Passwords).

```bash
xcrun notarytool store-credentials cellar-notarize \
    --apple-id    "you@example.com" \
    --team-id     "TEAMID12345" \
    --password    "app-specific-password-here"
```

The profile name `cellar-notarize` is the value you'll pass as
`$NOTARIZE_PROFILE`.

### 3.3 Set the env vars in your shell

```bash
# Add to ~/.zshrc or ~/.bash_profile
export DEVELOPER_ID="Developer ID Application: Your Name (TEAMID12345)"
export NOTARIZE_PROFILE="cellar-notarize"
export BUNDLE_ID="com.cellar.cellar"   # optional; this is the default
```

## 4. Producing a signed build locally

### 4.1 Dry-run first

Verify the script wiring without touching `codesign`:

```bash
./scripts/build-signed-macos.sh --dry-run
```

This prints every `codesign` / `notarytool` / `stapler` / `spctl`
invocation. Use it to sanity-check entitlement paths, identity strings,
and the order of operations. No Apple creds required.

### 4.2 Live build

```bash
./scripts/build-signed-macos.sh
```

End-to-end pipeline:

1. `cargo build --release --workspace`
2. `pnpm install` + `pnpm --filter @dpagk/cellar-mcp build`
3. Sign `target/release/cel-cortex-daemon` with daemon entitlements
4. Stage the signed daemon into `app/src-tauri/binaries/`
5. `pnpm tauri build` → `Cellar.app` + `Cellar.dmg`
6. `plutil -replace NSAppleEventsUsageDescription` on the `.app`'s Info.plist
7. Re-sign the `.app` with `--deep` + app entitlements
8. `xcrun notarytool submit ... --wait` (5–30 min, blocking)
9. `xcrun stapler staple` on `.dmg` and `.app`
10. `spctl --assess` to confirm Gatekeeper accepts

Outputs:

```
target/release/bundle/macos/Dilipod Cellar.app   ← signed + stapled
target/release/bundle/dmg/Dilipod Cellar_0.1.0_aarch64.dmg
```

### 4.3 Useful flags

| Flag | Purpose |
|------|---------|
| `--dry-run` | Print invocations, don't run them. No creds needed. |
| `--skip-build` | Assume `cargo` + `pnpm build` already ran. Faster iteration. |
| `--skip-notarize` | Sign only. Use for local install testing — Gatekeeper will reject. |
| `--skip-staple` | Submit for notarization but don't staple. Use `notarize-stapleless.sh` later. |

### 4.4 Stapleless fallback

If the build runner can't staple inline (CI runner ages out, artifact
already uploaded as a release asset), use the dedicated script:

```bash
# Submit for notarization but don't staple
./scripts/build-signed-macos.sh --skip-staple

# Later, on any Mac with stapler installed:
./scripts/notarize-stapleless.sh /path/to/Cellar.dmg --staple-only
```

## 5. CI setup (GitHub Actions secrets)

The workflow `.github/workflows/release-signed-macos.yml` runs on every
`v*.*.*` tag push. It needs these **GitHub Actions secrets** configured at
the repo (Settings → Secrets and variables → Actions):

| Secret | Description |
|--------|-------------|
| `APPLE_CERTIFICATE_BASE64` | `base64 -i Developer_ID_Application.p12` of the exported cert + private key |
| `APPLE_CERTIFICATE_PASSWORD` | Password set when exporting the `.p12` from Keychain Access |
| `KEYCHAIN_PASSWORD` | Any strong password — used only for the ephemeral CI keychain |
| `DEVELOPER_ID` | `"Developer ID Application: Name (TEAMID)"` — must match the cert's CN exactly |
| `APPLE_API_KEY` | App Store Connect API key id (e.g. `ABCDE12345`) |
| `APPLE_API_ISSUER` | App Store Connect issuer id (UUID) |
| `APPLE_API_KEY_P8_BASE64` | `base64 -i AuthKey_ABCDE12345.p8` of the API key |
| `BUNDLE_ID` | (optional) `com.cellar.cellar` if unset |

### 5.1 Exporting the .p12

In Keychain Access:

1. Find your "Developer ID Application: …" entry in the **My Certificates**
   category (not "Certificates" — that view is missing the private key).
2. Right-click → Export.
3. Format: Personal Information Exchange (.p12). Set a password — this is
   `APPLE_CERTIFICATE_PASSWORD`.
4. `base64 -i Developer_ID_Application.p12 | pbcopy` → paste into the
   GitHub secret.

### 5.2 Creating the App Store Connect API key

1. https://appstoreconnect.apple.com → Users and Access → Integrations →
   App Store Connect API → +.
2. Access: "Developer".
3. Download the `.p8` (one chance). The Key ID is shown in the table; the
   Issuer ID is at the top of the page.

```bash
base64 -i AuthKey_ABCDE12345.p8 | pbcopy   # → APPLE_API_KEY_P8_BASE64
echo "ABCDE12345"                          # → APPLE_API_KEY
echo "deadbeef-...-..."                    # → APPLE_API_ISSUER
```

## 6. Debugging a notarization rejection

When `xcrun notarytool submit --wait` returns `status: Invalid`, fetch the
log:

```bash
xcrun notarytool log <submission-id> \
    --keychain-profile cellar-notarize \
    /tmp/notarize-log.json

# or with API key:
xcrun notarytool log <submission-id> \
    --key       "$APPLE_API_KEY_PATH" \
    --key-id    "$APPLE_API_KEY" \
    --issuer    "$APPLE_API_ISSUER" \
    /tmp/notarize-log.json

cat /tmp/notarize-log.json | jq '.issues'
```

Common rejection reasons and fixes:

| Issue | Cause | Fix |
|-------|-------|-----|
| `The binary is not signed with a valid Developer ID certificate.` | Identity used wasn't a Developer ID Application | Re-export, re-import; `security find-identity -v -p codesigning` must show "valid" |
| `The signature does not include a secure timestamp.` | Forgot `--timestamp` flag | The build script always passes it; check that no manual re-sign step omitted it |
| `The executable does not have the hardened runtime enabled.` | Forgot `--options runtime` | Same as above |
| `The entitlements property 'com.apple.security.get-task-allow' is not allowed.` | Debug build accidentally signed | Make sure `cargo build --release` (not `--debug`) |
| `The binary uses an SDK older than the 10.9 SDK.` | Ancient Rust toolchain | `rustup update stable` |
| `The signature of the binary is invalid.` | Nested binary not signed, or signed with a different identity | Sign nested binaries (daemon, sidecar) separately *before* `codesign --deep` on the .app |

## 7. Reading `spctl --assess` errors

`spctl --assess --type execute --verbose=4 Cellar.app` is the local
Gatekeeper simulation. Possible outputs:

| Output | Meaning | Action |
|--------|---------|--------|
| `accepted source=Notarized Developer ID` | Best case: signed + notarized + stapled | Ship it |
| `accepted source=Developer ID` | Signed but not notarized | Notarize and staple |
| `rejected source=Unnotarized Developer ID` | Same as above but Gatekeeper assessment is harsher | Notarize |
| `rejected (the code is valid but does not seem to be an app)` | `.app` bundle structure is broken (missing Info.plist, wrong layout) | Check `Cellar.app/Contents/Info.plist` exists; check `CFBundleExecutable` matches a binary in `Contents/MacOS/` |
| `a sealed resource is missing or invalid` | Modified the bundle after signing | Re-sign with `--force --deep` (the build script does this after the Info.plist patch) |
| `rejected: invalid Info.plist` | Plist is malformed (e.g., trailing `<plist version="1.0">` with no close tag) | `plutil -lint Cellar.app/Contents/Info.plist` |

## 8. Entitlements explained

The two entitlements files in this repo and why each claim is present:

### 8.1 `app/src-tauri/Entitlements.plist` (the .app bundle)

| Entitlement | Required for |
|-------------|--------------|
| `com.apple.security.app-sandbox = false` | We do not run sandboxed — sandbox would block Apple Events, AX reads, FSEvents, IPC over Unix sockets. |
| `com.apple.security.automation.apple-events` | AppleScript / OSAScript calls into Calendar / Mail / Messages / Notes / Reminders adapters. Without this, calls fail silently even after the user has granted TCC permission. |
| `com.apple.security.cs.disable-library-validation` | Daemon + sidecar load adapter ProcessDriver binaries built by the same workspace. Library validation insists everything in the bundle is signed by the same Team ID, which works for us — but Tauri's WKWebView frameworks come from system locations with different signatures, so we keep this off. |
| `com.apple.security.cs.allow-jit` | WKWebView and the embedded JS runtime (NL rule compiler) JIT-compile JS at runtime. |
| `com.apple.security.cs.allow-unsigned-executable-memory` | Some Rust dylibs and the JS runtime allocate executable pages that are not signed (JIT trampolines, ffi shims). Hardened runtime denies these by default. |

### 8.2 `cel-cortex-daemon/entitlements.plist` (the daemon binary)

Stricter — no JS runtime in the daemon, so no JIT entitlements.

| Entitlement | Required for |
|-------------|--------------|
| `com.apple.security.app-sandbox = false` | Same as above. |
| `com.apple.security.automation.apple-events` | Embedded agent + cel_act gateway dispatch actions through AppleScript adapters. |
| `com.apple.security.cs.disable-library-validation` | Daemon loads adapter ProcessDriver binaries. |

### 8.3 Entitlements NOT included (and why)

These are sometimes copy-pasted into entitlements files without justification.
We deliberately omit them; if anyone proposes adding one, this list documents
why we said no:

| Entitlement | Why we omit |
|-------------|-------------|
| `com.apple.security.cs.allow-dyld-environment-variables` | Debug-only. Lets attackers inject dylibs via `DYLD_INSERT_LIBRARIES`. |
| `com.apple.security.cs.debugger` | Debug-only. Lets the process call `task_for_pid` on others. |
| `com.apple.security.get-task-allow` | Debug-only. **Notarization rejects bundles that set this.** |
| `com.apple.security.network.client / .server` | Sandbox-only. Outside the sandbox, all network is allowed by default; adding these in a non-sandboxed binary is noise that future readers will misinterpret. |
| `com.apple.security.files.user-selected.read-write` | Sandbox-only. Same as above for file access. |
| `com.apple.security.files.downloads.read-write` | Sandbox-only. |
| `com.apple.security.device.audio-input` | Not used at v1 (no voice input). |
| `com.apple.security.personal-information.*` | Not used at v1. |

### 8.4 Accessibility is NOT an entitlement

The macOS Accessibility API (`AXUIElement…`, used by `cel-accessibility`)
is gated by the TCC database, not by an entitlement. The user grants it
from System Settings → Privacy & Security → Accessibility. The grant is
keyed by **signed identity** — re-signing with a new identity invalidates
the grant and the user has to re-grant.

Implication: never sign release builds with a different identity than the
one used for the previous release. If you must rotate identities (e.g., the
old one expired), expect users to re-grant Accessibility at first launch
of the new build.

## 9. Re-signing after losing the identity

If the build server is rebuilt or the cert is rotated, you'll need to:

1. Re-issue a "Developer ID Application" cert from developer.apple.com
   (keep the same Team ID — different cert id is OK, same Team ID is what
   TCC keys on).
2. Re-import into the build keychain.
3. Re-export the .p12 and update the `APPLE_CERTIFICATE_BASE64` GitHub
   secret.
4. Update `DEVELOPER_ID` if the cert's CN changed (it shouldn't, unless
   the team name was edited).
5. Ship a release with `cellar doctor --reset-grants` (TODO: not yet
   implemented as of 2026-05; for now users will see the "Cellar wants to
   control your computer" prompt again on first launch of the new build).

---

## Pointers

- `scripts/build-signed-macos.sh` — main build script.
- `scripts/notarize-stapleless.sh` — fallback notarization + staple.
- `.github/workflows/release-signed-macos.yml` — CI release workflow.
- `app/src-tauri/Entitlements.plist` — app entitlements.
- `cel-cortex-daemon/entitlements.plist` — daemon entitlements.
- Apple docs: [Hardened Runtime](https://developer.apple.com/documentation/security/hardened_runtime),
  [Notarizing macOS Software Before Distribution](https://developer.apple.com/documentation/security/notarizing_macos_software_before_distribution).
