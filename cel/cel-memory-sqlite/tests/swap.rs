//! Crate-level conformance test: `SqliteMemoryProvider` must work through
//! the locked `cel_memory::MemoryProvider` trait surface, not just as a
//! concrete type.
//!
//! Daemon-level integration tests (rule + gateway + provider) live in the
//! downstream Cellar daemon, where they belong — those tests depend on
//! daemon-only crates that `cel-memory-sqlite` deliberately does not pull as
//! dev-dependencies, keeping this crate standalone-testable.

use std::sync::Arc;

use cel_memory::{ChunkKind, ChunkSource, MemoryProvider, NewMemoryChunk};
use cel_memory_sqlite::{MockEmbedder, SqliteMemoryProvider};

#[tokio::test]
async fn sqlite_provider_works_through_locked_trait() {
    // Exercise the trait surface (Arc<dyn MemoryProvider>), not the
    // concrete type. This is the contract every backend honors.
    let embedder = Arc::new(MockEmbedder::new());
    let memory: Arc<dyn MemoryProvider> = Arc::new(
        SqliteMemoryProvider::open_in_memory(embedder)
            .await
            .unwrap(),
    );

    let chunk = memory
        .write(NewMemoryChunk {
            kind: ChunkKind::Chat,
            source: ChunkSource::Embedded,
            session_id: None,
            project_root: None,
            caller_id: "embedded".into(),
            content: "user asked about the Q4 report".into(),
            metadata: serde_json::Value::Null,
            importance: None,
            shareable: false,
            pinned: false,
        })
        .await
        .unwrap();

    let fetched = memory.get(&chunk.id).await.unwrap().unwrap();
    assert_eq!(fetched.content, chunk.content);

    let stats = memory.stats().await.unwrap();
    assert_eq!(stats.total_chunks, 1);
    assert_eq!(stats.embedding_model.as_deref(), Some("mock-384"));
}
