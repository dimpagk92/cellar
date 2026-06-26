# Quickstart: CEL OSS Contracts

This quickstart uses CEL without the full Cellar runtime. You will capture or
construct context, persist memory, build a brief, and inspect receipts through
the open crates.

## Prerequisites

| Requirement | Version |
|---|---|
| Rust | stable |
| Cargo | bundled with Rust |
| macOS / Linux | only required by downstream live source adapters |

## 1. Inspect The Crate Matrix

Read [crates.md](crates.md) first if you are deciding what to embed.

```text
cel-context         context snapshots
cel-memory          memory contract
cel-memory-sqlite   local backend
cel-brief           governed model input
cel-contracts       action / planning / receipt schemas
```

## 2. Build A Context Snapshot

```sh
cargo run -p cel-context --example context_snapshot -- --json
```

This prints a `ScreenContext`: app/window metadata, elements, source labels,
confidence scores, bounds, state, and observed events when available.

`cel-context` does not capture accessibility, browser, OCR, network, or vision
streams itself. Your own source emits `ContextElement`s or `ScreenContext`s and
feeds them into the generic merger. See [concepts/context.md](concepts/context.md).

## 3. Store Durable Memory

Start with the in-memory reference implementation:

```sh
cargo run -p cel-memory --example basic
```

Then use the local SQLite backend:

```sh
cargo run -p cel-memory-sqlite --example basic
```

Both examples use the same `MemoryProvider` trait. Your agent code should
depend on the trait and swap backends underneath it.

## 4. Build A Governed Brief

Run the standalone prompt assembly example:

```sh
cargo run -p cel-brief --example no_cellar
```

Then run the memory-aware example:

```sh
cargo run -p cel-brief --features memory --example with_memory
```

The output includes a structured `Brief` and `BriefReceipt`: token counts,
source stats, dropped items, and redactions.

## 5. Inspect Receipts

Receipts are data contracts, not the audit product itself.

- `BriefReceipt` proves what was included, dropped, redacted, and budgeted for a model call.
- `ExecutionReceipt` proves how a runtime dispatched an action and what effect it observed.
- Task completion still requires post-state evidence: readback, context reread, screenshot, or runtime diff.

See [concepts/receipts.md](concepts/receipts.md).

## 6. Combine The Pieces

The common flow is:

```text
sources/logs/UI facts
        ↓
ScreenContext
        ↓
MemoryProvider retrieve/write
        ↓
BriefBuilder
        ↓
model call + receipts
```

See [examples/context-to-brief](../examples/context-to-brief) for the guided
end-to-end path.

## Commercial Runtime

The open crates do not include the full live cortex runtime. Cellar/Dilipod
operates these contracts continuously through live perception, policy
enforcement, monitoring, audit timelines, compliance exports, and hosted
workers. See [oss-boundary.md](oss-boundary.md).
