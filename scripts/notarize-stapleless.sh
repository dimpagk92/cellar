#!/usr/bin/env bash
#
# notarize-stapleless.sh — submit an already-signed artifact for notarization
# and (optionally) staple later.
#
# Why this exists: when running in CI we sometimes can't staple inline.
# `xcrun stapler staple` requires the artifact to live on a writable
# filesystem at submission time AND for the runner to still be online
# when Apple's status flips to "Accepted" — which can take 5–30 minutes.
# Either the runner ages out before the staple completes, or the artifact
# has already been uploaded as a release asset and is no longer on disk.
#
# In those cases:
#   1. Run this script in CI to submit + wait for the verdict.
#   2. Download the .dmg / .app from the release later, on any Mac.
#   3. Run this script again with `--staple-only` to staple in place.
#
# The notarization ticket lives on Apple's CDN once accepted — stapler
# pulls it down. There's no time limit on stapling.
#
# Required env vars (same as build-signed-macos.sh):
#   NOTARIZE_PROFILE   — keychain profile name; OR
#   APPLE_API_KEY + APPLE_API_ISSUER + APPLE_API_KEY_PATH — for CI runs
#                        without keychain access.
#
# Usage:
#   notarize-stapleless.sh /path/to/artifact.dmg [--submit-only|--staple-only]
#
# Flags:
#   --submit-only   Submit + wait for verdict, but skip stapler.
#   --staple-only   Skip submission; just run `stapler staple` on the
#                   artifact (assumes Apple has already accepted it).
#   --dry-run       Print invocations without running them.
#   -h | --help     Print usage.
#
set -euo pipefail

usage() {
    sed -n '2,/^set -euo pipefail$/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' | head -n -2
    exit "${1:-0}"
}

DRY_RUN=0
MODE=both   # both | submit | staple

ARTIFACT=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --submit-only) MODE=submit ;;
        --staple-only) MODE=staple ;;
        --dry-run)     DRY_RUN=1 ;;
        -h|--help)     usage 0 ;;
        -*)
            echo "ERROR: unknown flag: $1" >&2
            usage 1
            ;;
        *)
            if [[ -z "$ARTIFACT" ]]; then
                ARTIFACT="$1"
            else
                echo "ERROR: multiple artifact paths given: '$ARTIFACT' and '$1'" >&2
                usage 1
            fi
            ;;
    esac
    shift
done

if [[ -z "$ARTIFACT" ]]; then
    echo "ERROR: missing artifact path" >&2
    usage 1
fi

log()  { printf '[notarize-stapleless] %s\n' "$*" >&2; }
fail() { printf '[notarize-stapleless] ERROR: %s\n' "$*" >&2; exit 1; }

run() {
    if [[ "$DRY_RUN" == "1" ]]; then
        printf 'DRY-RUN $ %s\n' "$(printf '%q ' "$@")"
    else
        log "exec: $*"
        "$@"
    fi
}

[[ "$DRY_RUN" == "1" || -e "$ARTIFACT" ]] || fail "artifact does not exist: $ARTIFACT"

# Build the auth arguments for notarytool. Prefer keychain profile; fall
# back to API key triple (for CI runs that can't access the keychain).
auth_args=()
if [[ -n "${NOTARIZE_PROFILE:-}" ]]; then
    auth_args=(--keychain-profile "$NOTARIZE_PROFILE")
elif [[ -n "${APPLE_API_KEY:-}" && -n "${APPLE_API_ISSUER:-}" && -n "${APPLE_API_KEY_PATH:-}" ]]; then
    auth_args=(
        --key       "$APPLE_API_KEY_PATH"
        --key-id    "$APPLE_API_KEY"
        --issuer    "$APPLE_API_ISSUER"
    )
elif [[ "$DRY_RUN" == "1" ]]; then
    auth_args=(--keychain-profile '<NOTARIZE_PROFILE>')
else
    fail "no auth: set \$NOTARIZE_PROFILE OR \$APPLE_API_KEY + \$APPLE_API_ISSUER + \$APPLE_API_KEY_PATH"
fi

log "artifact: $ARTIFACT"
log "mode:     $MODE"

if [[ "$MODE" == "both" || "$MODE" == "submit" ]]; then
    log "submitting to Apple notarization"
    run xcrun notarytool submit "$ARTIFACT" "${auth_args[@]}" --wait --timeout 30m
fi

if [[ "$MODE" == "both" || "$MODE" == "staple" ]]; then
    log "stapling notarization ticket"
    run xcrun stapler staple "$ARTIFACT"
    log "validating with spctl"
    case "$ARTIFACT" in
        *.dmg) run spctl --assess --type install --verbose=4 "$ARTIFACT" ;;
        *.app|*.app/) run spctl --assess --type execute --verbose=4 "$ARTIFACT" ;;
        *)     log "skipping spctl: unrecognized artifact extension" ;;
    esac
fi

log "done."
