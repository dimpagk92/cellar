# CEL OSS Crate Matrix

The community surface is six hero crates at **0.2.0**. Each crate is published
independently on [crates.io](https://crates.io/) and maintained in its **own
GitHub repository** under [`dimpagk92`](https://github.com/dimpagk92). The
[`cellar-oss`](../) umbrella workspace mirrors those crates for integrated
examples and docs.

Upgrading from 0.1.x? See [migration-0.2.md](migration-0.2.md).

| Crate | Version | Repository | Role | Owns | Does not own |
|---|---|---|---|---|---|
| [`cel-context`](https://github.com/dimpagk92/cel-context) | [0.2.0](https://crates.io/crates/cel-context/0.2.0) | [GitHub](https://github.com/dimpagk92/cel-context) | Context snapshot standard | `ContextElement`, `ContextSnapshot`, merge mechanics | Live capture, policy, dispatch |
| [`cel-memory`](https://github.com/dimpagk92/cel-memory) | [0.2.0](https://crates.io/crates/cel-memory/0.2.0) | [GitHub](https://github.com/dimpagk92/cel-memory) | Memory contract | `MemoryProvider`, `Embedder`, chunks, sessions, hooks | Storage engines, prompt assembly |
| [`cel-memory-sqlite`](https://github.com/dimpagk92/cel-memory-sqlite) | [0.2.0](https://crates.io/crates/cel-memory-sqlite/0.2.0) | [GitHub](https://github.com/dimpagk92/cel-memory-sqlite) | Local SQLite backend | Hybrid retrieval, migrations, `FastEmbedEmbedder` | Trait definition, live context |
| [`cel-brief`](https://github.com/dimpagk92/cel-brief) | [0.2.0](https://crates.io/crates/cel-brief/0.2.0) | [GitHub](https://github.com/dimpagk92/cel-brief) | Governed model input | `BriefBuilder`, budgets, governance, receipts | Live perception, storage |
| [`cel-contracts`](https://github.com/dimpagk92/cel-contracts) | [0.2.0](https://crates.io/crates/cel-contracts/0.2.0) | [GitHub](https://github.com/dimpagk92/cel-contracts) | Boundary schemas | Actions, planning views, execution receipts | Runtime execution, UI |
| [`cel-summarizer`](https://github.com/dimpagk92/cel-summarizer) | [0.2.0](https://crates.io/crates/cel-summarizer/0.2.0) | [GitHub](https://github.com/dimpagk92/cel-summarizer) | LLM summarizers | `AnthropicSummarizer`, `OllamaSummarizer`, `build_default` | Memory storage, prompt assembly |

Implementing a custom memory backend (PostgreSQL, DuckDB, …)? See
[`cel-memory` BACKENDS.md](https://github.com/dimpagk92/cel-memory/blob/main/BACKENDS.md).

## How they compose

```text
sources / adapters / logs
        ↓
cel-context        → one ContextSnapshot
        ↓
cel-memory         → what persists across turns
        ↓
cel-summarizer     → optional LLM session/day rollups
        ↓
cel-brief          → what the model sees this turn
        ↓
cel-contracts      → actions, views, receipts at runtime boundaries
```

The commercial Cellar/Dilipod runtime operates these contracts continuously. The
OSS crates do not ship a full always-on desktop runtime.

## Dependency direction

```text
cel-memory-sqlite → cel-memory
cel-summarizer    → cel-memory
cel-brief         → cel-memory (optional `memory` feature)
cel-contracts     → cel-context
cel-context       → serde only
```

`cel-brief` must never depend on a live cortex runtime. Perception backends adapt
into `cel-brief` through the optional `perception` feature.

## What to link in external copy

- Context: [concepts/context.md](concepts/context.md)
- Memory: [concepts/memory.md](concepts/memory.md) + [BACKENDS.md](https://github.com/dimpagk92/cel-memory/blob/main/BACKENDS.md)
- Brief assembly: [concepts/brief.md](concepts/brief.md)
- Receipts: [concepts/receipts.md](concepts/receipts.md)
- 0.1 → 0.2 upgrade: [migration-0.2.md](migration-0.2.md)
- OSS/commercial split: [oss-boundary.md](oss-boundary.md)
