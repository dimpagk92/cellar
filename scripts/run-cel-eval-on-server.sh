#!/usr/bin/env bash
#
# Run the cel-eval scenarios suite on the dedicated Hetzner benchmark
# server (204.168.232.124) against Gemini.
#
# Why this script:
#   Local cel-eval runs against Sonnet 4.5 are expensive and slow. The
#   server has Rust + Chrome installed and a Gemini API key in
#   `/opt/cellar/benchmarks/.env`, so production-quality measurements
#   can move off the MacBook and onto cheap Gemini-flash calls.
#
# Usage:
#   ./scripts/run-cel-eval-on-server.sh                          # defaults
#   SCENARIOS_DIR=eval/prototype-subset ./scripts/run-cel-eval-on-server.sh
#   CEL_MODEL=gemini-3-pro TRIALS=3 ./scripts/run-cel-eval-on-server.sh
#   SKIP_DEPLOY=1 ./scripts/run-cel-eval-on-server.sh            # rerun without rsync
#   SKIP_BUILD=1 ./scripts/run-cel-eval-on-server.sh             # reuse last build
#   PULL_ONLY=1 ./scripts/run-cel-eval-on-server.sh              # just fetch results back
#
# Environment overrides (sensible defaults below):
#   SERVER          (default 204.168.232.124)
#   SERVER_USER     (default root)
#   REMOTE_PATH     (default /opt/cellar)
#   SCENARIOS_DIR   (default eval/scenarios — full suite, 40 scenarios)
#   TRIALS          (default 2)
#   CEL_MODEL       (default gemini-3-flash)
#   CEL_LLM_PROVIDER (default gemini)
#   LOCAL_RESULTS_DIR (default results/server-runs)
#
# What the script does, in order:
#   1. Rsync local cellar source → /opt/cellar on the server, preserving
#      the server's `benchmarks/.env` (which carries the API keys).
#   2. Build cel-eval --release --features live on the server.
#   3. Kill any prior fixture server / headless Chrome left over from a
#      previous run, then start fresh background instances.
#   4. Run cel-eval with the requested provider/model/trials, tee'ing
#      stdout+stderr to a timestamped log under /opt/cellar/results.
#   5. Rsync results back to LOCAL_RESULTS_DIR locally.
#   6. Tear down the background Chrome + fixture server.
#
# Exit codes:
#   0 — eval ran and results were pulled back
#   non-zero — something failed; the script prints WHICH step and stops
#
# Idempotency: re-running just overwrites results. Background Chrome /
# Python http.server processes are killed by name before launch so
# repeated runs don't pile up stray processes.

set -euo pipefail

# ── Config ──────────────────────────────────────────────────────────────────

SERVER="${SERVER:-204.168.232.124}"
SERVER_USER="${SERVER_USER:-root}"
REMOTE_PATH="${REMOTE_PATH:-/opt/cellar}"
SCENARIOS_DIR="${SCENARIOS_DIR:-eval/scenarios}"
TRIALS="${TRIALS:-2}"
CEL_MODEL="${CEL_MODEL:-gemini-3-flash}"
CEL_LLM_PROVIDER="${CEL_LLM_PROVIDER:-gemini}"
LOCAL_RESULTS_DIR="${LOCAL_RESULTS_DIR:-results/server-runs}"
SKIP_DEPLOY="${SKIP_DEPLOY:-0}"
SKIP_BUILD="${SKIP_BUILD:-0}"
PULL_ONLY="${PULL_ONLY:-0}"

# Derived values
RUN_STAMP="$(date +%Y%m%d-%H%M%S)"
REMOTE_LOG="${REMOTE_PATH}/results/cel-eval-server-${RUN_STAMP}.log"
SSH_TARGET="${SERVER_USER}@${SERVER}"

log() { printf "\033[36m[%s]\033[0m %s\n" "$(date +%H:%M:%S)" "$*"; }
die() { printf "\033[31m[%s] ERROR\033[0m: %s\n" "$(date +%H:%M:%S)" "$*" >&2; exit 1; }

