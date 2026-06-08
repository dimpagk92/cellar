//! Daemon-level IPC integration tests for the Phase 4 `memory.*` surface.
//!
//! Wires the real `DaemonIpcHandler` (with an in-memory rules store and a
//! `BasicMemoryProvider`-backed agent slot) into a duplex-stream IPC
//! server, then hits the three new methods end-to-end. Asserts:
//!
//! - `memory.remember` round-trips a chunk including `caller_id` and `shareable`.
//! - Two callers (`mcp:cursor`, `mcp:codex`) have isolated `Own` views.
//! - `shareable=true` chunks surface to other callers under `own_plus_shared`.
//! - `memory.forget` deletes by id (only owner can delete) and emits an
//!   `EvictionLog` row with reason `user_delete`.
//! - The `memory.rpc` capability is advertised when the agent slot is wired.
//! - Empty `memory.forget` is rejected with `ValidationFailed`.

use std::sync::Arc;

use cel_cortex_daemon::chat_bus::ChatBus;
use cel_cortex_daemon::{ipc::DaemonIpcHandler, Daemon};
use cel_memory::{BasicMemoryProvider, ChunkSource, EvictionReason, MemoryProvider};
use cellar_ipc::params::memory::{
    MemoryForgetParams, MemoryForgetPredicate, MemoryRecallParams, MemoryRememberParams,
};
use cellar_ipc::params::system::SystemHelloParams;
use cellar_ipc::results::memory::{MemoryForgetResult, MemoryRecallResult, MemoryRememberResult};
use cellar_ipc::results::system::SystemHelloResult;
use cellar_ipc::{serve_connection, Client};
use cellar_rules_store::SqliteRulesStore;

/// Build a `DaemonIpcHandler` with a real `BasicMemoryProvider` plugged
/// into the agent slot (so the `memory.*` methods light up). The agent
/// runtime itself is left `None` because none of these tests touch it.
fn handler_with_memory() -> (Arc<DaemonIpcHandler>, Arc<dyn MemoryProvider>) {
    let store = SqliteRulesStore::in_memory().unwrap();
    let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
    let chat_bus = ChatBus::new();
    let handler = Arc::new(
        DaemonIpcHandler::new("memory-phase4-test", store).with_agent(
            memory.clone(),
            None,
            chat_bus,
        ),
    );
    (handler, memory)
}

/// Spawn an IPC server task on a duplex stream and return a connected client.
async fn connect(handler: Arc<DaemonIpcHandler>) -> Client {
    let (server_stream, client_stream) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let _ = serve_connection(server_stream, handler).await;
    });
    let (client, _rx) = Client::from_stream(client_stream).await.unwrap();
    client
}

#[tokio::test]
async fn capability_memory_rpc_is_advertised() {
    let (handler, _) = handler_with_memory();
    let client = connect(handler).await;
    let hello: SystemHelloResult = client
        .call(
            "system.hello",
            SystemHelloParams {
                client_name: "memory-test".into(),
                client_version: "0".into(),
                supported_protocol_versions: vec!["1".into()],
            },
        )
        .await
        .unwrap();
    assert!(
        hello.capabilities.iter().any(|c| c == "memory.rpc"),
        "memory.rpc should be advertised when agent slot has a memory provider; got {:?}",
        hello.capabilities
    );
}

#[tokio::test]
async fn remember_round_trips_chunk_with_caller_id_and_shareable() {
    let (handler, _) = handler_with_memory();
    let client = connect(handler).await;
    let res: MemoryRememberResult = client
        .call(
            "memory.remember",
            MemoryRememberParams {
                content: "user prefers dry-run mode".into(),
                caller_id: Some("mcp:cursor".into()),
                kind: Some("correction".into()),
                session_id: None,
                project_root: None,
                tags: Some(vec!["pref".into(), "dry-run".into()]),
                importance: None,
                shareable: true,
                pinned: false,
            },
        )
        .await
        .unwrap();
    // Chunk JSON includes the daemon-stamped fields.
    let chunk = res.chunk;
    assert_eq!(chunk["caller_id"], "mcp:cursor");
    assert_eq!(chunk["shareable"], true);
    assert_eq!(chunk["kind"], "correction");
    assert_eq!(chunk["source"], "mcp");
    // metadata.tags propagated through.
    assert_eq!(chunk["metadata"]["tags"][0], "pref");
}

