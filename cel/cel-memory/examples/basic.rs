//! Minimal `cel-memory` example using [`BasicMemoryProvider`].
//!
//! Runs entirely in-process with no storage backend in scope. Demonstrates:
//! - Constructing the in-memory reference provider
//! - Opening a session
//! - Writing chunks
//! - Retrieving via lexical query (the reference provider does not do vector
//!   retrieval; see `cel-memory-sqlite` for that)
//! - Closing the session
//! - Inspecting stats
//!
//! Run with: `cargo run -p cel-memory --example basic`

use cel_memory::{
    BasicMemoryProvider, CallerScope, ChunkKind, ChunkSource, MemoryProvider, MemoryQuery,
    NewMemoryChunk, NewMemorySession, RetrievalProfile, SessionOutcome,
};
use serde_json::json;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let memory = BasicMemoryProvider::new();

    let session = memory
        .open_session(NewMemorySession {
            caller_id: "example".into(),
            title: Some("hello-world".into()),
            metadata: json!(null),
        })
        .await?;

    for content in [
        "Q4 report is filed under ~/Workspace",
        "User prefers dry-run mode",
        "Last action: copy ~/Documents/draft.md to ~/Workspace",
    ] {
        memory
            .write(NewMemoryChunk {
                kind: ChunkKind::Chat,
                source: ChunkSource::Embedded,
                session_id: Some(session.id.clone()),
                project_root: None,
                caller_id: "example".into(),
                content: content.into(),
                metadata: json!(null),
                importance: None,
                shareable: false,
                pinned: false,
            })
            .await?;
    }

    let hits = memory
        .retrieve(MemoryQuery {
            text: "workspace".into(),
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
            caller_id: "example".into(),
        })
        .await?;

    println!("retrieved {} chunks containing 'workspace':", hits.len());
    for c in &hits {
        println!("  - {}", c.content);
    }

    memory
        .close_session(&session.id, SessionOutcome::Success)
        .await?;

    let stats = memory.stats().await?;
    println!(
        "\nstats: {} chunks, {} sessions",
        stats.total_chunks, stats.total_sessions
    );

    Ok(())
}
