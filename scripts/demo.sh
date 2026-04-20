#!/usr/bin/env bash
set -euo pipefail

# ─────────────────────────────────────────────────────────────────────────────
# CEL Hybrid Runtime Demo
#
# Runs the 5 hybrid benchmark scenarios that showcase where CEL's
# a11y+CDP+vision fusion beats screenshot-only agents.
#
# Usage:
#   ./scripts/demo.sh              # Run full demo (CEL + Computer Use comparison)
#   ./scripts/demo.sh --cel-only   # Run only CEL (faster, no comparison)
#   ./scripts/demo.sh --task NAME  # Run a single hybrid task
# ─────────────────────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BENCH_DIR="$ROOT_DIR/benchmarks"

# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color
BOLD='\033[1m'

echo -e "${BOLD}╔══════════════════════════════════════════════════════╗${NC}"
echo -e "${BOLD}║       CEL Hybrid Runtime Demo                       ║${NC}"
echo -e "${BOLD}║  \"We handle the workflows that break when all       ║${NC}"
echo -e "${BOLD}║   you have is screenshots.\"                         ║${NC}"
echo -e "${BOLD}╚══════════════════════════════════════════════════════╝${NC}"
echo ""

# Parse args
CEL_ONLY=false
TASK_FILTER=""
RUNS=3

for arg in "$@"; do
  case $arg in
    --cel-only) CEL_ONLY=true ;;
    --task) shift; TASK_FILTER="$1" ;;
    --runs) shift; RUNS="$1" ;;
  esac
  shift 2>/dev/null || true
done

cd "$BENCH_DIR"

# ── Step 1: Start live-view in background ──────────────────────────────────
echo -e "${BLUE}[1/4]${NC} Starting live-view server..."
if command -v cellar &>/dev/null; then
  cellar live-view --port 6080 &
  LIVEVIEW_PID=$!
  echo -e "  ${GREEN}Live view:${NC} http://127.0.0.1:6080"
  echo -e "  ${YELLOW}Open this URL to watch runtime decisions in real-time${NC}"
  sleep 2
else
  echo -e "  ${YELLOW}cellar CLI not found — skipping live-view${NC}"
  echo -e "  (Run 'pnpm build' in the root to build the CLI)"
  LIVEVIEW_PID=""
fi
echo ""

# ── Step 2: Run CEL on hybrid scenarios ────────────────────────────────────
echo -e "${BLUE}[2/4]${NC} Running CEL on hybrid scenarios (${RUNS} runs each)..."
echo ""

CELLAR_ARGS="--tool cellar --category hybrid --runs $RUNS"
if [ -n "$TASK_FILTER" ]; then
  CELLAR_ARGS="--tool cellar --task $TASK_FILTER --runs $RUNS"
fi

npx tsx src/harness.ts $CELLAR_ARGS

echo ""

# ── Step 3: Run Computer Use comparison (optional) ─────────────────────────
if [ "$CEL_ONLY" = false ]; then
  echo -e "${BLUE}[3/4]${NC} Running Computer Use (screenshot-only) for comparison..."
  echo ""

  CU_ARGS="--tool computer-use --category hybrid --runs $RUNS"
  if [ -n "$TASK_FILTER" ]; then
    CU_ARGS="--tool computer-use --task $TASK_FILTER --runs $RUNS"
  fi

  npx tsx src/harness.ts $CU_ARGS || echo -e "  ${YELLOW}Computer Use run had errors (expected for some hybrid scenarios)${NC}"
  echo ""
else
  echo -e "${BLUE}[3/4]${NC} ${YELLOW}Skipping Computer Use comparison (--cel-only)${NC}"
  echo ""
fi

# ── Step 4: Generate report ────────────────────────────────────────────────
echo -e "${BLUE}[4/4]${NC} Generating benchmark report..."
npx tsx src/reporter.ts
echo ""

echo -e "${BOLD}╔══════════════════════════════════════════════════════╗${NC}"
echo -e "${BOLD}║  Demo complete!                                     ║${NC}"
echo -e "${BOLD}╚══════════════════════════════════════════════════════╝${NC}"
echo ""
echo -e "  ${GREEN}Results:${NC}  benchmarks/results/run-$(date +%Y-%m-%d).json"
echo -e "  ${GREEN}Report:${NC}   benchmarks/BENCHMARKS.md"
if [ -n "$LIVEVIEW_PID" ]; then
  echo -e "  ${GREEN}Live view:${NC} http://127.0.0.1:6080 (still running, PID $LIVEVIEW_PID)"
  echo -e "  ${YELLOW}Press Ctrl+C or kill $LIVEVIEW_PID to stop live-view${NC}"
fi
echo ""
echo -e "${BOLD}Key metrics to look at in the report:${NC}"
echo -e "  - ${GREEN}semanticRoutes${NC}     — where a11y tree disambiguated elements"
echo -e "  - ${GREEN}staleRecoveries${NC}    — where freshness model saved the action"
echo -e "  - ${GREEN}sideEffectWarnings${NC} — where side effects were caught"
echo -e "  - ${RED}terminalFailures${NC}   — where CEL stopped instead of looping"
echo -e "  - ${BLUE}successRate${NC}        — head-to-head pass rate vs screenshot-only"
