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

// ───── Phase 2: retrieve (hybrid vec + FTS + recency) ─────

fn retrieve_q(caller: &str, text: &str) -> cel_memory::MemoryQuery {
    cel_memory::MemoryQuery {
        text: text.into(),
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
        caller_id: caller.into(),
    }
}

#[tokio::test]
async fn retrieve_returns_keyword_matches_via_fts() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    provider
        .write(nc("embedded", "discussing the Q4 report"))
        .await
        .unwrap();
    provider
        .write(nc("embedded", "completely unrelated content about weather"))
        .await
        .unwrap();
    let hits = provider
        .retrieve(retrieve_q("embedded", "q4 report"))
        .await
        .unwrap();
    assert!(!hits.is_empty(), "expected at least one hit");
    assert!(
        hits[0].content.to_lowercase().contains("q4"),
        "first hit should mention Q4, got: {}",
        hits[0].content
    );
}

#[tokio::test]
async fn retrieve_caller_scope_isolates_results() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    provider
        .write(nc("embedded", "shared phrase alpha"))
        .await
        .unwrap();
    provider
        .write(nc("mcp:cursor", "shared phrase alpha"))
        .await
        .unwrap();
    // Own scope: embedded sees only its own.
    let hits_own = provider
        .retrieve(retrieve_q("embedded", "shared phrase"))
        .await
        .unwrap();
    assert!(hits_own.iter().all(|c| c.caller_id == "embedded"));
    // Global scope: sees both.
    let mut q = retrieve_q("audit", "shared phrase");
    q.caller_scope = cel_memory::CallerScope::Global;
    let hits_global = provider.retrieve(q).await.unwrap();
    let callers: std::collections::HashSet<_> =
        hits_global.iter().map(|c| c.caller_id.clone()).collect();
    assert!(callers.contains("embedded"));
    assert!(callers.contains("mcp:cursor"));
}

#[tokio::test]
async fn retrieve_kind_filter() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    let mut chat = nc("embedded", "alpha");
    chat.kind = ChunkKind::Chat;
    let mut action = nc("embedded", "alpha");
    action.kind = ChunkKind::Action;
    provider.write(chat).await.unwrap();
    provider.write(action).await.unwrap();
    let mut q = retrieve_q("embedded", "alpha");
    q.kinds = Some(vec![ChunkKind::Chat]);
    let hits = provider.retrieve(q).await.unwrap();
    assert!(hits.iter().all(|c| c.kind == ChunkKind::Chat));
    assert_eq!(hits.len(), 1);
}

#[tokio::test]
async fn retrieve_empty_text_rejected() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    let err = provider
        .retrieve(retrieve_q("embedded", "   "))
        .await
        .unwrap_err();
    assert!(matches!(err, MemoryError::InvalidArgument(_)));
}

#[tokio::test]
async fn retrieve_honors_min_importance() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    // Two chunks at default importance (0.5).
    provider.write(nc("embedded", "alpha chunk")).await.unwrap();
    let bumped = provider
        .write(nc("embedded", "alpha bumped"))
        .await
        .unwrap();
    provider.update_importance(&bumped.id, 0.95).await.unwrap();

    let mut q = retrieve_q("embedded", "alpha");
    q.min_importance = Some(0.9);
    let hits = provider.retrieve(q).await.unwrap();
    assert!(
        hits.iter().all(|c| c.importance >= 0.9),
        "found chunks below importance floor"
    );
    // The bumped chunk should be in the results.
    assert!(hits.iter().any(|c| c.id == bumped.id));
}

// (Phase-0 stub test removed — `retrieve` is implemented in Phase 2, see
// the behavioural tests above. `retrieve_empty_text_rejected` covers the
// argument-validation path that the stub test used to test by accident.)

