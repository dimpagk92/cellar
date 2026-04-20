#!/usr/bin/env bash
#
# One-command wrapper for the macos-native bench. Handles:
#   - Verifying the _celeval user exists (runs setup-eval-user.sh if not)
#   - Activating that user's session
#   - Passing the right env vars (BENCH_LLM_MODEL, CEL_EVAL_USER, API keys)
#   - Running the bench with the desktop-safety guard satisfied
#
# Requires a ONE-TIME manual setup that this script can't automate:
#   1. Run `./scripts/setup-eval-user.sh` (creates the _celeval user)
#   2. Log into _celeval via "Users & Groups" → switch user, ONE TIME
#   3. In that session, grant Terminal (or iTerm) Accessibility and
#      Automation permissions (System Settings → Privacy & Security)
#   4. Log back to your primary user
#
# After that, you can run live evals from your primary user with this
# wrapper — it'll invoke the bench as _celeval in the background.

set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
REPO_ROOT="$( cd "$SCRIPT_DIR/.." && pwd )"
cd "$REPO_ROOT"

EVAL_USER="${CEL_EVAL_USER:-_celeval}"
LIMIT="${1:-2}"  # default: run 2 tasks

# Model selection priority:
#   1. Explicit second arg (e.g. `./run-macos-native-safely.sh 1 claude-sonnet-4-6`)
#   2. MACOS_NATIVE_MODEL env var (specific to this wrapper)
#   3. Hard-coded default: claude-haiku-4-5-20251001
#
# Deliberately NOT reading BENCH_LLM_MODEL — the repo's .env sets that
# to gemini-2.5-flash for the browser bench, which would silently pick
# the wrong model here. This wrapper is opinionated.
MODEL="${2:-${MACOS_NATIVE_MODEL:-claude-haiku-4-5-20251001}}"

# ── Step 1: user exists? ─────────────────────────────────────────────────────
if ! id "$EVAL_USER" >/dev/null 2>&1; then
  echo "✗ User '$EVAL_USER' does not exist."
  echo "  Run: ./scripts/setup-eval-user.sh"
  echo "  Then log into that user once to grant Accessibility + Automation perms."
  exit 1
fi

# ── Step 2: permissions? best-effort check ───────────────────────────────────
# We can't reliably check TCC from another user — just warn.
echo "→ Running macos-native bench as '$EVAL_USER'"
echo "  Model: $MODEL"
echo "  Limit: $LIMIT tasks"
echo ""
echo "  If you haven't yet logged into '$EVAL_USER' and granted Accessibility"
echo "  + Automation permissions to Terminal/Node, the bench will fail."
echo "  See docs/eval-isolation.md § 'One-time setup' for the checklist."
echo ""

# ── Step 3: grant _celeval ACL read-access to the repo path ─────────────────
# macOS home directories are usually 700 or use ACLs that block other users.
# _celeval needs to TRAVERSE the parent chain AND read/list the repo.
# ACLs are additive and narrow (this user, these paths only) — safer than
# chmod o+r on your home dir.

# Check if ACL already present to avoid re-granting on every run.
ACL_MARKER="/Users/${EVAL_USER}/.cellar-acl-granted"
if ! sudo -u "$EVAL_USER" test -f "$ACL_MARKER" 2>/dev/null; then
  echo "→ Granting ${EVAL_USER} read-access ACL to ${REPO_ROOT} (one-time, needs sudo)"

  # Traverse permission on the parent chain
  PARENT="$REPO_ROOT"
  while [[ "$PARENT" != "/" && "$PARENT" != "/Users" ]]; do
    sudo chmod +a "user:${EVAL_USER} allow search,readattr,readextattr" "$PARENT" 2>/dev/null || true
    PARENT="$(dirname "$PARENT")"
  done

  # Recursive read+list on the repo itself. This takes 1-2s on a typical
  # cellar checkout (excluding .git / node_modules / target which are huge
  # but don't need per-file ACLs — the directory ACL covers them).
  sudo chmod +a "user:${EVAL_USER} allow read,execute,list,search,readattr,readextattr,file_inherit,directory_inherit" "$REPO_ROOT"

  # Benchmark results and sandbox dirs need WRITE access for _celeval to
  # save JSON output. Scoped to just these subpaths.
  mkdir -p "$REPO_ROOT/benchmarks/results"
  sudo chmod +a "user:${EVAL_USER} allow read,write,execute,delete,list,search,add_file,add_subdirectory,file_inherit,directory_inherit" "$REPO_ROOT/benchmarks/results"

  # Mark that we've done this
  sudo -u "$EVAL_USER" touch "$ACL_MARKER"
  echo "  ✓ ACL granted (cached marker at $ACL_MARKER)"
fi

# The repo needs to be reachable by an absolute path that _celeval can cd to.
# A symlink from _celeval's home works once the ACL chain is in place.
SHARED_LINK="/Users/${EVAL_USER}/cellar"
if ! sudo -u "$EVAL_USER" test -L "$SHARED_LINK" 2>/dev/null; then
  echo "→ Creating repo symlink /Users/${EVAL_USER}/cellar → ${REPO_ROOT}"
  sudo ln -sf "$REPO_ROOT" "$SHARED_LINK"
  sudo chown -h "${EVAL_USER}:staff" "$SHARED_LINK"
fi

# ── Step 4: locate the API key ──────────────────────────────────────────────
# Read from benchmarks/.env since that's where the real keys live.
if [[ ! -f "${REPO_ROOT}/benchmarks/.env" ]]; then
  echo "✗ benchmarks/.env missing — provision it with your API keys first."
  exit 1
fi

# ── Step 5: run as the eval user ────────────────────────────────────────────
# sudo -u switches identity; -H sets HOME; -E would leak env, so we pass
# only the env vars we need explicitly.
#
# The CELLAR_ACK_DESKTOP_KEYS gate is OK here because we ARE in an isolated
# user session — stray keystrokes land in _celeval's empty desktop.
echo "→ Starting bench..."
# Use the ABSOLUTE path to avoid any chdir resolution issues through the
# symlink. The ACL we granted above makes this readable.
sudo -u "$EVAL_USER" -H env \
  BENCH_LLM_MODEL="$MODEL" \
  CEL_EVAL_USER="$EVAL_USER" \
  CELLAR_ACK_DESKTOP_KEYS=1 \
  HOME="/Users/${EVAL_USER}" \
  PATH="$PATH" \
  bash -c "cd '${REPO_ROOT}/benchmarks' && npx tsx src/standard/osworld/macos-native/runner.ts --limit ${LIMIT}"

echo ""
echo "→ Bench complete. Results: ${REPO_ROOT}/benchmarks/results/"
