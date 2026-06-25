# Memory

`cel-memory` answers:

> What should persist across agent turns, and how does agent code avoid coupling to one storage backend?

## Crates

| Crate | Role |
|---|---|
| `cel-memory` | Trait, value types, sessions, retrieval queries, write hooks |
| `cel-memory-sqlite` | SQLite + vector + FTS implementation of the trait |

## Core Types

- `MemoryProvider` — async interface every backend implements.
- `MemoryChunk` / `NewMemoryChunk` — persisted units of memory.
- `MemorySession` / `NewMemorySession` — run or conversation scopes.
- `MemoryQuery` — retrieval request.
- `RetrievalProfile` — retrieval mode for the caller's intent.
- `MemoryWriteHook` — governance hook for redaction or veto before persistence.

## Why It Exists

Agent runtimes should depend on the memory contract, not a database:

```text
agent runtime → MemoryProvider → BasicMemoryProvider
                              → SqliteMemoryProvider
                              → your backend
```

That keeps local-first use, test doubles, and production backends swappable.

## Examples

In-memory reference provider:

```sh
cargo run -p cel-memory --example basic
```

SQLite backend:

```sh
cargo run -p cel-memory-sqlite --example basic
```

## Governance Boundary

The OSS trait includes write hooks because governance must be enforceable at the
boundary. The commercial product can add policy authoring, review queues,
retention controls, compliance exports, and audit UI on top of the same hook.

## Backend Choice

Use `BasicMemoryProvider` when you need:

- examples
- tests
- in-process prototypes

Use `SqliteMemoryProvider` when you need:

- local persistence
- hybrid vector + FTS retrieval
- one file to back up or encrypt
- backend behavior closer to production

See [../../examples/memory-provider](../../examples/memory-provider).
