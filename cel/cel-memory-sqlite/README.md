# cel-memory-sqlite

SQLite + [`sqlite-vec`](https://github.com/asg017/sqlite-vec) implementation of [`cel-memory`](../cel-memory)'s `MemoryProvider` trait. Local-first, single-file, embedded vector + FTS retrieval.

**Status:** v0.1 — Phase 1 (persistence + writes) complete; Phase 2 (hybrid retrieval) in progress. Roadmap in the [Cellar Memory & Context Manager plan](https://github.com/dimpagk92/cellar/blob/main/plans/cellar-memory-manager.md).

## What's in this crate

- `SqliteMemoryProvider` — `MemoryProvider` impl backed by SQLite (one file, no separate process).
- `Embedder` trait + `MockEmbedder` (always available).
- `FastEmbedEmbedder` behind the `fastembed` feature — local ONNX runtime + `bge-small-en-v1.5` (~130 MB model download).
- Schema migrations for `memory_chunks`, `memory_vec` (sqlite-vec virtual table), `memory_fts` (FTS5), sessions, access log, eviction log.
- `sqlite-vec` extension loaded at connection open; the `memory_vec` virtual table is available without extra setup.

## Design

- **Single SQLite file.** One file to back up, encrypt, ship to compliance. No separate vector daemon, no second process to manage.
- **Brute-force vector scan up to ~1M chunks per user.** `sqlite-vec` is fast on Apple Silicon (5–30 ms at typical personal-memory scale). HNSW is a drop-in upgrade when `sqlite-vec` ships it.
- **Hybrid retrieval** (Phase 2): vector + FTS + recency, weighted per `RetrievalProfile`.
- **Governance-first.** Every write consults the optional `MemoryWriteHook` from `cel-memory` — a rule engine can redact or veto.

## Example

```rust
use cel_memory_sqlite::{SqliteMemoryProvider, MockEmbedder};

let provider = SqliteMemoryProvider::open(
    "memory.sqlite",
    Box::new(MockEmbedder::new(384)),
).await?;
// Use as cel_memory::MemoryProvider — same trait as BasicMemoryProvider.
```

## Features

- `fastembed` — enables `FastEmbedEmbedder` for local embeddings. Off by default to avoid the 130 MB model download in dev workflows.

## License

Apache-2.0
