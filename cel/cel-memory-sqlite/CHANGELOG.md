# Changelog

All notable changes to `cel-memory-sqlite` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Pre-`0.1.0` versions developed in-workspace; the first published crates.io
release is `0.1.0`.

## [Unreleased]

## [0.1.0-pre] — 2026-05-23

### Added
- `SqliteMemoryProvider` — SQLite + [`sqlite-vec`](https://github.com/asg017/sqlite-vec)
  implementation of `cel_memory::MemoryProvider`. One file on disk, no
  separate vector service. Writes, gets, deletes, sessions, pinning,
  purge, stats, aging sweep, batched writes, exports, importance updates,
  superseding, access logging — all real.
- `Embedder` trait + `MockEmbedder` (always available, deterministic
  384-dim vectors derived from the content hash).
- `FastEmbedEmbedder` behind the `fastembed` feature — local ONNX runtime
  + `bge-small-en-v1.5`. Off by default to avoid the ~130 MB model
  download in dev workflows.
- Schema migrations for `memory_chunks`, `memory_vec` (sqlite-vec virtual
  table), `memory_fts` (FTS5), `memory_sessions`, `memory_summary_members`,
  `memory_access_log`, `memory_eviction_log`.
- Hybrid retrieval (vector + FTS + recency, weighted by `RetrievalProfile`).
- TTL+LRU `retrieve` cache with eager invalidation on writes and deletes.
- `examples/basic.rs` — round-trips 10 chunks through a tempfile DB with
  `MockEmbedder`. Builds with only this crate's declared deps.
- `SqliteMemoryError` — self-contained `thiserror` enum.

### Notes
- Imports only `cel-memory` from the workspace — verified by
  `scripts/lint-guard-extraction-crates.sh` (added 2026-05-23).
- Runtime-only dev-deps are deliberately absent here, so the crate stays
  standalone-testable.
