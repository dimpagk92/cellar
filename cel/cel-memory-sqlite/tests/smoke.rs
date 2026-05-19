//! Smoke tests for the SQLite memory backend.
//!
//! Exercise the dependencies we're committing to in v1 Phase 0:
//!
//! 1. sqlite-vec extension loads and a 384-dim vector round-trips through
//!    a `vec0` virtual table with k-NN search.
//! 2. Schema migrations apply cleanly against a fresh in-memory DB.
//! 3. SqliteMemoryProvider opens, persists chunks, retrieves them by ID,
//!    counts them via `stats`, deletes them, and `purge_all` wipes state.
//! 4. Session lifecycle works through the SQLite path.

use std::sync::Arc;

use cel_memory::{
    ChunkKind, ChunkSource, MemoryError, MemoryProvider, NewMemoryChunk, NewMemorySession,
    SessionOutcome,
};
use cel_memory_sqlite::{MockEmbedder, SqliteMemoryProvider};

fn nc(caller: &str, content: &str) -> NewMemoryChunk {
    NewMemoryChunk {
        kind: ChunkKind::Chat,
        source: ChunkSource::Embedded,
        session_id: None,
        project_root: None,
        caller_id: caller.into(),
        content: content.into(),
        metadata: serde_json::json!({"k": "v"}),
        importance: None,
        shareable: false,
        pinned: false,
    }
}

#[tokio::test]
async fn sqlite_vec_extension_loads_and_knn_works() {
    // Register the extension BEFORE opening the connection — auto-extensions
    // only affect connections opened after registration.
    cel_memory_sqlite::vec_extension::register();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE VIRTUAL TABLE vt USING vec0(id TEXT PRIMARY KEY, v FLOAT[4])",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO vt(id, v) VALUES ('a', ?), ('b', ?), ('c', ?)",
        rusqlite::params![
            serde_json::to_string(&[1.0_f32, 0.0, 0.0, 0.0]).unwrap(),
            serde_json::to_string(&[0.0_f32, 1.0, 0.0, 0.0]).unwrap(),
            serde_json::to_string(&[0.9_f32, 0.1, 0.0, 0.0]).unwrap(),
        ],
    )
    .unwrap();

    let mut stmt = conn
        .prepare(
            "SELECT id, distance FROM vt
             WHERE v MATCH ? AND k = 2
             ORDER BY distance",
        )
        .unwrap();
    let query = serde_json::to_string(&[1.0_f32, 0.0, 0.0, 0.0]).unwrap();
    let rows: Vec<(String, f64)> = stmt
        .query_map([query], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .filter_map(|x| x.ok())
        .collect();
    // Closest two: 'a' (distance 0) then 'c' (close).
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "a");
    assert!(rows[0].1 < rows[1].1);
}

#[tokio::test]
async fn provider_opens_and_runs_migrations() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    // stats() proves the schema was created.
    let stats = provider.stats().await.unwrap();
    assert_eq!(stats.total_chunks, 0);
    assert_eq!(stats.embedding_model.as_deref(), Some("mock-384"));
}

#[tokio::test]
async fn provider_writes_and_reads_a_chunk() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();

    let written = provider
        .write(nc("embedded", "Q4 report is filed under Workspace"))
        .await
        .unwrap();
    assert_eq!(written.embedding_dim, 384);
    assert_eq!(written.embedding_model, "mock-384");

    let fetched = provider.get(&written.id).await.unwrap().unwrap();
    assert_eq!(fetched.content, written.content);
    assert_eq!(fetched.caller_id, "embedded");
    assert_eq!(fetched.kind, ChunkKind::Chat);
    assert_eq!(fetched.metadata["k"], "v");

    let stats = provider.stats().await.unwrap();
    assert_eq!(stats.total_chunks, 1);
    assert_eq!(stats.session_chunks, 1);
}

#[tokio::test]
async fn provider_empty_content_rejected() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    let err = provider.write(nc("embedded", "")).await.unwrap_err();
    assert!(matches!(err, MemoryError::InvalidArgument(_)));
}

#[tokio::test]
async fn provider_session_open_close() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    let s = provider
        .open_session(NewMemorySession {
            caller_id: "embedded".into(),
            title: Some("test".into()),
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap();
    assert_eq!(s.outcome, SessionOutcome::Open);

    provider
        .close_session(&s.id, SessionOutcome::Success)
        .await
        .unwrap();
    let s2 = provider.get_session(&s.id).await.unwrap().unwrap();
    assert_eq!(s2.outcome, SessionOutcome::Success);
    assert!(s2.ended_at.is_some());
}

#[tokio::test]
async fn provider_close_unknown_session_returns_not_found() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    let err = provider
        .close_session("nope", SessionOutcome::Success)
        .await
        .unwrap_err();
    assert!(matches!(err, MemoryError::NotFound(_)));
}

#[tokio::test]
async fn provider_delete_clears_chunk_and_vec_row() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    let c = provider.write(nc("embedded", "hello")).await.unwrap();
    provider
        .delete(&c.id, cel_memory::EvictionReason::UserDelete)
        .await
        .unwrap();
    assert!(provider.get(&c.id).await.unwrap().is_none());
    let stats = provider.stats().await.unwrap();
    assert_eq!(stats.total_chunks, 0);
}

#[tokio::test]
async fn provider_purge_all_returns_counts_and_wipes() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    provider.write(nc("embedded", "one")).await.unwrap();
    provider.write(nc("embedded", "two")).await.unwrap();
    let _ = provider
        .open_session(NewMemorySession {
            caller_id: "embedded".into(),
            title: None,
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap();

    let report = provider.purge_all().await.unwrap();
    assert_eq!(report.chunks_deleted, 2);
    assert_eq!(report.sessions_deleted, 1);

    let stats = provider.stats().await.unwrap();
    assert_eq!(stats.total_chunks, 0);
    assert_eq!(stats.total_sessions, 0);
}

#[tokio::test]
async fn provider_pin_works_and_unknown_id_errors() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    let c = provider.write(nc("embedded", "x")).await.unwrap();
    provider.pin(&c.id, true).await.unwrap();
    let after = provider.get(&c.id).await.unwrap().unwrap();
    assert!(after.pinned);
    let err = provider.pin("missing-id", true).await.unwrap_err();
    assert!(matches!(err, MemoryError::NotFound(_)));
}

#[tokio::test]
async fn provider_persists_across_reopen() {
    // Write through one provider, close, reopen against the same file,
    // verify the chunk is still there.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.db");
    let embedder = Arc::new(MockEmbedder::new());
    {
        let provider = SqliteMemoryProvider::open(&path, embedder.clone())
            .await
            .unwrap();
        provider.write(nc("embedded", "persist me")).await.unwrap();
    }
    let provider2 = SqliteMemoryProvider::open(&path, embedder).await.unwrap();
    let stats = provider2.stats().await.unwrap();
    assert_eq!(stats.total_chunks, 1);
}

#[tokio::test]
async fn provider_retrieve_is_not_implemented_in_phase_0() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    let err = provider
        .retrieve(cel_memory::MemoryQuery {
            text: "x".into(),
            kinds: None,
            since: None,
            until: None,
            session_id: None,
            caller_scope: cel_memory::CallerScope::Own,
            project_root_prefix: None,
            k: 8,
            include_rollups: true,
            min_importance: None,
            profile: cel_memory::RetrievalProfile::AgentChatTurn,
            caller_id: "x".into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, MemoryError::NotImplemented(_)));
}
