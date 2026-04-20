# Cellar Tests

## Test Suites

### Rust Unit Tests
```bash
cargo test --workspace       # All Rust crates
cargo test -p cel-cortex     # Cortex only (40 tests)
```

### TypeScript Unit Tests
```bash
pnpm test                    # All packages (vitest)
```

### Cortex Integration Tests

**Prerequisites**: macOS with accessibility permissions granted to your terminal.

```bash
# NAPI smoke test — verifies Rust Cortex via native bindings (28 tests)
node tests/cortex/napi-smoke.mjs

# MCP protocol test — full cel_perceive lifecycle via JSON-RPC (14 tests)
node tests/cortex/mcp-protocol.mjs
```

### E2E Tests
```bash
cd e2e && npx playwright test    # All Playwright suites
```

### Run Everything
```bash
make test-all    # Rust + TypeScript + Cortex NAPI + Cortex MCP
```

## Makefile Targets

| Target | What it runs |
|--------|-------------|
| `make test` | Rust + TypeScript unit tests |
| `make test-all` | Everything (Rust + TS + Cortex) |
| `make test-cortex` | Rust Cortex unit tests (40) |
| `make test-cortex-napi` | NAPI binding smoke tests (28) |
| `make test-cortex-mcp` | MCP protocol end-to-end (14) |
| `make test-e2e` | Playwright E2E tests |

## CI

GitHub Actions runs on every push/PR to `main`:
- Rust check + tests (macOS, Windows, Linux)
- TypeScript build + lint
- NAPI binary build
- Cortex tests (macOS only — requires accessibility APIs)
- E2E Playwright tests
