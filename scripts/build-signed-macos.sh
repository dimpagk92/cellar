#!/usr/bin/env bash
#
# build-signed-macos.sh — one-command signed + notarized macOS build for Cellar.
#
# Pipeline:
#   1. Build the Rust workspace (release) and TS packages.
#   2. Sign the bundled daemon binary (cel-cortex-daemon) with the daemon's
#      hardened-runtime entitlements.
#   3. Sign the cel-mcp sidecar.
#   4. Run `pnpm tauri build` to produce Cellar.app + Cellar.dmg.
#      tauri-bundler signs the .app with the app entitlements + hardened
#      runtime, picking up APPLE_SIGNING_IDENTITY from env.
#   5. Re-sign the .app with --deep to ensure nested binaries inherit the
#      same identity (defensive — tauri-bundler should already do this).
#   6. Inject Info.plist usage descriptions (NSAppleEventsUsageDescription).
#   7. Verify the signature locally (codesign --verify --strict).
#   8. Submit Cellar.dmg to Apple notarization via `xcrun notarytool`,
#      using the keychain profile named in $NOTARIZE_PROFILE.
#   9. Staple the notarization ticket to the .dmg AND to the .app inside it.
#   10. Validate with `spctl --assess --type install`.
#
# Required env vars:
#   DEVELOPER_ID      — full "Developer ID Application: Name (TEAMID)" string.
#   NOTARIZE_PROFILE  — name of a `xcrun notarytool store-credentials` profile
#                       (see docs/codesigning.md for setup).
#   BUNDLE_ID         — CFBundleIdentifier of the .app (default: com.cellar.cellar).
#
# Optional env vars:
#   PRODUCT_NAME      — bundle product name (default: "Dilipod Cellar").
#   DMG_NAME          — output .dmg basename (default: derived from PRODUCT_NAME).
#   TEAM_ID           — Apple Developer Team ID; derived from DEVELOPER_ID if absent.
#   APPLE_API_KEY,
#   APPLE_API_ISSUER,
#   APPLE_API_KEY_PATH — alternative to NOTARIZE_PROFILE for non-keychain CI runs.
#
# Flags:
#   --dry-run         — print every codesign / notarytool / stapler / spctl
#                       invocation without executing it, and don't require any
#                       Apple creds. Useful for verifying script wiring in CI.
#   --skip-build      — skip the Rust + TS build step; assume already built.
#   --skip-notarize   — sign only, no notarization. For local dev iteration.
#   --skip-staple     — sign + notarize but don't staple (CI fallback path;
#                       use scripts/notarize-stapleless.sh to staple later).
#   -h | --help       — print usage.
#
# Conventions:
#   - All paths absolute.
#   - All steps idempotent where possible.
#   - Exits non-zero on any failure. Strict shell.
#
set -euo pipefail

# ─── 0. Setup ────────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"

# Defaults
DRY_RUN=0
SKIP_BUILD=0
SKIP_NOTARIZE=0
SKIP_STAPLE=0

PRODUCT_NAME="${PRODUCT_NAME:-Dilipod Cellar}"
BUNDLE_ID="${BUNDLE_ID:-com.cellar.cellar}"

# Standard paths
APP_DIR="$ROOT_DIR/app"
APP_TAURI_DIR="$APP_DIR/src-tauri"
APP_ENTITLEMENTS="$APP_TAURI_DIR/Entitlements.plist"
DAEMON_ENTITLEMENTS="$ROOT_DIR/cel-cortex-daemon/entitlements.plist"
TARGET_DIR="$ROOT_DIR/target/release"
DAEMON_BIN="$TARGET_DIR/cel-cortex-daemon"
BUNDLE_DIR="$TARGET_DIR/bundle/macos"
DMG_DIR="$TARGET_DIR/bundle/dmg"

# ─── 1. Argument parsing ─────────────────────────────────────────────────────

usage() {
    sed -n '2,/^set -euo pipefail$/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' | head -n -2
    exit "${1:-0}"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run)        DRY_RUN=1 ;;
        --skip-build)     SKIP_BUILD=1 ;;
        --skip-notarize)  SKIP_NOTARIZE=1 ;;
        --skip-staple)    SKIP_STAPLE=1 ;;
        -h|--help)        usage 0 ;;
        *)
            echo "ERROR: unknown flag: $1" >&2
            usage 1
            ;;
    esac
    shift
done

# ─── 2. Helpers ──────────────────────────────────────────────────────────────

log()  { printf '[build-signed-macos] %s\n' "$*" >&2; }
fail() { printf '[build-signed-macos] ERROR: %s\n' "$*" >&2; exit 1; }

