# cel-memory-sqlite

SQLite-backed local memory for AI agents. Implements
[`cel-memory`](../cel-memory)'s `MemoryProvider` trait with single-file storage,
FTS, and vector search through [`sqlite-vec`](https://github.com/asg017/sqlite-vec).

**Status:** v0.1 — implements the full `MemoryProvider` surface: writes, sessions, hybrid (vector + FTS + recency) retrieval fronted by a TTL+LRU cache, summarization and daily/rule-week rollups (via an injected summarizer), aging sweeps, export, and stats. `re_embed_all` is the one method still unimplemented.

## Purpose

Use `cel-memory-sqlite` when you want a local, embeddable `MemoryProvider`
backend without running a separate database or vector service. It is designed
for agents, CLIs, desktop apps, local servers, and test harnesses that need
durable memory in one SQLite file.

## What's Included

- `SqliteMemoryProvider` — `MemoryProvider` impl backed by SQLite (one file, no separate process).
- `Embedder` trait + `MockEmbedder` (always available).
- `FastEmbedEmbedder` behind the `fastembed` feature — local ONNX runtime + `bge-small-en-v1.5` (~130 MB model download).
- Schema migrations for `memory_chunks`, `memory_vec` (sqlite-vec virtual table), `memory_fts` (FTS5), sessions, access log, eviction log.
- `sqlite-vec` extension loaded at connection open; the `memory_vec` virtual table is available without extra setup.

## Design

- **Single SQLite file.** One file to back up, encrypt, ship to compliance. No separate vector service, no second process to manage.
- **Brute-force vector scan up to ~1M chunks per user.** `sqlite-vec` is fast on Apple Silicon (5–30 ms at typical personal-memory scale). HNSW is a drop-in upgrade when `sqlite-vec` ships it.
- **Hybrid retrieval**: vector + FTS + recency, weighted per `RetrievalProfile`, fused with reciprocal-rank fusion and fronted by a short-TTL LRU cache.
- **Governance-first.** Every write consults the optional `MemoryWriteHook` from `cel-memory` — a rule engine can redact or veto.

## Example

```rust
use std::sync::Arc;
use cel_memory_sqlite::{MockEmbedder, SqliteMemoryProvider};

let provider = SqliteMemoryProvider::open(
    "memory.sqlite",
    Arc::new(MockEmbedder::new()),
).await?;
// Use as cel_memory::MemoryProvider — same trait as BasicMemoryProvider.
```

Run the complete example:

```sh
cargo run -p cel-memory-sqlite --example basic
```

## Features

- `fastembed` — enables `FastEmbedEmbedder` for local embeddings. Off by default to avoid the 130 MB model download in dev workflows.

## License

Apache-2.0
