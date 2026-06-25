# CEL OSS

**CEL is the open context and trust data plane for AI agents.**

CEL gives agent builders reusable contracts for four things every serious agent
runtime needs:

- fuse many sources into one canonical context snapshot
- persist cross-turn memory locally or behind your own backend
- assemble governed model briefs with receipts
- inspect what the agent saw, remembered, sent to the model, and later claimed

The open project is intentionally **not** the full Cellar runtime. The live
cortex engine, policy enforcement, monitoring, compliance workflows, hosted
workers, and GUI surfaces are the commercial Cellar/Dilipod operating layer
built on these contracts.

## OSS Crates

Each crate is published on [crates.io](https://crates.io/) and maintained in a
standalone repository. This workspace (`cellar-oss`) mirrors them for integrated
examples and shared docs.

| Crate | Repository | Role | Start here |
|---|---|---|---|
| [`cel-context`](https://github.com/dimpagk92/cel-context) | [crates.io](https://crates.io/crates/cel-context) | `ContextElement` / `ContextSnapshot` model and merge mechanics | [docs/concepts/context.md](docs/concepts/context.md) |
| [`cel-memory`](https://github.com/dimpagk92/cel-memory) | [crates.io](https://crates.io/crates/cel-memory) | Memory trait, sessions, scopes, write hooks | [docs/concepts/memory.md](docs/concepts/memory.md) · [BACKENDS.md](https://github.com/dimpagk92/cel-memory/blob/main/BACKENDS.md) |
| [`cel-memory-sqlite`](https://github.com/dimpagk92/cel-memory-sqlite) | [crates.io](https://crates.io/crates/cel-memory-sqlite) | Local SQLite + vector + FTS backend | [docs/concepts/memory.md](docs/concepts/memory.md) |
| [`cel-brief`](https://github.com/dimpagk92/cel-brief) | [crates.io](https://crates.io/crates/cel-brief) | Brief assembly, budgeting, governance, receipts | [docs/concepts/brief.md](docs/concepts/brief.md) |
| [`cel-contracts`](https://github.com/dimpagk92/cel-contracts) | [crates.io](https://crates.io/crates/cel-contracts) | Action, planning, and execution receipt schemas | [docs/concepts/receipts.md](docs/concepts/receipts.md) |

See [docs/crates.md](docs/crates.md) for the full crate matrix.

## Architecture

```text
+------------------------------------------------------------+
| Agents       LangGraph | Mastra | Claude Code | Cursor     |
|              Codex | GPT | Gemini | n8n | MCP clients     |
+------------------------------------------------------------+
| Cellar       live cortex runtime, policy, monitoring,       |
| runtime      compliance, hosted execution, GUI workflows    |
+------------------------------------------------------------+
| CEL OSS      context snapshots, memory, brief assembly,     |
| contracts    transport schemas, receipts                    |
+------------------------------------------------------------+
| Sources      browser | desktop apps | logs | traces | APIs  |
+------------------------------------------------------------+
```

## Quickstart

Use the OSS contracts without the full runtime. Clone a standalone crate repo, or
run from this workspace:

```sh
# standalone repo (from repo root)
cargo run --example context_snapshot -- --json
cargo run --example basic
cargo run --example standalone
cargo run --features memory --example with_memory

# this workspace
cargo run -p cel-context --example context_snapshot -- --json
cargo run -p cel-memory --example basic
cargo run -p cel-memory-sqlite --example basic
cargo run -p cel-brief --example standalone
cargo run -p cel-brief --features memory --example with_memory
```

For a guided path, read [docs/quickstart.md](docs/quickstart.md).

## Examples

The top-level examples are organized by job-to-be-done:

- [examples/merge-context](examples/merge-context) — emit or capture multiple sources into one `ContextSnapshot`.
- [examples/memory-provider](examples/memory-provider) — store and retrieve memory through the `MemoryProvider` trait.
- [examples/build-brief](examples/build-brief) — build a governed prompt bundle and inspect its receipt.
- [examples/receipt-inspection](examples/receipt-inspection) — understand action and brief receipts.
- [examples/context-to-brief](examples/context-to-brief) — connect context, memory, and brief assembly end-to-end.

## Commercial Boundary

Open CEL provides the contracts. Cellar/Dilipod operates those contracts in a
live environment:

| Open CEL | Commercial Cellar/Dilipod |
|---|---|
| Context schema and merge contracts | Live cortex runtime |
| Memory and SQLite backend | Policy enforcement and approvals |
| Brief assembly and brief receipts | Monitoring, alerting, audit timeline |
| Receipt and transport schemas | Compliance exports and hosted workers |

See [docs/oss-boundary.md](docs/oss-boundary.md) and
[docs/commercial-model.md](docs/commercial-model.md).

## License

Open CEL crates and docs are Apache-2.0 unless a subdirectory states otherwise.
