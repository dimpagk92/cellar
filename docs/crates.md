# CEL OSS Crate Matrix

The community surface is five hero crates. Each crate is published independently
on [crates.io](https://crates.io/) and maintained in its **own GitHub repository**
under [`dimpagk92`](https://github.com/dimpagk92). The [`cellar-oss`](../)
umbrella workspace mirrors those crates for integrated examples and docs.

| Crate | Repository | Role | Owns | Does not own |
|---|---|---|---|---|
| [`cel-context`](https://github.com/dimpagk92/cel-context) | [GitHub](https://github.com/dimpagk92/cel-context) | Context snapshot standard | `ContextElement`, `ContextSnapshot`, merge mechanics | Live capture, policy, dispatch |
| [`cel-memory`](https://github.com/dimpagk92/cel-memory) | [GitHub](https://github.com/dimpagk92/cel-memory) | Memory contract | `MemoryProvider`, chunks, sessions, hooks | Storage engines, prompt assembly |
| [`cel-memory-sqlite`](https://github.com/dimpagk92/cel-memory-sqlite) | [GitHub](https://github.com/dimpagk92/cel-memory-sqlite) | Local SQLite backend | Hybrid retrieval, migrations, embedder seam | Trait definition, live context |
| [`cel-brief`](https://github.com/dimpagk92/cel-brief) | [GitHub](https://github.com/dimpagk92/cel-brief) | Governed model input | `BriefBuilder`, budgets, governance, receipts | Live perception, storage |
| [`cel-contracts`](https://github.com/dimpagk92/cel-contracts) | [GitHub](https://github.com/dimpagk92/cel-contracts) | Boundary schemas | Actions, planning views, execution receipts | Runtime execution, UI |

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
cel-brief          → what the model sees this turn
        ↓
cel-contracts      → actions, views, receipts at runtime boundaries
```

The commercial Cellar/Dilipod runtime operates these contracts continuously. The
OSS crates do not ship a full always-on desktop runtime.

## Dependency direction

```text
cel-memory-sqlite → cel-memory
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
- OSS/commercial split: [oss-boundary.md](oss-boundary.md)