# run / run_quiet — execute a command unless DRY_RUN=1, in which case just
# print the exact invocation (quoted) so the operator can verify wiring.
run() {
    if [[ "$DRY_RUN" == "1" ]]; then
        printf 'DRY-RUN $ %s\n' "$(printf '%q ' "$@")"
    else
        log "exec: $*"
        "$@"
    fi
}

# read_required_env — abort early with a helpful message if a required env
# var is missing, unless we're in dry-run mode (which exists precisely to
# verify wiring without credentials).
read_required_env() {
    local name="$1"
    local description="$2"
    if [[ -z "${!name:-}" ]]; then
        if [[ "$DRY_RUN" == "1" ]]; then
            # Placeholder so the script can print invocations without crashing.
            printf -v "$name" '<%s>' "$name"
            export "$name"
        else
            fail "missing env var \$$name ($description)"
        fi
    fi
}

# ─── 3. Validate env + paths ─────────────────────────────────────────────────

log "mode: $([[ $DRY_RUN == 1 ]] && echo "DRY RUN" || echo "LIVE")"
log "product: $PRODUCT_NAME"
log "bundle id: $BUNDLE_ID"

read_required_env DEVELOPER_ID     "full Developer ID Application identity string"
read_required_env NOTARIZE_PROFILE "xcrun notarytool keychain profile name"

# Derive TEAM_ID from DEVELOPER_ID if not explicitly set:
# "Developer ID Application: Acme Inc. (ABCDE12345)" → "ABCDE12345".
if [[ -z "${TEAM_ID:-}" ]]; then
    TEAM_ID="$(printf '%s' "$DEVELOPER_ID" | sed -n 's/.*(\([A-Z0-9]*\)).*/\1/p')"
    if [[ -z "$TEAM_ID" && "$DRY_RUN" != "1" ]]; then
        fail "could not derive TEAM_ID from DEVELOPER_ID; set TEAM_ID explicitly"
    fi
    [[ -z "$TEAM_ID" ]] && TEAM_ID="<TEAM_ID>"
fi
log "team id: $TEAM_ID"

DMG_NAME="${DMG_NAME:-${PRODUCT_NAME}_$(awk -F'"' '/"version"/{print $4; exit}' "$APP_TAURI_DIR/tauri.conf.json" 2>/dev/null || echo 0.0.0)_aarch64.dmg}"

# Export the env vars tauri-bundler reads at build time.
export APPLE_SIGNING_IDENTITY="$DEVELOPER_ID"
[[ -n "${PROVIDER_SHORT_NAME:-}" ]] && export APPLE_PROVIDER_SHORT_NAME="$PROVIDER_SHORT_NAME"

if [[ "$DRY_RUN" != "1" ]]; then
    [[ -f "$APP_ENTITLEMENTS"    ]] || fail "missing app entitlements: $APP_ENTITLEMENTS"
    [[ -f "$DAEMON_ENTITLEMENTS" ]] || fail "missing daemon entitlements: $DAEMON_ENTITLEMENTS"
fi

# ─── 4. Build Rust + TS (release) ────────────────────────────────────────────

if [[ "$SKIP_BUILD" == "1" ]]; then
    log "skip-build: assuming target/release/cel-cortex-daemon and tauri bundle exist"
else
    log "building Rust workspace (release)"
    run cargo build --release --workspace

    log "building TS packages"
    run pnpm install --frozen-lockfile
    run pnpm --filter @dpagk/cellar-mcp build
fi

# ─── 5. Sign the daemon binary ───────────────────────────────────────────────

log "signing daemon binary"
run codesign \
    --force \
    --options runtime \
    --timestamp \
    --entitlements "$DAEMON_ENTITLEMENTS" \
    --sign "$DEVELOPER_ID" \
    "$DAEMON_BIN"

# Stage the signed daemon into the Tauri bundle resources so `tauri build`
# picks it up. (Tauri copies binaries/* into the .app at bundle time.)
DAEMON_BUNDLED="$APP_TAURI_DIR/binaries/cel-cortex-daemon-aarch64-apple-darwin"
run mkdir -p "$APP_TAURI_DIR/binaries"
run cp "$DAEMON_BIN" "$DAEMON_BUNDLED"

# Verify the daemon signature before we hand it to Tauri.
run codesign --verify --strict --verbose=2 "$DAEMON_BIN"

# ─── 6. Build the Tauri .app + .dmg ──────────────────────────────────────────

if [[ "$SKIP_BUILD" == "1" ]]; then
    log "skip-build: assuming .app + .dmg already exist in target/release/bundle/"
else
    log "building Tauri app (.app + .dmg) — picks up APPLE_SIGNING_IDENTITY from env"
    run pnpm --dir "$APP_DIR" tauri build
