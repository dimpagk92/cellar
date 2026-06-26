//! Use a write hook to redact memory before it is persisted.
//!
//! Run with: `cargo run -p cel-memory --example write_hook`

use cel_memory::{
    BasicMemoryProvider, ChunkKind, ChunkSource, ClosureHook, MemoryProvider, NewMemoryChunk,
    WriteDecision,
};
use serde_json::json;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let memory = BasicMemoryProvider::new().with_write_hook(std::sync::Arc::new(ClosureHook(
        |chunk: &NewMemoryChunk| {
            if chunk.content.contains("api_key") {
                WriteDecision::Redact {
                    reason: "contains api_key".into(),
                }
            } else {
                WriteDecision::Allow
            }
        },
    )));

    let kept = memory.write(chunk("User prefers short answers")).await?;
    let redacted = memory.write(chunk("api_key=secret")).await?;

    println!("kept chunk: {}", kept.content);
    println!(
        "redacted write returned placeholder chunk id={} content={:?}",
        redacted.id, redacted.content
    );
    println!("stored chunks: {}", memory.stats().await?.total_chunks);
    Ok(())
}

fn chunk(content: &str) -> NewMemoryChunk {
    NewMemoryChunk {
        kind: ChunkKind::Chat,
        source: ChunkSource::Embedded,
        session_id: None,
        project_root: None,
        caller_id: "example-agent".into(),
        content: content.into(),
        metadata: json!(null),
        importance: None,
        shareable: false,
        pinned: false,
    }
}
