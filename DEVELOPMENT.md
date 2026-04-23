# Cellar — Development Guide

## Project Structure

- **Rust workspace** at repo root (`Cargo.toml`) — all `cel/` crates and `adapters/`
- **TypeScript monorepo** via pnpm workspace — `agent/`, `mcp-server/`, `recorder/`, `live-view/`, `registry/`, `cli/`
- Rust <-> TypeScript bridge via **napi-rs** (`cel/cel-napi/`)

### Package Overview

| Package | Language | Description |
|---------|----------|-------------|
| `cel-context` | Rust | Unified context API, fusion engine, element references |
| `cel-accessibility` | Rust | Platform accessibility bridges (AT-SPI2, AXUIElement) |
| `cel-display` | Rust | Screen capture via xcap |
| `cel-input` | Rust | Mouse/keyboard injection via enigo |
| `cel-vision` | Rust | Vision model integration (multi-provider) |
| `cel-network` | Rust | Network traffic monitoring |
| `cel-store` | Rust | SQLite storage (memory, knowledge, FTS5) |
| `cel-llm` | Rust | LLM provider abstraction |
| `cel-planner` | Rust | Observe-plan-act loop |
| `cel-napi` | Rust | Node.js native bindings (napi-rs) |
| `@cellar/agent` | TypeScript | Workflow execution engine, Cel class, TypeScript types |
| `@dpagk/cellar-mcp` | TypeScript | MCP server (4 tools: context, action, observe, knowledge) |
| `@cellar/cli` | TypeScript | `cellar` CLI tool |
| `@cellar/recorder` | TypeScript | Training: passive observation + explicit recording |
| `@cellar/live-view` | TypeScript | Screen streaming server (WebSocket + SSE) |
| `@cellar/registry` | TypeScript | Community workflow registry client |

## Build Commands

```bash
make build          # Build everything (Rust + TypeScript)
make build-rust     # cargo build --workspace
make build-ts       # pnpm install && pnpm -r build
make test           # Run all tests
make lint           # Lint Rust + TypeScript
make clean          # Clean all build artifacts
```

### Building individual packages

```bash
# Rust
cargo build -p cel-context       # Single crate
cargo test -p cel-context        # Test a crate

# TypeScript
pnpm --filter @cellar/agent build
pnpm --filter @dpagk/cellar-mcp build
pnpm --filter @cellar/cli build
```

### Building the native module

The napi-rs bridge must be compiled for your platform:

```bash
cargo build -p cel-napi
```

This produces a `.node` file that `@cellar/agent` loads at runtime. Without it, the Cel class returns mock data (useful for TypeScript-only development).

## Conventions

### Rust

- Platform-specific code in `windows.rs` / `macos.rs` / `linux.rs` behind `#[cfg(target_os)]`
- All public types derive `Serialize, Deserialize` (for JSON serialization through napi)
- All CEL crate names prefixed with `cel-` (e.g., `cel-display`, `cel-input`)
- Format: `cargo fmt` | Lint: `cargo clippy`

### TypeScript

- Strict mode, ES2022 target
- All packages scoped under `@cellar/` (e.g., `@cellar/agent`, `@cellar/cli`)
- Build: `tsc` (no bundler)
- Test: `vitest`
- Adapters use the `AdapterTrait` from `adapter-common`

## Key Architecture

### Context Fusion

The unified context API (`cel-context`) merges 5 streams with a priority hierarchy:

1. **Native API** (highest) — deterministic adapter data
2. **Accessibility tree** — structured platform data
3. **Vision** (fallback) — triggered when < 5 actionable elements from accessibility
4. **Network** (supplementary) — connection state signals

Every element has a confidence score (0.0-1.0) and source attribution.

### Data Flow

```
Agent (or MCP client)
  ↓ calls getContext()
Cel class (@cellar/agent)
  ↓ calls cel-napi binding
cel-napi (Rust → Node.js bridge)
  ↓ calls ContextMerger
cel-context (fusion engine)
  ├── cel-accessibility → AT-SPI2 / AXUIElement tree
  ├── cel-display → screen capture (for vision fallback)
  ├── cel-vision → LLM vision analysis (when needed)
  └── cel-network → connection events
  ↓ returns ScreenContext
JSON → TypeScript → Agent
```

### Context References

Elements are identified by ephemeral IDs that change per snapshot. Context references (`ContextReference`) provide resilient identification through multiple signals:

- Element type (exact match, weight 0.3)
- Label (fuzzy match, weight 0.3)
- Ancestor path (prefix match, weight 0.2)
- Bounds region (coarse spatial, weight 0.1)
- Value pattern (content match, weight 0.1)

Resolution code: `cel-context/src/resolve.rs`

### MCP Server

The MCP server (`mcp-server/`) is a thin wrapper around the Cel class:

```
MCP Client (Claude Desktop, Cursor)
  ↓ MCP protocol (stdio)
mcp-server/src/index.ts
  ↓ tool handlers
tools/cel-context.ts → cel.getContext()
tools/cel-action.ts  → cel.click(), cel.typeText(), etc.
tools/cel-observe.ts → polling loop over cel.getContext()
tools/cel-knowledge.ts → cel.searchKnowledge(), etc.
```

The server creates a single Cel instance at startup. All tool handlers share it.

## Useful Commands

```bash
# First-run setup (interactive — picks LLM provider, writes ~/.cellar/config.toml)
cellar init

# Configure macOS AX + CDP permissions
cellar setup

# See what the agent sees right now
cellar context --json

# Watch context changes in real-time
cellar context --watch

# Test a single action
cellar action click 500 300

# Start MCP server for testing
cellar mcp

# Get Claude Desktop config
cellar mcp install
```

## LLM Provider Configuration

Resolution order for any field (provider, model, endpoint, api_key):

1. Role-specific env var — e.g. `CEL_LLM_PLANNER_MODEL` (roles: `Planner`, `Observer`, `Vision`, `General`, `Validator`, `Localizer`, `Orchestrator`)
2. Base env var — e.g. `CEL_LLM_MODEL`
3. Provider-specific env var — `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `HUGGINGFACE_API_KEY`
4. `~/.cellar/config.toml` `[llm]` section (written by `cellar init`)
5. Provider defaults (e.g. `gemini-2.5-flash` for Gemini, `gemma4:e4b` for Ollama)

Supported providers: `openai`, `anthropic`, `gemini`, `huggingface`, `ollama`, `custom`. Ollama defaults to `http://localhost:11434/v1/chat/completions` + `gemma4:e4b` — no API key needed. Any OpenAI-compatible endpoint works via `custom`.

See `cel/cel-llm/src/config.rs` for the full resolution logic and [docs/deployment.md](docs/deployment.md) for per-role routing patterns.
