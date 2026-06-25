# Example: Memory Provider

Goal: write and retrieve memory through the open `MemoryProvider` contract.

## In-Memory Reference Provider

```sh
cargo run -p cel-memory --example basic
```

Use this for tests, demos, and backend conformance.

## SQLite Backend

```sh
cargo run -p cel-memory-sqlite --example basic
```

Use this when you want local persistence and hybrid vector + FTS retrieval.

## Integration Pattern

```rust
use cel_memory::{MemoryProvider, MemoryQuery, CallerScope, RetrievalProfile};

async fn retrieve_for_turn(
    memory: &dyn MemoryProvider,
    caller_id: &str,
    query: &str,
) -> cel_memory::Result<Vec<cel_memory::MemoryChunk>> {
    memory.retrieve(MemoryQuery {
        text: query.into(),
        caller_id: caller_id.into(),
        caller_scope: CallerScope::Own,
        k: 8,
        profile: RetrievalProfile::AgentChatTurn,
        kinds: None,
        since: None,
        until: None,
        session_id: None,
        project_root_prefix: None,
        include_rollups: true,
        min_importance: None,
    }).await
}
```

Agent code depends on the trait. Storage remains a deployment choice.