#[tokio::test]
async fn retrieve_results_consistent_across_repeated_calls() {
    // Regression guard for the retrieve cache: two identical calls must
    // return the same set of hits in the same order, whether the second
    // call hits the cache or re-runs the SQL path.
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    provider
        .write(nc("embedded", "alpha beta gamma"))
        .await
        .unwrap();
    provider.write(nc("embedded", "alpha")).await.unwrap();
    let q = retrieve_q("embedded", "alpha");
    let a = provider.retrieve(q.clone()).await.unwrap();
    let b = provider.retrieve(q).await.unwrap();
    assert_eq!(a.len(), b.len(), "second call should match first");
    for (x, y) in a.iter().zip(b.iter()) {
        assert_eq!(x.id, y.id, "result order must be stable");
    }
}

#[tokio::test]
async fn retrieve_invalidates_cache_on_write() {
    // The hot-path cache must drop on every write. If invalidation was
    // missing, the second retrieve would return the pre-write candidate
    // set and `after.len() == before.len()` would hold.
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    provider.write(nc("embedded", "alpha")).await.unwrap();
    let q = retrieve_q("embedded", "alpha");
    let before = provider.retrieve(q.clone()).await.unwrap();
    provider.write(nc("embedded", "alpha beta")).await.unwrap();
    let after = provider.retrieve(q).await.unwrap();
    assert!(
        after.len() > before.len(),
        "expected post-write retrieve to surface the new chunk \
         (cache invalidation missing? before={}, after={})",
        before.len(),
        after.len()
    );
}

#[tokio::test]
async fn retrieve_invalidates_cache_on_delete() {
    // Same contract as `retrieve_invalidates_cache_on_write`, but for the
    // delete path — without invalidation, a tombstoned chunk could keep
    // showing up in retrieve results for the full TTL window.
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    let a = provider.write(nc("embedded", "alpha first")).await.unwrap();
    provider
        .write(nc("embedded", "alpha second"))
        .await
        .unwrap();
    let q = retrieve_q("embedded", "alpha");
    let before = provider.retrieve(q.clone()).await.unwrap();
    assert!(before.iter().any(|c| c.id == a.id));
    provider
        .delete(&a.id, cel_memory::EvictionReason::UserDelete)
        .await
        .unwrap();
    let after = provider.retrieve(q).await.unwrap();
    assert!(
        !after.iter().any(|c| c.id == a.id),
        "deleted chunk should not appear in post-delete retrieve"
    );
}

// ───── Phase 1 completion: list_sessions / delete_matching / etc ─────

