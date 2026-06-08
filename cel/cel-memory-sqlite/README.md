# cel-memory-sqlite

SQLite + [`sqlite-vec`](https://github.com/asg017/sqlite-vec) implementation of [`cel-memory`](../cel-memory)'s `MemoryProvider` trait. Local-first, single-file, embedded vector + FTS retrieval.

**Status:** v0.1 — implements the full `MemoryProvider` surface: writes, sessions, hybrid (vector + FTS + recency) retrieval fronted by a TTL+LRU cache, summarization and daily/rule-week rollups (via an injected summarizer), aging sweeps, export, and stats. `re_embed_all` is the one method still unimplemented.

## What's in this crate

- `SqliteMemoryProvider` — `MemoryProvider` impl backed by SQLite (one file, no separate process).
- `Embedder` trait + `MockEmbedder` (always available).
- `FastEmbedEmbedder` behind the `fastembed` feature — local ONNX runtime + `bge-small-en-v1.5` (~130 MB model download).
- Schema migrations for `memory_chunks`, `memory_vec` (sqlite-vec virtual table), `memory_fts` (FTS5), sessions, access log, eviction log.
- `sqlite-vec` extension loaded at connection open; the `memory_vec` virtual table is available without extra setup.

## Design

- **Single SQLite file.** One file to back up, encrypt, ship to compliance. No separate vector daemon, no second process to manage.
- **Brute-force vector scan up to ~1M chunks per user.** `sqlite-vec` is fast on Apple Silicon (5–30 ms at typical personal-memory scale). HNSW is a drop-in upgrade when `sqlite-vec` ships it.
- **Hybrid retrieval**: vector + FTS + recency, weighted per `RetrievalProfile`, fused with reciprocal-rank fusion and fronted by a short-TTL LRU cache.
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
