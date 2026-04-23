# Contributing to Cellar

We welcome contributions of all kinds. This guide helps you get started.

## Where to Start

**Good first contributions:**

| Area | Difficulty | What to do |
|------|-----------|------------|
| Documentation | Easy | Improve docs, add examples, fix typos |
| Test coverage | Easy | Add tests for existing code (see `docs/TEST_INVENTORY.md`) |
| Bug reports | Easy | File issues with reproduction steps |
| Adapters | Medium | Build a new adapter for an application you use (see [docs/building-adapters.md](docs/building-adapters.md)) |
| macOS accessibility | Medium-Hard | Implement AXUIElement bridge in `cel-accessibility/` |
| Windows accessibility | Medium-Hard | Implement UI Automation bridge in `cel-accessibility/` |
| MCP tools | Medium | Add features to existing MCP tools or propose new ones |

## Development Setup

### Prerequisites

- **Node.js 20+** and **pnpm 9+** (TypeScript packages, MCP server, CLI)
- **Rust 1.75+** (optional — for CEL core, accessibility bridge, native bindings)
- **Linux:** `libatspi2.0-dev` for accessibility support

### Build

```bash
git clone https://github.com/dimpagk92/cellar.git
cd cellar

# TypeScript only (works without Rust)
pnpm install && pnpm -r build

# Full build (Rust + TypeScript)
make build
```

### Test

```bash
make test              # All tests
cargo test             # Rust unit tests
pnpm test              # TypeScript tests
```

See [DEVELOPMENT.md](DEVELOPMENT.md) for full build commands and conventions.

## Project Structure

```
cellar/
  cel/                  Rust core (Apache 2.0)
    cel-context/        Unified context API, fusion engine, references
    cel-accessibility/  Platform accessibility bridges
    cel-display/        Screen capture
    cel-input/          Mouse/keyboard injection
    cel-vision/         Vision model integration
    cel-network/        Traffic monitoring
    cel-store/          SQLite storage (memory, knowledge)
    cel-llm/            LLM provider abstraction
    cel-planner/        Observe-plan-act loop
    cel-napi/           Node.js native bindings (napi-rs)
  mcp-server/           MCP server (4 tools)
  agent/                Workflow execution engine (TypeScript)
  cli/                  CLI tool
  adapters/             Application-specific adapters
  recorder/             Training/recording system
  live-view/            Screen streaming server
  registry/             Community workflow registry (planned)
  docs/                 Documentation
  benchmarks/           Performance benchmarks
```

## Submitting Changes

1. **Fork and branch.** Create a feature branch from `main`.
2. **Keep commits focused.** One logical change per commit.
3. **Add tests.** If you're changing behavior, add or update tests.
4. **Follow conventions.**
   - Rust: platform code in `windows.rs`/`macos.rs`/`linux.rs` behind `#[cfg(target_os)]`
   - TypeScript: strict mode, ES2022 target
   - CEL crates prefixed `cel-`, TypeScript packages scoped `@cellar/`
5. **Update docs.** If you're adding a feature, update the relevant docs.
6. **Open a PR.** Describe what you changed and why. Link to the issue if there is one.

## What We're Looking For

### Accessibility Bridges

The biggest impact area. CEL currently has a working AT-SPI2 bridge for Linux. macOS and Windows are stubbed:

- **macOS:** `cel-accessibility/src/macos.rs` — needs AXUIElement via the Accessibility framework
- **Windows:** `cel-accessibility/src/windows.rs` — needs UI Automation (UIA) via the `uiautomation` crate

The interface is defined in `cel-accessibility/src/lib.rs`. Implement the `AccessibilityTree` trait for your platform.

### Adapters

Application-specific adapters that use native APIs for higher accuracy than accessibility alone. See [docs/building-adapters.md](docs/building-adapters.md).

### MCP Server

The MCP server (`mcp-server/`) is new. Ideas for improvement:
- Add MCP Resources (e.g., expose screenshots as resources)
- Add MCP Prompts (predefined prompt templates for common tasks)
- Improve tool descriptions for better LLM understanding
- Add SSE/Streamable HTTP transport for remote connections

## Conventions

### Rust

- Format: `cargo fmt`
- Lint: `cargo clippy`
- Platform-specific code behind `#[cfg(target_os = "...")]`
- All public types derive `Serialize, Deserialize`
- Crate names: `cel-{name}`

### TypeScript

- Strict mode, ES2022 target
- Package names: `@cellar/{name}`
- Build: `tsc` (no bundler)
- Test: `vitest`

## License

By contributing, you agree that your contributions will be licensed under:
- **Everything OSS-destined** (`cel/`, `agent/`, `cli/`, `mcp-server/`, `cellar-worker/`, `live-view/`, `recorder/`, `registry/`, `docs/`, `benchmarks/`, `examples/`, `e2e/`, `tests/`, `box/`): Apache License 2.0
- **Community adapters** (`adapters/`): MIT License
