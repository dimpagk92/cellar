//! Write application code against `MemoryProvider`, not a concrete backend.
//!
//! Run with: `cargo run -p cel-memory --example backend_swap`

use cel_memory::{
    BasicMemoryProvider, CallerScope, ChunkKind, ChunkSource, MemoryProvider, MemoryQuery,
    NewMemoryChunk, RetrievalProfile,
};
use serde_json::json;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let memory = BasicMemoryProvider::new();
    remember_preference(
        &memory,
        "example-agent",
        "Prefer concise deployment summaries",
    )
    .await?;
    let hits = recall(&memory, "example-agent", "deployment").await?;

    println!("retrieved {} memories", hits.len());
    for hit in hits {
        println!("  - {}", hit.content);
    }
    Ok(())
}

async fn remember_preference(
    memory: &dyn MemoryProvider,
    caller_id: &str,
    content: &str,
) -> cel_memory::Result<()> {
    memory
        .write(NewMemoryChunk {
            kind: ChunkKind::Context,
            source: ChunkSource::Embedded,
            session_id: None,
            project_root: None,
            caller_id: caller_id.into(),
            content: content.into(),
            metadata: json!({ "source": "example" }),
            importance: Some(0.8),
            shareable: false,
            pinned: false,
        })
        .await?;
    Ok(())
}

async fn recall(
    memory: &dyn MemoryProvider,
    caller_id: &str,
    text: &str,
) -> cel_memory::Result<Vec<cel_memory::MemoryChunk>> {
    memory
        .retrieve(MemoryQuery {
            text: text.into(),
            kinds: None,
            since: None,
            until: None,
            session_id: None,
            caller_scope: CallerScope::Own,
            project_root_prefix: None,
            k: 5,
            include_rollups: true,
            min_importance: None,
            profile: RetrievalProfile::AgentChatTurn,
            caller_id: caller_id.into(),
        })
        .await
}
