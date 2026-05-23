//! Minimal `cel-memory-sqlite` example mirroring `cel-memory/examples/basic.rs`.
//!
//! Demonstrates that the SQLite-backed provider can stand on its own with
//! only its declared deps (`cel-memory`, `rusqlite` via the bundled feature,
//! `sqlite-vec`, `tempfile`) — no other `cel-*` crates required.
//!
//! Flow:
//!   1. Open a fresh provider against a temp-file DB with `MockEmbedder`
//!      (no model download, no network, no fastembed feature).
//!   2. Open a session.
//!   3. Write 10 chunks.
//!   4. Read them back individually via `get`.
//!   5. Print counts via `stats`.
//!   6. Close the session and drop the provider cleanly.
//!
//! Run with: `cargo run -p cel-memory-sqlite --example basic`

use std::sync::Arc;

use cel_memory::{
    ChunkKind, ChunkSource, MemoryProvider, NewMemoryChunk, NewMemorySession, SessionOutcome,
};
use cel_memory_sqlite::{MockEmbedder, SqliteMemoryProvider};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Open a fresh provider against a temp-file DB.
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("memory.sqlite");
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open(&db_path, embedder).await?;
    println!("opened SqliteMemoryProvider at {}", db_path.display());

    // 2. Open a session.
    let session = provider
        .open_session(NewMemorySession {
            caller_id: "example".into(),
            title: Some("basic-example".into()),
            metadata: serde_json::json!({}),
        })
        .await?;
    println!("opened session {}", session.id);

    // 3. Write 10 chunks.
    let mut written = Vec::with_capacity(10);
    for i in 0..10 {
        let chunk = provider
            .write(NewMemoryChunk {
                kind: ChunkKind::Chat,
                source: ChunkSource::Embedded,
                session_id: Some(session.id.clone()),
                project_root: None,
                caller_id: "example".into(),
                content: format!("chunk number {i} — synthetic content for the basic example"),
                metadata: serde_json::json!({ "index": i }),
                importance: None,
                shareable: false,
                pinned: false,
            })
            .await?;
        written.push(chunk);
    }
    println!("wrote {} chunks", written.len());

    // 4. Read them back individually.
    let mut read_back = 0;
    for chunk in &written {
        if let Some(fetched) = provider.get(&chunk.id).await? {
            assert_eq!(fetched.content, chunk.content);
            read_back += 1;
        }
    }
    println!("read back {read_back} chunks (all matched on content)");

    // 5. Print counts.
    let stats = provider.stats().await?;
    println!(
        "stats: total_chunks={}, total_sessions={}, embedding_model={:?}",
        stats.total_chunks, stats.total_sessions, stats.embedding_model
    );

    // 6. Close the session and drop the provider cleanly.
    provider
        .close_session(&session.id, SessionOutcome::Success)
        .await?;
    println!("closed session and finished cleanly");

    Ok(())
}