#[tokio::test]
async fn caller_ids_get_normalised_with_mcp_prefix() {
    let (handler, _) = handler_with_memory();
    let client = connect(handler).await;
    // Pass a raw client name (no prefix); daemon should stamp `mcp:cursor`.
    let res: MemoryRememberResult = client
        .call(
            "memory.remember",
            MemoryRememberParams {
                content: "hi".into(),
                caller_id: Some("cursor".into()),
                kind: None,
                session_id: None,
                project_root: None,
                tags: None,
                importance: None,
                shareable: false,
                pinned: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(res.chunk["caller_id"], "mcp:cursor");
}

#[tokio::test]
async fn two_callers_have_isolated_own_views() {
    let (handler, _) = handler_with_memory();
    let client = connect(handler).await;
    for (caller, content) in [
        ("mcp:cursor", "cursor private note about auth"),
        ("mcp:codex", "codex private note about auth"),
    ] {
        client
            .call::<_, MemoryRememberResult>(
                "memory.remember",
                MemoryRememberParams {
                    content: content.into(),
                    caller_id: Some(caller.into()),
                    kind: None,
                    session_id: None,
                    project_root: None,
                    tags: None,
                    importance: None,
                    shareable: false,
                    pinned: false,
                },
            )
            .await
            .unwrap();
    }
    // Cursor recalls only its own chunk under Own scope.
    let cursor: MemoryRecallResult = client
        .call(
            "memory.recall",
            MemoryRecallParams {
                query: "auth".into(),
                caller_id: Some("mcp:cursor".into()),
                limit: Some(10),
                kinds: None,
                scope: None,
                min_importance: None,
                since: None,
                session_id: None,
                project_root_prefix: None,
            },
        )
        .await
        .unwrap();
    assert!(
        cursor.chunks.iter().all(|c| c["caller_id"] == "mcp:cursor"),
        "cursor leak: {:?}",
        cursor.chunks
    );
    // Codex recalls only its own chunk.
    let codex: MemoryRecallResult = client
        .call(
            "memory.recall",
            MemoryRecallParams {
                query: "auth".into(),
                caller_id: Some("mcp:codex".into()),
                limit: Some(10),
                kinds: None,
                scope: None,
                min_importance: None,
                since: None,
                session_id: None,
                project_root_prefix: None,
            },
        )
        .await
        .unwrap();
    assert!(
        codex.chunks.iter().all(|c| c["caller_id"] == "mcp:codex"),
        "codex leak: {:?}",
        codex.chunks
    );
}

#[tokio::test]
async fn shareable_chunks_surface_under_own_plus_shared() {
    let (handler, _) = handler_with_memory();
    let client = connect(handler).await;
    // Cursor writes a shareable chunk + a private chunk.
    client
        .call::<_, MemoryRememberResult>(
            "memory.remember",
            MemoryRememberParams {
                content: "user prefers MM-DD-YYYY dates".into(),
                caller_id: Some("mcp:cursor".into()),
                kind: None,
                session_id: None,
                project_root: None,
                tags: None,
                importance: None,
                shareable: true,
                pinned: false,
            },
        )
        .await
        .unwrap();
    client
        .call::<_, MemoryRememberResult>(
            "memory.remember",
            MemoryRememberParams {
                content: "cursor private user secret".into(),
                caller_id: Some("mcp:cursor".into()),
                kind: None,
                session_id: None,
                project_root: None,
                tags: None,
                importance: None,
                shareable: false,
                pinned: false,
            },
        )
        .await
        .unwrap();
    // Codex queries with own_plus_shared scope. Only the shareable chunk
    // from cursor surfaces; the private one is invisible. Codex doesn't
    // see its own (none written) so the response is exactly that one.
    let res: MemoryRecallResult = client
        .call(
            "memory.recall",
            MemoryRecallParams {
                query: "user".into(),
                caller_id: Some("mcp:codex".into()),
                limit: Some(10),
                kinds: None,
                scope: Some("own_plus_shared".into()),
                min_importance: None,
                since: None,
                session_id: None,
                project_root_prefix: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(res.count, 1, "{:?}", res.chunks);
    assert!(
        res.chunks[0]["content"]
            .as_str()
            .unwrap()
            .contains("MM-DD-YYYY"),
        "wrong chunk surfaced: {:?}",
        res.chunks[0]
    );
    assert_eq!(res.chunks[0]["caller_id"], "mcp:cursor");
    assert_eq!(res.chunks[0]["shareable"], true);
}

#[tokio::test]
async fn forget_by_id_deletes_only_owned_chunks_and_emits_eviction_log() {
    let (handler, memory) = handler_with_memory();
    let client = connect(handler).await;
    // Write one chunk as cursor.
    let written: MemoryRememberResult = client
        .call(
            "memory.remember",
            MemoryRememberParams {
                content: "delete me".into(),
                caller_id: Some("mcp:cursor".into()),
                kind: None,
                session_id: None,
                project_root: None,
                tags: None,
                importance: None,
                shareable: false,
                pinned: false,
            },
        )
        .await
        .unwrap();
    let chunk_id = written.chunk["id"].as_str().unwrap().to_string();

    // Forget as cursor — succeeds.
    let res: MemoryForgetResult = client
        .call(
            "memory.forget",
            MemoryForgetParams {
                caller_id: Some("mcp:cursor".into()),
                chunk_ids: Some(vec![chunk_id.clone()]),
                predicate: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(res.deleted, 1);

    // The chunk is actually gone.
    assert!(memory.get(&chunk_id).await.unwrap().is_none());

    // The eviction log has a UserDelete row referencing the chunk.
    let bundle = memory
        .export(cel_memory::ExportFilter {
            predicate: None,
            include_eviction_log: true,
            include_access_log: false,
            include_sessions: false,
        })
        .await
        .unwrap();
    assert!(
        bundle
            .evictions
            .iter()
            .any(|e| e.chunk_id == chunk_id && e.reason == EvictionReason::UserDelete),
        "expected an EvictionLog row with UserDelete; got {:?}",
        bundle.evictions
    );
}

#[tokio::test]
async fn forget_id_owned_by_another_caller_is_rejected() {
    let (handler, memory) = handler_with_memory();
    let client = connect(handler).await;
    // Bootstrap a chunk directly into the memory store as a different
    // caller, so we can attempt a cross-caller delete via IPC.
    let chunk = memory
        .write(cel_memory::NewMemoryChunk {
            kind: cel_memory::ChunkKind::Chat,
            source: ChunkSource::Mcp,
            session_id: None,
            project_root: None,
            caller_id: "mcp:cursor".into(),
            content: "cursor's chunk".into(),
            metadata: serde_json::Value::Null,
            importance: None,
            shareable: false,
            pinned: false,
        })
        .await
        .unwrap();
    let err = client
        .call::<_, MemoryForgetResult>(
            "memory.forget",
            MemoryForgetParams {
                caller_id: Some("mcp:codex".into()),
                chunk_ids: Some(vec![chunk.id.clone()]),
                predicate: None,
            },
        )
        .await
        .unwrap_err();
    let s = format!("{err:?}");
    assert!(
        s.contains("NotAuthorized") || s.contains("not_authorized"),
        "{s}"
    );
    // Chunk is still present.
    assert!(memory.get(&chunk.id).await.unwrap().is_some());
}

#[tokio::test]
async fn forget_predicate_scopes_to_caller_and_emits_evictions() {
    let (handler, memory) = handler_with_memory();
    let client = connect(handler).await;
    // Two cursor chunks + one codex chunk, all matching the same content
    // substring so the unfiltered predicate would catch all three.
    for (caller, content) in [
        ("mcp:cursor", "cursor chunk alpha"),
        ("mcp:cursor", "cursor chunk beta"),
        ("mcp:codex", "codex chunk alpha"),
    ] {
        client
            .call::<_, MemoryRememberResult>(
                "memory.remember",
                MemoryRememberParams {
                    content: content.into(),
                    caller_id: Some(caller.into()),
                    kind: None,
                    session_id: None,
                    project_root: None,
                    tags: None,
                    importance: None,
                    shareable: false,
                    pinned: false,
                },
            )
            .await
            .unwrap();
    }
    // Cursor forgets all of its chunks via predicate matching the tag
    // (substring 'chunk'). Codex's chunk must not be touched.
    let res: MemoryForgetResult = client
        .call(
            "memory.forget",
            MemoryForgetParams {
                caller_id: Some("mcp:cursor".into()),
                chunk_ids: None,
                predicate: Some(MemoryForgetPredicate {
                    kind: None,
                    older_than: None,
                    tag: Some("chunk".into()),
                }),
            },
        )
        .await
        .unwrap();
    assert_eq!(res.deleted, 2);
    let stats = memory.stats().await.unwrap();
    assert_eq!(stats.total_chunks, 1);
    let bundle = memory
        .export(cel_memory::ExportFilter {
            predicate: None,
            include_eviction_log: true,
            include_access_log: false,
            include_sessions: false,
        })
        .await
        .unwrap();
    let user_deletes = bundle
        .evictions
        .iter()
        .filter(|e| e.reason == EvictionReason::UserDelete)
        .count();
    assert_eq!(user_deletes, 2);
}

#[tokio::test]
async fn forget_requires_exactly_one_mode() {
    let (handler, _) = handler_with_memory();
    let client = connect(handler).await;
    // Neither mode is invalid.
    let err = client
        .call::<_, MemoryForgetResult>(
            "memory.forget",
            MemoryForgetParams {
                caller_id: Some("mcp:cursor".into()),
                chunk_ids: None,
                predicate: None,
            },
        )
        .await
        .unwrap_err();
    let s = format!("{err:?}");
    assert!(
        s.to_lowercase().contains("validation") || s.contains("ValidationFailed"),
        "{s}"
    );

    // Both modes is also invalid.
    let err = client
        .call::<_, MemoryForgetResult>(
            "memory.forget",
            MemoryForgetParams {
                caller_id: Some("mcp:cursor".into()),
                chunk_ids: Some(vec!["x".into()]),
                predicate: Some(MemoryForgetPredicate {
                    tag: Some("foo".into()),
                    ..Default::default()
                }),
            },
        )
        .await
        .unwrap_err();
    let s = format!("{err:?}");
    assert!(
        s.to_lowercase().contains("validation") || s.contains("ValidationFailed"),
        "{s}"
    );
}

#[tokio::test]
async fn recall_empty_query_rejected() {
    let (handler, _) = handler_with_memory();
    let client = connect(handler).await;
    let err = client
        .call::<_, MemoryRecallResult>(
            "memory.recall",
            MemoryRecallParams {
                query: "   ".into(),
                caller_id: Some("mcp:cursor".into()),
                limit: None,
                kinds: None,
                scope: None,
                min_importance: None,
                since: None,
                session_id: None,
                project_root_prefix: None,
            },
        )
        .await
        .unwrap_err();
    let s = format!("{err:?}");
    assert!(
        s.to_lowercase().contains("validation") || s.contains("ValidationFailed"),
        "{s}"
    );
}

/// The default `Daemon::wire_subsystems` always installs the memory
/// provider into the agent slot (the embedded agent runtime itself only
/// activates when an LLM is configured, but the memory wire is always on).
/// This test asserts that path serves `memory.remember` end-to-end, so a
/// fresh daemon picks up the Phase 4 surface without any extra wiring.
#[tokio::test]
async fn default_daemon_serves_memory_remember() {
    let daemon = Daemon::wire_subsystems();
    let handler = Arc::clone(&daemon.ipc_handler);
    let client = connect(handler).await;
    let res: MemoryRememberResult = client
        .call(
            "memory.remember",
            MemoryRememberParams {
                content: "smoke".into(),
                caller_id: Some("mcp:cursor".into()),
                kind: None,
                session_id: None,
                project_root: None,
                tags: None,
                importance: None,
                shareable: false,
                pinned: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(res.chunk["caller_id"], "mcp:cursor");
    assert_eq!(res.chunk["source"], "mcp");
}
