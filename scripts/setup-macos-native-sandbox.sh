#!/usr/bin/env bash
# Prepare the ~/CellarBench sandbox for macos-native bench runs.
#
# Run once after cloning (and whenever the fixtures change). Safe to re-run.
# Does NOT modify anything outside $HOME/CellarBench.
#
# Preconditions (manual — the script can't grant them):
#   1. Accessibility permission granted to the Node binary running the bench
#      (System Settings → Privacy & Security → Accessibility).
#   2. Automation permission for osascript → System Events / Finder / Notes /
#      Safari / System Settings (will prompt on first bench run).
#   3. Optional: a dedicated macOS user account to isolate real data. See
#      docs/eval-isolation.md for the `_celeval` user recipe.

set -euo pipefail

SANDBOX_DIR="$HOME/CellarBench"

echo "→ Preparing macOS-native bench sandbox at $SANDBOX_DIR"

mkdir -p "$SANDBOX_DIR"

# Seed marker file so the bench can assert the sandbox is provisioned.
# Dotfiles aren't cleaned between runs.
if [[ ! -f "$SANDBOX_DIR/.seed" ]]; then
  cat > "$SANDBOX_DIR/.seed" <<'EOF'
This directory is the Cellar macos-native bench sandbox.
Tasks create and delete files here between runs — do NOT put real data
here. Clean it manually with: rm -rf ~/CellarBench/*
EOF
  echo "  ✓ seeded $SANDBOX_DIR/.seed"
fi

# Check Automation permissions (best-effort — a permission prompt will
# appear on first osascript call for each target app).
echo ""
echo "→ Checking osascript availability"
if command -v osascript >/dev/null 2>&1; then
  echo "  ✓ osascript found"
else
  echo "  ✗ osascript missing — macOS command-line tools may be broken" >&2
  exit 1
fi

# A test Note used by notes-append-to-existing
echo ""
echo "→ Creating baseline 'Bench Run Log' note (required by notes-append-to-existing)"
if osascript <<'EOF' 2>/dev/null
tell application "Notes"
  if not (exists note "Bench Run Log") then
    set newNote to make new note with properties {name:"Bench Run Log", body:"Initial bench run log."}
  end if
end tell
EOF
then
  echo "  ✓ 'Bench Run Log' note present"
else
  echo "  ⚠ Notes automation permission not granted. The first run of the"
  echo "    bench will prompt; accept, then re-run this setup script."
fi

echo ""
echo "→ Sandbox ready. Next:"
echo "    cd benchmarks && npx tsx src/standard/osworld/macos-native/runner.ts --limit 2"
echo ""
echo "Note: the runner currently stubs Cel invocation. Live integration"
echo "requires the NAPI binding to be loaded and cortex booted. See"
echo "benchmarks/src/standard/osworld/README.md for the wiring plan."