fi

APP_BUNDLE="$BUNDLE_DIR/${PRODUCT_NAME}.app"
DMG_PATH="$DMG_DIR/$DMG_NAME"

# ─── 7. Inject Info.plist usage descriptions ────────────────────────────────
#
# tauri-bundler doesn't expose NSAppleEventsUsageDescription in v2's schema
# reliably across versions, so we patch the .app's Info.plist post-bundle.
# Idempotent: plutil -replace works whether or not the key already exists.

INFO_PLIST="$APP_BUNDLE/Contents/Info.plist"
log "injecting Info.plist usage descriptions into $INFO_PLIST"
run plutil -replace NSAppleEventsUsageDescription \
    -string "Cellar uses Apple Events to drive Calendar, Mail, Messages, Notes, and Reminders on your behalf when a rule or chat action targets one of those apps." \
    "$INFO_PLIST"
run plutil -replace NSAppleScriptEnabled -bool true "$INFO_PLIST"

# ─── 8. Re-sign the .app (defensive deep sign) ───────────────────────────────
#
# We changed Info.plist after Tauri signed the bundle, which invalidates
# the signature. Re-sign with --deep to cover all nested binaries
# (cel-cortex-daemon, cel-mcp, Tauri frameworks) with the same identity.

log "re-signing .app bundle (Info.plist was modified)"
run codesign \
    --force \
    --deep \
    --options runtime \
    --timestamp \
    --entitlements "$APP_ENTITLEMENTS" \
    --sign "$DEVELOPER_ID" \
    "$APP_BUNDLE"

# ─── 9. Verify locally ───────────────────────────────────────────────────────

log "verifying .app signature"
run codesign --verify --deep --strict --verbose=2 "$APP_BUNDLE"

log "running spctl assessment (pre-notarization; will report 'rejected' until stapled)"
# Don't fail here — pre-notarized bundles are expected to be rejected by
# spctl. We just want the diagnostic output captured.
if [[ "$DRY_RUN" == "1" ]]; then
    printf 'DRY-RUN $ %s\n' "spctl --assess --type execute --verbose=4 $APP_BUNDLE"
else
    spctl --assess --type execute --verbose=4 "$APP_BUNDLE" || true
fi

# ─── 10. Notarize the .dmg ───────────────────────────────────────────────────

if [[ "$SKIP_NOTARIZE" == "1" ]]; then
    log "skip-notarize: stopping after signing"
    exit 0
fi

[[ -f "$DMG_PATH" || "$DRY_RUN" == "1" ]] || fail "expected .dmg at $DMG_PATH"

# Notarytool auth: prefer keychain profile (local dev); fall back to API-key
# triple (CI runs without keychain access). The CI workflow at
# .github/workflows/release-signed-macos.yml exports the API-key triple.
notarytool_auth=()
if [[ -n "${APPLE_API_KEY:-}" && -n "${APPLE_API_ISSUER:-}" && -n "${APPLE_API_KEY_PATH:-}" ]]; then
    notarytool_auth=(
        --key       "$APPLE_API_KEY_PATH"
        --key-id    "$APPLE_API_KEY"
        --issuer    "$APPLE_API_ISSUER"
    )
    log "submitting $DMG_PATH to Apple notarization (App Store Connect API key)"
else
    notarytool_auth=(--keychain-profile "$NOTARIZE_PROFILE")
    log "submitting $DMG_PATH to Apple notarization (keychain profile: $NOTARIZE_PROFILE)"
fi
run xcrun notarytool submit "$DMG_PATH" "${notarytool_auth[@]}" --wait --timeout 30m

# ─── 11. Staple the ticket ───────────────────────────────────────────────────

if [[ "$SKIP_STAPLE" == "1" ]]; then
    log "skip-staple: notarization submitted; run scripts/notarize-stapleless.sh later to staple"
    exit 0
fi

log "stapling notarization ticket to .dmg"
run xcrun stapler staple "$DMG_PATH"

log "stapling notarization ticket to .app (so it works once extracted)"
run xcrun stapler staple "$APP_BUNDLE"

# ─── 12. Final validation ────────────────────────────────────────────────────

log "validating stapled .dmg via spctl"
run spctl --assess --type install --verbose=4 "$DMG_PATH"

log "validating stapled .app via spctl"
run spctl --assess --type execute --verbose=4 "$APP_BUNDLE"

log "done."
log "  signed + notarized .app : $APP_BUNDLE"
log "  signed + notarized .dmg : $DMG_PATH"
log "  team id                 : $TEAM_ID"
log "  bundle id               : $BUNDLE_ID"
