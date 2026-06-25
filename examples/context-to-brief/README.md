# Example: Context To Brief

Goal: show how the OSS contracts compose without requiring the full Cellar
runtime.

```text
source data
   ↓
ScreenContext
   ↓
MemoryProvider
   ↓
BriefBuilder
   ↓
Brief + BriefReceipt
```

## 1. Capture Or Emit Context

```sh
cargo run -p cel-context --example context_snapshot -- --json
```

Or emit your own `ContextElement` records from logs, app APIs, browser DOM, or a
test fixture.

## 2. Persist What Should Survive The Turn

```sh
cargo run -p cel-memory-sqlite --example basic
```

In a real agent loop, write selected observations, user preferences, completed
actions, and durable facts through `MemoryProvider`.

## 3. Assemble What The Model Sees

```sh
cargo run -p cel-brief --features memory --example with_memory
```

`cel-brief` can consume memory via `MemorySource`. A runtime can also expose its
current context through a perception source without making `cel-brief` depend on
that runtime.

## 4. Keep Receipts

Every model call should retain the `BriefReceipt`. Every mutating runtime action
should retain an `ExecutionReceipt`. Together they answer:

- What did the model see?
- What was remembered?
- What did the runtime attempt?
- What evidence supports the final claim?

## Commercial Runtime

The open contracts are enough to build custom loops and integrations. The
commercial Cellar/Dilipod layer adds continuous cortex operation, source
prioritization over time, policy enforcement, monitoring, compliance exports,
and hosted execution.