#[tokio::test]
async fn list_sessions_filters_by_caller_and_outcome() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    let a = provider
        .open_session(NewMemorySession {
            caller_id: "embedded".into(),
            title: Some("a".into()),
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap();
    let _b = provider
        .open_session(NewMemorySession {
            caller_id: "mcp:cursor".into(),
            title: Some("b".into()),
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap();
    provider
        .close_session(&a.id, SessionOutcome::Success)
        .await
        .unwrap();

    // caller_id filter narrows to embedded
    let embedded = provider
        .list_sessions(cel_memory::SessionFilter {
            caller_id: Some("embedded".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(embedded.len(), 1);
    assert_eq!(embedded[0].caller_id, "embedded");

    // open_only narrows to the still-open session
    let open = provider
        .list_sessions(cel_memory::SessionFilter {
            open_only: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].outcome, SessionOutcome::Open);
}

#[tokio::test]
async fn delete_matching_empty_predicate_is_noop() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    provider.write(nc("embedded", "x")).await.unwrap();
    let n = provider
        .delete_matching(
            cel_memory::MemoryPredicate::default(),
            cel_memory::EvictionReason::UserDelete,
        )
        .await
        .unwrap();
    assert_eq!(n, 0);
    assert_eq!(provider.stats().await.unwrap().total_chunks, 1);
}

#[tokio::test]
async fn delete_matching_by_caller_and_kind() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    let mut chat = nc("embedded", "chat one");
    chat.kind = ChunkKind::Chat;
    let mut action = nc("embedded", "action one");
    action.kind = ChunkKind::Action;
    let mut other_chat = nc("mcp:cursor", "chat two");
    other_chat.kind = ChunkKind::Chat;
    provider.write(chat).await.unwrap();
    provider.write(action).await.unwrap();
    provider.write(other_chat).await.unwrap();

    // Delete only embedded chats — should leave embedded action and cursor chat.
    let n = provider
        .delete_matching(
            cel_memory::MemoryPredicate {
                kinds: Some(vec![ChunkKind::Chat]),
                callers: Some(vec!["embedded".into()]),
                ..Default::default()
            },
            cel_memory::EvictionReason::UserDelete,
        )
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(provider.stats().await.unwrap().total_chunks, 2);
}

#[tokio::test]
async fn delete_matching_by_content_substring() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    provider
        .write(nc("embedded", "mention of bank.example.com"))
        .await
        .unwrap();
    provider
        .write(nc("embedded", "unrelated content"))
        .await
        .unwrap();
    let n = provider
        .delete_matching(
            cel_memory::MemoryPredicate {
                content_contains: Some("BANK.example".into()), // case-insensitive
                ..Default::default()
            },
            cel_memory::EvictionReason::RedactRule,
        )
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(provider.stats().await.unwrap().total_chunks, 1);
}

#[tokio::test]
async fn update_importance_clamps_and_persists() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    let c = provider.write(nc("embedded", "x")).await.unwrap();
    provider.update_importance(&c.id, 1.7).await.unwrap(); // > 1.0 → clamped
    let got = provider.get(&c.id).await.unwrap().unwrap();
    assert_eq!(got.importance, 1.0);
    provider.update_importance(&c.id, -0.5).await.unwrap(); // < 0 → clamped
    let got = provider.get(&c.id).await.unwrap().unwrap();
    assert_eq!(got.importance, 0.0);
    let err = provider
        .update_importance("missing-id", 0.5)
        .await
        .unwrap_err();
    assert!(matches!(err, MemoryError::NotFound(_)));
}

#[tokio::test]
async fn supersede_sets_superseded_by_and_validates_both_chunks_exist() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    let old = provider.write(nc("embedded", "old advice")).await.unwrap();
    let new = provider
        .write(nc("embedded", "corrected advice"))
        .await
        .unwrap();
    provider.supersede(&old.id, &new.id).await.unwrap();
    let after = provider.get(&old.id).await.unwrap().unwrap();
    assert_eq!(after.superseded_by.as_deref(), Some(new.id.as_str()));
    // Unknown new chunk errors before mutating anything
    let err = provider
        .supersede(&old.id, "does-not-exist")
        .await
        .unwrap_err();
    assert!(matches!(err, MemoryError::NotFound(_)));
}

#[tokio::test]
async fn record_access_appends_to_log_and_validates_chunk_exists() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    let c = provider.write(nc("embedded", "x")).await.unwrap();
    provider.record_access(&c.id, "agent", true).await.unwrap();
    // No public way to read the access log directly except via export.
    let bundle = provider
        .export(cel_memory::ExportFilter {
            include_access_log: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(bundle.accesses.len(), 1);
    assert_eq!(bundle.accesses[0].chunk_id, c.id);
    assert!(bundle.accesses[0].used);
    let err = provider
        .record_access("missing", "agent", false)
        .await
        .unwrap_err();
    assert!(matches!(err, MemoryError::NotFound(_)));
}

#[tokio::test]
async fn run_aging_sweep_deletes_old_unpinned_non_correction_chunks() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    let fresh = provider.write(nc("embedded", "fresh")).await.unwrap();
    let stale = provider.write(nc("embedded", "stale")).await.unwrap();
    // Backdate "stale" past the 30-day horizon via direct SQL — there's no
    // public API to set created_at after the fact, by design.
    {
        use rusqlite::params;
        let old_ms = (chrono::Utc::now() - chrono::Duration::days(45)).timestamp_millis();
        let conn_arc = provider.conn_for_test();
        let guard = conn_arc.lock().await;
        guard
            .execute(
                "UPDATE memory_chunks SET created_at = ? WHERE id = ?",
                params![old_ms, stale.id],
            )
            .unwrap();
    }
    let report = provider.run_aging_sweep().await.unwrap();
    assert_eq!(report.deleted, 1);
    // Fresh chunk survived; stale was evicted.
    assert!(provider.get(&fresh.id).await.unwrap().is_some());
    assert!(provider.get(&stale.id).await.unwrap().is_none());
}

#[tokio::test]
async fn write_hook_redact_suppresses_persistence() {
    use cel_memory::{ClosureHook, MemoryWriteHook, WriteDecision};

    let embedder = Arc::new(MockEmbedder::new());
    let hook: Arc<dyn MemoryWriteHook> = Arc::new(ClosureHook(|c: &cel_memory::NewMemoryChunk| {
        if c.content.contains("redact-me") {
            WriteDecision::Redact {
                reason: "test rule".into(),
            }
        } else {
            WriteDecision::Allow
        }
    }));
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap()
        .with_write_hook(hook);

    // Allowed chunk persists.
    let allowed = provider
        .write(nc("embedded", "an innocent chat"))
        .await
        .unwrap();
    assert!(!allowed.content.starts_with("<redacted"));
    // Redacted chunk is returned with a marker but isn't in the store.
    let redacted = provider
        .write(nc("embedded", "this should redact-me"))
        .await
        .unwrap();
    assert!(redacted.content.starts_with("<redacted"));
    assert_eq!(redacted.embedding_dim, 0);
    // Stats: only the allowed chunk landed.
    let stats = provider.stats().await.unwrap();
    assert_eq!(stats.total_chunks, 1);
}

#[tokio::test]
async fn write_batch_round_trips_multiple_chunks() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    let chunks = vec![
        nc("embedded", "alpha one"),
        nc("embedded", "alpha two"),
        nc("mcp:cursor", "alpha three"),
    ];
    let written = provider.write_batch(chunks).await.unwrap();
    assert_eq!(written.len(), 3);
    // All chunks are persisted.
    let stats = provider.stats().await.unwrap();
    assert_eq!(stats.total_chunks, 3);
    // Each chunk has the embedder's dim recorded.
    for c in &written {
        assert_eq!(c.embedding_dim, 384);
        assert_eq!(c.embedding_model, "mock-384");
    }
}