# ── 0. Pre-flight ───────────────────────────────────────────────────────────

log "Pre-flight: confirming SSH reachability and remote toolchain"
ssh -o ConnectTimeout=10 "${SSH_TARGET}" \
    'test -f $HOME/.cargo/env && which google-chrome && which python3 >/dev/null' \
    || die "Remote pre-flight failed — Rust toolchain or Chrome missing on ${SERVER}"

if [ "${PULL_ONLY}" = "1" ]; then
    log "PULL_ONLY=1 set — skipping all steps except results pull"
    mkdir -p "${LOCAL_RESULTS_DIR}"
    rsync -avz "${SSH_TARGET}:${REMOTE_PATH}/results/" "${LOCAL_RESULTS_DIR}/"
    log "Done. Latest results at ${LOCAL_RESULTS_DIR}/"
    exit 0
fi

# ── 1. Deploy source ────────────────────────────────────────────────────────

if [ "${SKIP_DEPLOY}" = "1" ]; then
    log "SKIP_DEPLOY=1 — using whatever's already on the server"
else
    log "Step 1/5: rsync local source → ${SERVER}:${REMOTE_PATH}"
    # `--exclude='/benchmarks/.env'` preserves the server's API keys.
    # `--exclude='/target'` skips the (huge, useless on a different OS)
    # local build cache; the server has its own incremental cache.
    # `--exclude='/results'` keeps prior runs' results intact (we
    #   may want to compare across runs).
    # `--exclude='/.git'` — server is rsync-deploy, not git clone.
    # NOTE: macOS rsync 2.6.9 (the system default) doesn't support
    # `--info=stats1` — modern rsync flag. Plain `-az --delete` works
    # on both macOS and Linux rsync.
    rsync -az --delete \
        --exclude='/target' \
        --exclude='/node_modules' \
        --exclude='/results' \
        --exclude='/.git' \
        --exclude='/benchmarks/.env' \
        --exclude='/benchmarks/server/results' \
        --exclude='*.log' \
        ./ "${SSH_TARGET}:${REMOTE_PATH}/" || die "rsync failed"
fi

# ── 2. Build cel-eval ───────────────────────────────────────────────────────

if [ "${SKIP_BUILD}" = "1" ]; then
    log "SKIP_BUILD=1 — using existing target/release/cel-eval on server"
    ssh "${SSH_TARGET}" "test -x ${REMOTE_PATH}/target/release/cel-eval" \
        || die "SKIP_BUILD set but no cel-eval binary found on server"
else
    log "Step 2/5: build cel-eval --release --features live (this can take 10-20 min cold)"
    # `source ~/.cargo/env` because ssh non-login shells don't pick up
    # the cargo bin path the Rust installer added to .bashrc.
    ssh "${SSH_TARGET}" "
        set -e
        cd ${REMOTE_PATH}
        source \$HOME/.cargo/env
        cargo build --release --features live -p cel-eval
    " || die "cargo build failed on server"
fi

# ── 3. Verify Gemini key + Chrome / fixtures ────────────────────────────────

log "Step 3/5: confirm GEMINI_API_KEY present, prep Chrome + fixture server"
# Bootstrap script written to the server, then run as a separate
# SSH invocation. Avoids the nested-quoting + "SSH-doesn't-disconnect-
# when-children-keep-fds-open" trap that inline heredocs hit
# (backticks in comments got interpreted locally; nohup/setsid
# children kept stdout/stderr fds open and SSH never returned).
ssh "${SSH_TARGET}" "cat > /tmp/cel-eval-setup.sh" <<'BOOTSTRAP'
#!/usr/bin/env bash
set -e
cd /opt/cellar
grep -qE '^(GEMINI_API_KEY|GOOGLE_GEMINI_API_KEY|GOOGLE_API_KEY)=' benchmarks/.env \
    || { echo 'NO GEMINI KEY IN benchmarks/.env'; exit 1; }
