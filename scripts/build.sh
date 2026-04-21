#!/usr/bin/env bash
set -euo pipefail

# Ensure cargo and pnpm are in PATH
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

# ─── Cellar Build Script ─────────────────────────────────────────────────────
#
# Builds everything needed for development and distribution:
#   1. Rust workspace (cel-cortex, cel-napi, all crates)
#   2. NAPI native binary (.node file)
#   3. TypeScript packages (agent, mcp-server)
#   4. Optionally: Tauri app (.dmg)
#
# Usage:
#   ./scripts/build.sh           # Build everything (dev)
#   ./scripts/build.sh release   # Build everything (release + Tauri .dmg)
#   ./scripts/build.sh napi      # Build just the NAPI binary
#   ./scripts/build.sh test      # Build + run all tests
#
# Prerequisites:
#   - Rust toolchain (rustup)
#   - Node.js >= 20
#   - pnpm
#   - macOS (for accessibility APIs)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
cd "$ROOT_DIR"

MODE="${1:-dev}"

echo "=== Cellar Build ($MODE) ==="
echo ""

# ─── Step 1: Rust workspace ─────────────────────────────────────────────────

echo "▸ Building Rust workspace..."
if [ "$MODE" = "release" ]; then
    cargo build --release --workspace 2>&1 | tail -3
else
    cargo build --workspace 2>&1 | tail -3
fi
echo "  ✓ Rust workspace built"

# ─── Step 2: NAPI native binary ─────────────────────────────────────────────

echo ""
echo "▸ Building NAPI binary..."
if [ "$MODE" = "release" ]; then
    cargo build --release -p cel-napi
    cp target/release/libcel_napi.dylib cel/cel-napi/cel-napi.darwin-arm64.node
else
    cargo build --release -p cel-napi  # NAPI always release for performance
    cp target/release/libcel_napi.dylib cel/cel-napi/cel-napi.darwin-arm64.node
fi
echo "  ✓ NAPI binary: cel/cel-napi/cel-napi.darwin-arm64.node ($(du -h cel/cel-napi/cel-napi.darwin-arm64.node | cut -f1))"

# ─── Step 3: TypeScript packages ────────────────────────────────────────────

echo ""
echo "▸ Building TypeScript packages..."
pnpm install --frozen-lockfile 2>/dev/null || pnpm install
cd agent && pnpm build 2>&1 && cd ..
cd mcp-server && pnpm build 2>&1 && cd ..
echo "  ✓ TypeScript packages built"

# ─── Step 4: Tests (if requested) ───────────────────────────────────────────

if [ "$MODE" = "test" ]; then
    echo ""
    echo "▸ Running Rust tests..."
    cargo test -p cel-cortex 2>&1 | tail -3
    echo ""
    echo "▸ Running NAPI smoke test..."
    node tests/cortex/napi-smoke.mjs 2>&1 | tail -3
    echo ""
    echo "▸ Running MCP protocol test..."
    node tests/cortex/mcp-protocol.mjs 2>&1 | tail -3
    echo ""
    echo "▸ TypeScript type check..."
    cd agent && npx tsc --noEmit && echo "  ✓ agent types OK" && cd ..
    cd mcp-server && npx tsc --noEmit && echo "  ✓ mcp-server types OK" && cd ..
fi

# ─── Summary ─────────────────────────────────────────────────────────────────

echo ""
echo "=== Build Complete ==="
echo ""
echo "To use the MCP server with Claude:"
echo "  Add to Claude MCP config:"
echo "  {"
echo "    \"cellar\": {"
echo "      \"command\": \"node\","
echo "      \"args\": [\"$ROOT_DIR/mcp-server/dist/index.js\"]"
echo "    }"
echo "  }"
