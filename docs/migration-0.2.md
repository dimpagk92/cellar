# Migrating from CEL 0.1.x to 0.2.0

CEL **0.2.0** is a semver-minor release with a small set of intentional breaking
cleanups. All six OSS crates ship at **0.2.0** on [crates.io](https://crates.io/).

| Crate | crates.io |
|-------|-----------|
| `cel-context` | [0.2.0](https://crates.io/crates/cel-context/0.2.0) |
| `cel-contracts` | [0.2.0](https://crates.io/crates/cel-contracts/0.2.0) |
| `cel-memory` | [0.2.0](https://crates.io/crates/cel-memory/0.2.0) |
| `cel-memory-sqlite` | [0.2.0](https://crates.io/crates/cel-memory-sqlite/0.2.0) |
| `cel-brief` | [0.2.0](https://crates.io/crates/cel-brief/0.2.0) |
| `cel-summarizer` | [0.2.0](https://crates.io/crates/cel-summarizer/0.2.0) *(new)* |

**MSRV:** all crates now declare `rust-version = "1.76"`.

---

## Upgrade order

Publish and resolve dependencies in this order:

```text
cel-context → cel-contracts
cel-memory  → cel-memory-sqlite → cel-summarizer
cel-brief   (optional cel-memory 0.2.0 via `memory` feature)
```

Example `Cargo.toml` pins:

```toml
cel-context = "0.2"
cel-contracts = "0.2"
cel-memory = "0.2"
cel-memory-sqlite = "0.2"
cel-brief = "0.2"
cel-summarizer = "0.2"
```

---

## Breaking renames

### `ScreenContext` → `ContextSnapshot` (`cel-context`)

The deprecated `ScreenContext` type alias was removed in **0.2.0**. Use
`ContextSnapshot` everywhere.

```rust
// before (0.1.x)
use cel_context::ScreenContext;

// after (0.2.0)
use cel_context::ContextSnapshot;
```

JSON field names are unchanged — only the Rust type name changed.

The in-tree `cellar-oss` workspace still exposes `ScreenContext` as a local alias
for integrated runtime code. Standalone crate consumers and new projects should
use `ContextSnapshot`.

### `ChunkSource::Cortex` → `Perception` (`cel-memory`)

The deprecated enum variant alias was removed.

```rust
// before
ChunkSource::Cortex

// after
ChunkSource::Perception
```

Serialized value remains `"perception"`.

---

## `Embedder` moved to `cel-memory`

Since **0.2.0**, the [`Embedder`](https://docs.rs/cel-memory/latest/cel_memory/trait.Embedder.html)
trait lives in `cel-memory`, not `cel-memory-sqlite`.

```rust
// preferred
use cel_memory::{Embedder, MockEmbedder};

// still works — re-exported for compatibility
use cel_memory_sqlite::{Embedder, MockEmbedder};
```

`FastEmbedEmbedder` remains in `cel-memory-sqlite` behind the `fastembed` feature.

Custom backend authors: see
[`cel-memory` BACKENDS.md](https://github.com/dimpagk92/cel-memory/blob/main/BACKENDS.md).

---

## New: `cel-summarizer`

LLM-backed [`Summarizer`](https://docs.rs/cel-memory/latest/cel_memory/trait.Summarizer.html)
implementations now ship as a standalone crate — no dependency on a private
router.

```toml
cel-summarizer = "0.2"
```

```rust
use std::sync::Arc;
use cel_memory::{BasicMemoryProvider, Summarizer};
use cel_summarizer::build_default;

let summarizer: Arc<dyn Summarizer> = build_default()?;
let memory = BasicMemoryProvider::new().with_summarizer(summarizer);
```

### Environment variables (OSS)

| Variable | Purpose |
|----------|---------|
| `CEL_SUMMARIZER_PROVIDER` | `anthropic` (default) or `ollama` |
| `ANTHROPIC_API_KEY` | Anthropic API key |
| `CEL_SUMMARIZER_MODEL` | Override default model |
| `OLLAMA_BASE_URL` | Ollama base URL (default `http://127.0.0.1:11434`) |

Cellar-private code may still use `CELLAR_MEMORY_SUMMARIZER_*` env vars via the
in-tree wrapper; new OSS integrations should use `CEL_SUMMARIZER_*`.

---

## Conformance testing

Backend crates should call [`assert_write_get_stats`](https://docs.rs/cel-memory/latest/cel_memory/fn.assert_write_get_stats.html)
from integration tests:

```rust
use std::sync::Arc;
use cel_memory::{assert_write_get_stats, MemoryProvider};

let memory: Arc<dyn MemoryProvider> = Arc::new(your_provider);
assert_write_get_stats(memory, "probe content").await?;
```

See [`cel-memory-sqlite/tests/swap.rs`](https://github.com/dimpagk92/cel-memory-sqlite/blob/main/tests/swap.rs).

---

## What did not change

- `MemoryProvider` trait surface (beyond summarization helpers added in 0.1.8)
- `BriefBuilder` public API
- `cel-contracts` action and receipt schemas
- JSON serialization shapes for `ContextElement` and `MemoryChunk`

---

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `ScreenContext` not found | Import `ContextSnapshot` from `cel_context` |
| `ChunkSource::Cortex` not found | Use `ChunkSource::Perception` |
| `Embedder` import from sqlite fails on old pin | Bump `cel-memory-sqlite` to `0.2` |
| Summarizer needs LLM client | Add `cel-summarizer = "0.2"` |
| `cel-memory-sqlite` won't resolve `cel-memory 0.2` | Publish/pull `cel-memory` 0.2.0 first |

Report issues on the relevant standalone repo under
[`dimpagk92`](https://github.com/dimpagk92).