pkill -f 'google-chrome.*remote-debugging-port=9333' 2>/dev/null || true
pkill -f 'python3 -m http.server 4567' 2>/dev/null || true
sleep 1
rm -rf /tmp/cel-eval-chrome
# nohup + setsid + full FD detachment so SSH can disconnect cleanly.
# Without redirecting all three FDs to /dev/null the SSH session
# keeps a handle to the child's stdio and the connection hangs.
setsid nohup python3 -m http.server 4567 --directory benchmarks/fixtures \
    </dev/null >/tmp/fixtures.log 2>/tmp/fixtures.err &
setsid nohup google-chrome --headless=new --disable-gpu --no-sandbox \
    --remote-debugging-port=9333 --user-data-dir=/tmp/cel-eval-chrome about:blank \
    </dev/null >/tmp/chrome.log 2>/tmp/chrome.err &
# Wait up to 15s each for Chrome's CDP /json/version AND the fixture
# server. Separate loops because Chrome and python http.server have
# different startup curves — Chrome is usually 3-8s while
# http.server is sub-second, but if Chrome was already running from
# a prior session the Chrome loop exits immediately and we'd miss
# the fixture coming up.
for _ in $(seq 1 15); do
    if curl -sf http://localhost:9333/json/version >/dev/null 2>&1; then
        break
    fi
    sleep 1
done
curl -sf http://localhost:9333/json/version >/dev/null \
    || { echo 'Chrome CDP /json/version not reachable on 9333 after 15s'; exit 1; }
for _ in $(seq 1 15); do
    if curl -sf http://localhost:4567/simple-form.html >/dev/null 2>&1; then
        break
    fi
    sleep 1
done
curl -sf http://localhost:4567/simple-form.html >/dev/null \
    || { echo 'Fixture server not reachable on 4567 after 15s'; exit 1; }
echo STEP3_OK
BOOTSTRAP
ssh "${SSH_TARGET}" 'bash /tmp/cel-eval-setup.sh' \
    || die "Step 3 setup failed — check /tmp/chrome.log /tmp/chrome.err on the server"

# OLD inline version intentionally retained below as a guard against
# the issue recurring — see the bootstrap above for the live path.
DISABLED_INLINE=1
if [ "${DISABLED_INLINE}" = "_NEVER_" ]; then
ssh "${SSH_TARGET}" "
    set -e
    cd ${REMOTE_PATH}
    # Check the .env carries a Gemini key.
    grep -qE '^(GEMINI_API_KEY|GOOGLE_GEMINI_API_KEY|GOOGLE_API_KEY)=' benchmarks/.env \
        || { echo 'NO GEMINI KEY IN benchmarks/.env'; exit 1; }
    # Kill any leftover Chrome / fixture-server from prior runs.
    pkill -f 'google-chrome.*remote-debugging-port=9333' 2>/dev/null || true
    pkill -f 'python3 -m http.server 4567' 2>/dev/null || true
    # Fresh Chrome user-data-dir each run — cookies / localStorage
    # from a prior trial shouldn't pollute the next.
    rm -rf /tmp/cel-eval-chrome
    # Background fixture server + headless Chrome via setsid. Over
    # SSH a plain backgrounded process can be killed when the SSH
    # connection closes (the HUP signal still propagates to the
    # child group on some configurations). setsid puts the process
    # in its own session, fully detached from the SSH process group.
    # (Avoid backticks in this comment block: the whole SSH command
    # is double-quoted locally, and bash would interpret backticks
    # as command substitution BEFORE sending to the remote.)
    setsid python3 -m http.server 4567 --directory benchmarks/fixtures \
        </dev/null >/tmp/fixtures.log 2>&1 &
    setsid google-chrome --headless=new --disable-gpu --no-sandbox \
        --remote-debugging-port=9333 \
        --user-data-dir=/tmp/cel-eval-chrome \
        about:blank \
        </dev/null >/tmp/chrome.log 2>&1 &
    # Disown so this shell exits cleanly without waiting on them.
    disown -a 2>/dev/null || true
    # Poll for Chrome's /json/version up to 15s instead of a fixed
    # sleep. On a cold Hetzner CPX42 Chrome takes ~4-5s to write its
    # DevToolsActivePort file; a fixed `sleep 3` raced and the curl
    # below fired before Chrome was ready.
    for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
        if curl -sf http://localhost:9333/json/version >/dev/null 2>&1; then
            break
        fi
        sleep 1
    done
    # Sanity: Chrome's /json/version endpoint reachable?
    curl -sf http://localhost:9333/json/version >/dev/null \
        || { echo 'Chrome CDP /json/version not reachable on 9333 after 15s'; exit 1; }
    # Sanity: fixture server reachable?
    curl -sf http://localhost:4567/simple-form.html >/dev/null \
        || { echo 'Fixture server not reachable on 4567'; exit 1; }