#[tokio::test]
async fn write_batch_empty_input_is_ok() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    let out = provider.write_batch(Vec::new()).await.unwrap();
    assert_eq!(out.len(), 0);
}

#[tokio::test]
async fn write_batch_rejects_empty_content_up_front() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    let chunks = vec![nc("embedded", "ok"), nc("embedded", "")];
    let err = provider.write_batch(chunks).await.unwrap_err();
    assert!(matches!(err, MemoryError::InvalidArgument(_)));
    // No partial state: nothing committed.
    assert_eq!(provider.stats().await.unwrap().total_chunks, 0);
}

// ───── Phase 3: summarize_session ─────

#[tokio::test]
async fn summarize_session_without_summarizer_returns_not_implemented() {
    // Provider opened without `with_summarizer` keeps the v1 contract:
    // `NotImplemented` rather than crashing or trying to call out.
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    let session = provider
        .open_session(NewMemorySession {
            caller_id: "embedded".into(),
            title: Some("doomed".into()),
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap();
    let err = provider.summarize_session(&session.id).await.unwrap_err();
    assert!(matches!(err, MemoryError::NotImplemented(_)));
}

#[tokio::test]
async fn summarize_session_unknown_id_returns_not_found() {
    let embedder = Arc::new(MockEmbedder::new());
    let summarizer = cel_memory::MockSummarizer::new("never called");
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap()
        .with_summarizer(summarizer.clone());
    let err = provider.summarize_session("missing").await.unwrap_err();
    assert!(matches!(err, MemoryError::NotFound(_)));
    // Summarizer was never reached.
    assert_eq!(summarizer.call_count(), 0);
}

#[tokio::test]
async fn summarize_session_empty_session_errors_invalid_argument() {
    let embedder = Arc::new(MockEmbedder::new());
    let summarizer = cel_memory::MockSummarizer::new("never called");
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap()
        .with_summarizer(summarizer.clone());
    let session = provider
        .open_session(NewMemorySession {
            caller_id: "embedded".into(),
            title: None,
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap();
    let err = provider.summarize_session(&session.id).await.unwrap_err();
    assert!(matches!(err, MemoryError::InvalidArgument(_)));
    // Summarizer was never reached because we short-circuit on zero
    // members.
    assert_eq!(summarizer.call_count(), 0);
}

#[tokio::test]
async fn summarize_session_writes_summary_and_links_members() {
    // The contract: given N member chunks in a session, summarize_session
    // (a) produces a JobSummary chunk, (b) writes N rows into
    // memory_summary_members, (c) updates memory_sessions.summary, (d)
    // bumps total_chunks by exactly one.
    let embedder = Arc::new(MockEmbedder::new());
    let summarizer = cel_memory::MockSummarizer::new("the user closed the chat after Q4 review");
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap()
        .with_summarizer(summarizer.clone());

    let session = provider
        .open_session(NewMemorySession {
            caller_id: "embedded".into(),
            title: Some("Q4 review".into()),
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap();

    // Three member chunks attached to the session — chat, action, chat.
    let m1 = {
        let mut c = nc("embedded", "user: where is the Q4 report?");
        c.session_id = Some(session.id.clone());
        provider.write(c).await.unwrap()
    };
    let m2 = {
        let mut c = nc("embedded", "agent ran fs.copy on report.xlsx");
        c.session_id = Some(session.id.clone());
        c.kind = ChunkKind::Action;
        provider.write(c).await.unwrap()
    };
    let m3 = {
        let mut c = nc("embedded", "user: thanks");
        c.session_id = Some(session.id.clone());
        provider.write(c).await.unwrap()
    };

    let pre_stats = provider.stats().await.unwrap();
    assert_eq!(pre_stats.total_chunks, 3);

    let summary = provider.summarize_session(&session.id).await.unwrap();

    // 1. Summary chunk shape.
    assert_eq!(summary.kind, ChunkKind::JobSummary);
    assert_eq!(summary.session_id.as_deref(), Some(session.id.as_str()));
    assert_eq!(summary.caller_id, "embedded");
    assert_eq!(summary.content, "the user closed the chat after Q4 review");
    // Importance defaulted via the §10.2 heuristic: JobSummary bumps
    // baseline 0.5 by +0.2, no other signals → 0.7.
    assert!(
        (summary.importance - 0.7).abs() < 1e-5,
        "expected importance 0.7, got {}",
        summary.importance
    );
    assert_eq!(summary.metadata["session_id"], session.id);
    assert_eq!(summary.metadata["member_count"], 3);
    assert_eq!(summary.metadata["summarizer"], "mock");

    // 2. memory_chunks gained exactly one row.
    let post_stats = provider.stats().await.unwrap();
    assert_eq!(post_stats.total_chunks, 4);

    // 3. memory_summary_members has one row per member, linking the
    //    summary to each input chunk. We read the table directly via
    //    the test accessor — there's no public API for it yet.
    let member_ids = {
        let conn = provider.conn_for_test();
        let guard = conn.lock().await;
        let mut stmt = guard
            .prepare(
                "SELECT member_id FROM memory_summary_members
                     WHERE rollup_id = ? ORDER BY member_id ASC",
            )
            .unwrap();
        let rows: Vec<String> = stmt
            .query_map(rusqlite::params![&summary.id], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        rows
    };
    let mut want = vec![m1.id.clone(), m2.id.clone(), m3.id.clone()];
    want.sort();
    assert_eq!(member_ids, want);

    // 4. The session's `summary` column was backfilled.
    let session_after = provider.get_session(&session.id).await.unwrap().unwrap();
    assert_eq!(
        session_after.summary.as_deref(),
        Some("the user closed the chat after Q4 review")
    );

    // 5. The mock summarizer received the right chunks + context. The
    //    member feed is ordered oldest-first per the plan §9.4.
    let calls = summarizer.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].chunk_ids,
        vec![m1.id.clone(), m2.id.clone(), m3.id.clone()]
    );
    assert_eq!(calls[0].kind_label.as_deref(), Some("session"));
    // The session title flows into the note slot.
    assert_eq!(calls[0].note.as_deref(), Some("session title: Q4 review"));
}

#[tokio::test]
async fn summarize_session_repeats_dont_double_link_members() {
    // memory_summary_members uses (rollup_id, member_id) as composite
    // PK; INSERT OR IGNORE keeps a re-summarize call idempotent across
    // its link table writes. Each call still produces a new summary
    // chunk (re-running summarization is allowed; the prior summary
    // simply isn't replaced).
    let embedder = Arc::new(MockEmbedder::new());
    let summarizer = cel_memory::MockSummarizer::new("synthesis");
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap()
        .with_summarizer(summarizer);

    let session = provider
        .open_session(NewMemorySession {
            caller_id: "embedded".into(),
            title: None,
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap();
    let mut c = nc("embedded", "only message");
    c.session_id = Some(session.id.clone());
    provider.write(c).await.unwrap();

    let s1 = provider.summarize_session(&session.id).await.unwrap();
    let s2 = provider.summarize_session(&session.id).await.unwrap();
    assert_ne!(s1.id, s2.id, "each call should mint a new JobSummary");

    let member_link_count = {
        let conn = provider.conn_for_test();
        let guard = conn.lock().await;
        guard
            .query_row::<i64, _, _>("SELECT COUNT(*) FROM memory_summary_members", [], |r| {
                r.get(0)
            })
            .unwrap()
    };
    // Two summaries × one member each = two rows. The point is no
    // duplicates per (summary, member) pair.
    assert_eq!(member_link_count, 2);
}

#[tokio::test]
async fn summarize_session_excludes_prior_job_summaries_from_input() {
    // If we re-summarize a session that already has a prior summary in
    // the same session_id, the prior summary chunk MUST NOT be in the
    // input feed — otherwise the model summarizes its own prior
    // output and quality snowballs.
    let embedder = Arc::new(MockEmbedder::new());
    let summarizer = cel_memory::MockSummarizer::new("fresh");
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap()
        .with_summarizer(summarizer.clone());

    let session = provider
        .open_session(NewMemorySession {
            caller_id: "embedded".into(),
            title: None,
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap();
    let mut a = nc("embedded", "msg one");
    a.session_id = Some(session.id.clone());
    let a = provider.write(a).await.unwrap();
    let mut b = nc("embedded", "msg two");
    b.session_id = Some(session.id.clone());
    let b = provider.write(b).await.unwrap();

    let _first = provider.summarize_session(&session.id).await.unwrap();
    let _second = provider.summarize_session(&session.id).await.unwrap();

    let calls = summarizer.calls();
    assert_eq!(calls.len(), 2);
    // Both calls should see exactly the two raw chunks — not three (the
    // prior JobSummary chunk would be the third).
    let want = vec![a.id.clone(), b.id.clone()];
    assert_eq!(calls[0].chunk_ids, want);
    assert_eq!(calls[1].chunk_ids, want);
}

#[tokio::test]
async fn export_with_predicate_and_logs_round_trips() {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .unwrap();
    let chunk = provider.write(nc("embedded", "alpha")).await.unwrap();
    provider.write(nc("embedded", "beta")).await.unwrap();
    provider
        .delete(&chunk.id, cel_memory::EvictionReason::UserDelete)
        .await
        .unwrap();

    let bundle = provider
        .export(cel_memory::ExportFilter {
            predicate: Some(cel_memory::MemoryPredicate {
                content_contains: Some("beta".into()),
                ..Default::default()
            }),
            include_sessions: true,
            include_eviction_log: true,
            include_access_log: true,
        })
        .await
        .unwrap();
    assert_eq!(bundle.chunks.len(), 1);
    assert!(bundle.chunks[0].content.contains("beta"));
    assert_eq!(bundle.evictions.len(), 1);
    assert_eq!(bundle.evictions[0].chunk_id, chunk.id);
}
