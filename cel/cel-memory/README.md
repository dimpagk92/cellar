# cel-memory

Local-first memory subsystem for AI agents. Trait surface, value types, and an in-memory reference provider.

`cel-memory` is the contract between an agent runtime and an arbitrary persistence layer. The trait is small enough to drop in your own backend (file, SQLite, Redis, Mem0, Hindsight, etc.) without changing agent code. Cellar ships an SQLite + vector implementation in [`cel-memory-sqlite`](../cel-memory-sqlite).

**Status:** v0.1 — the `MemoryProvider` trait surface is stable. Two implementations ship against it: `BasicMemoryProvider` (in-crate, in-memory reference) and [`cel-memory-sqlite`](../cel-memory-sqlite) (SQLite + vector + FTS, hybrid retrieval).

## What's in this crate

- `MemoryProvider` trait — async interface every backend implements.
- Value types: `MemoryChunk`, `ChunkKind`, `MemoryTier`, `MemoryQuery`, `MemorySession`, etc.
- `BasicMemoryProvider` — in-memory reference impl. Useful for tests and as the conformance reference for new backends.
- `MemoryWriteHook` trait — governance hook every backend should consult before persisting (lets a rule engine redact or veto writes).
- `MemoryError` — self-contained error type.

## What's NOT in this crate

- Storage. See [`cel-memory-sqlite`](../cel-memory-sqlite) for SQLite + vector retrieval.
- Embedding models. The trait makes no assumption about whether/how content is embedded — that's a backend concern.
- LLM-call retrieval logic. See `cel-brief` for "retrieve memory + assemble into an LLM prompt."

## Example

```rust
use cel_memory::{
    BasicMemoryProvider, ChunkKind, ChunkSource, MemoryProvider,
    NewMemoryChunk, MemoryQuery, CallerScope, RetrievalProfile,
};
use serde_json::json;

let memory = BasicMemoryProvider::new();

memory.write(NewMemoryChunk {
    kind: ChunkKind::Chat,
    source: ChunkSource::Embedded,
    caller_id: "my-agent".into(),
    content: "User prefers dry-run mode".into(),
    session_id: None,
    project_root: None,
    metadata: json!(null),
    importance: None,
    shareable: false,
    pinned: false,
}).await?;

let hits = memory.retrieve(MemoryQuery {
    text: "dry-run".into(),
    caller_scope: CallerScope::Own,
    caller_id: "my-agent".into(),
    k: 5,
    // ...
    profile: RetrievalProfile::AgentChatTurn,
    kinds: None, since: None, until: None,
    session_id: None, project_root_prefix: None,
    include_rollups: true, min_importance: None,
}).await?;
```

See [`examples/basic.rs`](examples/basic.rs) for a complete runnable example.

## Comparable libraries

| | cel-memory | Hindsight | Mem0 | Letta |
|---|---|---|---|---|
| Language | Rust | Python | Python | Python |
| Local-first | ✓ | ✓ | partial | ✓ |
| Pluggable backend | ✓ (trait) | partial | ✗ | ✗ |
| Governance hooks | ✓ | ✗ | ✗ | partial |
| Per-caller scoping | ✓ | ✗ | partial | partial |
| Sessions | ✓ | ✓ | ✓ | ✓ |

## License

Apache-2.0