" || die "Step 3 setup failed — check /tmp/chrome.log and /tmp/fixtures.log on the server"
fi  # end DISABLED_INLINE guard

# ── 4. Run cel-eval ─────────────────────────────────────────────────────────

log "Step 4/5: run cel-eval on server (scenarios=${SCENARIOS_DIR}, trials=${TRIALS}, model=${CEL_MODEL})"
log "Log will tee to ${REMOTE_LOG}"
# IMPORTANT: this can take a long time (full eval/scenarios at trials=2
# was ~1h 44m on Sonnet; Gemini Flash should be faster — single-shot is
# typically <1s vs Sonnet's ~10s — but full suite is still ~30-60 min).
ssh "${SSH_TARGET}" "
    set -e
    cd ${REMOTE_PATH}
    mkdir -p results
    set -a; source benchmarks/.env; set +a
    export CEL_CDP_PORT=9333
    export CEL_LLM_PROVIDER='${CEL_LLM_PROVIDER}'
    export CEL_MODEL='${CEL_MODEL}'
    ./target/release/cel-eval scenarios \
        --dir '${SCENARIOS_DIR}' \
        --trials ${TRIALS} \
        --exclude-tag desktop \
        --live \
        --allow-foreground-leak \
        --runtime canonical \
        --out results \
        --format md \
        --gate-threshold 0 \
        2>&1 | tee '${REMOTE_LOG}'
" || die "cel-eval run failed — check ${REMOTE_LOG} on server"
# Why --exclude-tag desktop: the Hetzner server is Ubuntu and has
# no AX tree (only the stub) + no AppleScript bridge, so scenarios
# tagged 'desktop' (Numbers/Mail/Messages/Calendar/Reminders) cannot
# pass and would polute the pass-rate denominator. The same suite
# run on macOS would omit --exclude-tag and exercise those scenarios
# against real AX. See cel/cel-eval/src/bin/cel_eval.rs for the
# flag's wiring and the loader's tag-exclusion behaviour.

# ── 5. Pull results, tear down ──────────────────────────────────────────────

log "Step 5/5: rsync results back to ${LOCAL_RESULTS_DIR}, tear down Chrome + fixture"
mkdir -p "${LOCAL_RESULTS_DIR}"
rsync -az "${SSH_TARGET}:${REMOTE_PATH}/results/" "${LOCAL_RESULTS_DIR}/" \
    || die "results rsync back failed"

# Best-effort teardown — don't fail the script if these don't find anything.
ssh "${SSH_TARGET}" "
    pkill -f 'google-chrome.*remote-debugging-port=9333' 2>/dev/null || true
    pkill -f 'python3 -m http.server 4567' 2>/dev/null || true
" || true

log "Done."
log "Latest eval report:   $(ls -t ${LOCAL_RESULTS_DIR}/eval-*.md 2>/dev/null | head -1)"
log "Server-run full log:  ${LOCAL_RESULTS_DIR}/cel-eval-server-${RUN_STAMP}.log"
