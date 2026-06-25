# CEL OSS Crate Matrix

The community surface is four hero crates plus a small shared contracts crate.
Together they define the open data plane: context, memory, briefs, and receipts.

The crates are independently consumable from crates.io under the `dimpagk92`
owner, but their source, examples, and shared docs live together in
`github.com/dimpagk92/cellar`. Do not split per-crate GitHub repos until a crate
develops its own independent contributor community and release cadence.

| Crate | Community Role | Owns | Does Not Own | Publish Target |
|---|---|---|---|---|
| `cel-context` | Common context snapshot standard | `ContextElement`, `ScreenContext`, source metadata, confidence, references, low-level merge/fallback mechanics | Continuous runtime policy, monitoring, action dispatch, product governance | crates.io |
| `cel-memory` | Durable memory contract | `MemoryProvider`, chunks, sessions, queries, caller scopes, write hooks, export/stats types | SQLite storage, model calls, prompt assembly, live context | crates.io |
| `cel-memory-sqlite` | Local-first memory backend | SQLite storage, vector/FTS retrieval, migrations, caching, export/stats implementation | Memory trait definition, prompt assembly, live context | crates.io |
| `cel-brief` | Governed per-turn model input | `Source`, `BriefBuilder`, token budgets, pruning, governance hook, `BriefReceipt` | Discovering live device truth, durable storage, action dispatch | crates.io |
| `cel-contracts` | Boundary schemas | planned actions, planning views, execution receipts, runtime capability types | Runtime execution, persistence, UI, policy | crates.io if shared independently |

## How They Compose

```text
sources / adapters / logs
        ↓
cel-context        → one ScreenContext
        ↓
cel-memory         → what persists across turns
        ↓
cel-brief          → what the model sees this turn
        ↓
cel-contracts      → actions, views, receipts at runtime boundaries
```

The commercial runtime (`cel-cortex`, `cellar-runtime`, policy/gateway wiring,
app, hosted workers) operates these contracts continuously. The OSS crates do
not promise a full always-on desktop runtime.

## Dependency Direction

Keep the public crates easy to embed:

```text
cel-memory-sqlite → cel-memory
cel-brief         → cel-memory (optional feature)
cel-contracts     → cel-context
cel-context       → low-level source crates only
```

`cel-brief` must never depend on `cel-cortex`. A live runtime may adapt its
perception output into `cel-brief` through the `perception` feature, but brief
assembly stays generic.

## What To Link In External Copy

- For context: [concepts/context.md](concepts/context.md)
- For memory: [concepts/memory.md](concepts/memory.md)
- For brief assembly: [concepts/brief.md](concepts/brief.md)
- For receipts: [concepts/receipts.md](concepts/receipts.md)
- For the OSS/commercial split: [oss-boundary.md](oss-boundary.md)
