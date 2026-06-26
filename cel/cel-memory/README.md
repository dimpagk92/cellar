# cel-memory

Backend-agnostic memory traits and value types for AI agents.

`cel-memory` is the contract between an agent and an arbitrary persistence
layer. The trait is small enough to drop in your own backend (file, SQLite,
Redis, Mem0, Hindsight, etc.) without changing agent code. The companion
[`cel-memory-sqlite`](../cel-memory-sqlite) crate provides a local SQLite
implementation.

## Purpose

Use `cel-memory` when agent code needs durable, scoped retrieval but should not
depend on a storage engine. Callers depend on `MemoryProvider`; backends decide
how chunks are stored, embedded, indexed, summarized, and aged.

**Status:** v0.1 — the `MemoryProvider` trait surface is stable. Two implementations ship against it: `BasicMemoryProvider` (in-crate, in-memory reference) and [`cel-memory-sqlite`](../cel-memory-sqlite) (SQLite + vector + FTS, hybrid retrieval).

## What's Included

- `MemoryProvider` trait — async interface every backend implements.
- Value types: `MemoryChunk`, `ChunkKind`, `MemoryTier`, `MemoryQuery`, `MemorySession`, etc.
- `BasicMemoryProvider` — in-memory reference impl. Useful for tests and as the conformance reference for new backends.
- `MemoryWriteHook` trait — governance hook every backend should consult before persisting (lets a rule engine redact or veto writes).
- `MemoryError` — self-contained error type.

## Out Of Scope

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

Runnable examples:

```sh
cargo run -p cel-memory --example basic
cargo run -p cel-memory --example backend_swap
cargo run -p cel-memory --example write_hook
cargo run -p cel-memory --example custom_provider
```

- `basic` uses the in-memory reference provider end to end.
- `backend_swap` shows application code written against `MemoryProvider`.
- `write_hook` shows policy/redaction before persistence.
- `custom_provider` shows a provider wrapper that implements the trait.

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
