# Changelog

All notable changes to `cel-memory` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Pre-`0.1.0` versions develop in-workspace as part of [Cellar](https://github.com/dimpagk92/cellar);
the first published release on crates.io will be `0.1.0`. See
[`plans/cellar-oss-extraction-prep.md`](../../plans/cellar-oss-extraction-prep.md)
for the extraction roadmap.

## [Unreleased]

## [0.1.0-pre] — 2026-05-23

### Added
- `MemoryProvider` trait — the locked seam between memory subsystem and
  callers (daemon, agents, MCP server).
- Value types: `MemoryChunk`, `NewMemoryChunk`, `ChunkKind`, `ChunkSource`,
  `MemoryTier`, `MemorySession`, `NewMemorySession`, `SessionOutcome`,
  `MemoryQuery`, `MemoryPredicate`, `RetrievalProfile`, `CallerScope`.
- `BasicMemoryProvider` — in-process reference implementation. Useful for
  tests and as a documentation of the trait contract.
- `MemoryWriteHook` trait — governance seam consulted before every write.
  Lets a rule engine redact or veto persistence without coupling the
  memory provider to a specific rule format.
- `MemoryError` — self-contained `thiserror` enum. No re-exports from
  `cel-cortex`, `cel-cortex-daemon`, or other workspace crates.
- `examples/basic.rs` — runnable end-to-end example using
  `BasicMemoryProvider`. Builds with only this crate's declared deps.

### Notes
- Zero `cel-*` and `cellar-*` imports in `src/` — audited 2026-05-22, lint
  guard added 2026-05-23 (`scripts/lint-guard-extraction-crates.sh`).
- Public API exposes no Cellar-domain types (`DaemonEvent`, `CelAction`,
  `RuleFireRecord`, `CortexSnapshot`). Future PRs may break the trait
  during the `0.1.0-pre` series; the trait stabilizes at `0.1.0`.
