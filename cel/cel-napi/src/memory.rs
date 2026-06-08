//! N-API exports for the Cellar Memory subsystem.
//!
//! Backs the `cel_remember`, `cel_recall`, `cel_forget` MCP tools by
//! opening a [`SqliteMemoryProvider`] against the daemon's
//! `~/.cellar/memory.sqlite` file. SQLite's WAL mode allows the daemon
//! and this process to share the database without contention beyond
//! brief writer-lock windows.
//!
//! Each JS-visible call takes a `db_path` so the binding can be repointed
//! in tests; the production MCP server passes `~/.cellar/memory.sqlite`
//! (the same path `cel-cortex-daemon` writes to).
//!
//! Embeddings: this binding uses [`MockEmbedder`] so the MCP-tool path
//! doesn't pull in the ~130 MB ONNX model the fastembed feature requires.
//! The daemon's own writes (embedded agent, gateway, matcher) use the real
//! embedder, so a chunk written here participates in FTS retrieval
//! immediately and in vector retrieval as soon as it's re-embedded (Phase
//! 4 backlog: shared embedder over IPC).

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use cel_memory::{
    CallerScope, ChunkKind, ChunkSource, EvictionReason, MemoryPredicate, MemoryProvider,
    MemoryQuery, NewMemoryChunk, RetrievalProfile,
};
use cel_memory_sqlite::{MockEmbedder, SqliteMemoryProvider};
use napi_derive::napi;
use tokio::sync::Mutex;

/// Per-process cache of opened providers, keyed by absolute DB path. The
/// MCP server typically uses one path so this is effectively a singleton;
/// caching keeps the migration run + sqlite-vec load to once per process.
static PROVIDER_CACHE: OnceLock<Mutex<HashMap<String, Arc<SqliteMemoryProvider>>>> =
    OnceLock::new();

/// Resolve or open the provider for `db_path`. The first call per path
/// runs migrations; subsequent calls reuse the cached `Arc`.
async fn provider(db_path: &str) -> napi::Result<Arc<SqliteMemoryProvider>> {
    let cache = PROVIDER_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let map = cache.lock().await;
        if let Some(p) = map.get(db_path) {
            return Ok(Arc::clone(p));
        }
    }
    // Open outside the cache lock so the relatively expensive migration
    // step doesn't serialize across concurrent first-touch callers.
    let embedder = Arc::new(MockEmbedder::new());
    let p = SqliteMemoryProvider::open(db_path, embedder)
        .await
        .map_err(|e| napi::Error::from_reason(format!("memory open: {e}")))?;
    let arc = Arc::new(p);
    let mut map = cache.lock().await;
    Ok(Arc::clone(map.entry(db_path.to_string()).or_insert(arc)))
}

/// Run a closure against the provider for `db_path` from a sync N-API
/// context by blocking the shared tokio runtime.
fn run_blocking<F, T>(fut: F) -> napi::Result<T>
where
    F: std::future::Future<Output = napi::Result<T>>,
{
    crate::rt_handle()?.block_on(fut)
}

/// `cel_remember` backing call. Returns the persisted chunk as a JSON
/// string. The caller is responsible for `caller_id` normalisation
/// (`mcp:<client>`) — see `mcp-server/src/tools/cel-remember.ts`.
///
/// `payload_json` is a [`NewMemoryChunk`] minus the provider-assigned
/// fields:
/// ```json
/// {
///   "kind": "chat" | "action" | ...,
///   "source": "mcp",
///   "caller_id": "mcp:cursor",
///   "content": "<text>",
///   "session_id": null,
///   "project_root": null,
///   "metadata": { "tags": ["..."] },
///   "importance": null,
///   "shareable": false,
///   "pinned": false
/// }
/// ```
#[napi]
pub fn memory_remember(db_path: String, payload_json: String) -> napi::Result<String> {
    let new_chunk: NewMemoryChunk = serde_json::from_str(&payload_json)
        .map_err(|e| napi::Error::from_reason(format!("invalid remember payload: {e}")))?;
    run_blocking(async move {
        let p = provider(&db_path).await?;
        let chunk = p
            .write(new_chunk)
            .await
            .map_err(|e| napi::Error::from_reason(format!("memory write: {e}")))?;
        serde_json::to_string(&chunk)
            .map_err(|e| napi::Error::from_reason(format!("serialize chunk: {e}")))
    })
}

/// `cel_recall` backing call. Returns a JSON array of chunks.
///
/// `query_json` is a [`MemoryQuery`]:
/// ```json
/// {
///   "text": "Q4 report",
///   "caller_id": "mcp:cursor",
///   "caller_scope": "own" | "own_plus_shared" | "global",
///   "k": 8,
///   "kinds": null,
///   "session_id": null,
///   "min_importance": null,
///   "profile": "agent_chat_turn",
///   "include_rollups": true
/// }
/// ```
#[napi]
pub fn memory_recall(db_path: String, query_json: String) -> napi::Result<String> {
    let query: MemoryQuery = serde_json::from_str(&query_json)
        .map_err(|e| napi::Error::from_reason(format!("invalid recall query: {e}")))?;
    run_blocking(async move {
        let p = provider(&db_path).await?;
        let hits = p
            .retrieve(query)
            .await
            .map_err(|e| napi::Error::from_reason(format!("memory retrieve: {e}")))?;
        serde_json::to_string(&hits)
            .map_err(|e| napi::Error::from_reason(format!("serialize hits: {e}")))
    })
}

/// `cel_forget` backing call — predicate path. Returns count deleted.
///
/// `predicate_json` is a [`MemoryPredicate`]. The MCP tool layer always
/// adds the caller's `caller_id` to `callers` so a client can't mass-
/// delete another caller's history. Empty predicates short-circuit to
/// zero (mirrors [`MemoryProvider::delete_matching`] semantics).
#[napi]
pub fn memory_forget_matching(db_path: String, predicate_json: String) -> napi::Result<u32> {
    let predicate: MemoryPredicate = serde_json::from_str(&predicate_json)
        .map_err(|e| napi::Error::from_reason(format!("invalid forget predicate: {e}")))?;
    run_blocking(async move {
        let p = provider(&db_path).await?;
        let n = p
            .delete_matching(predicate, EvictionReason::UserDelete)
            .await
            .map_err(|e| napi::Error::from_reason(format!("memory delete_matching: {e}")))?;
        Ok(n as u32)
    })
}

/// `cel_forget` backing call — id-list path. Returns count of chunks
/// actually deleted. Callers passed in but not owned by `caller_id` are
/// reported back as `0` (not-authorized handling lives in the MCP tool).
///
/// The function reads ownership row-by-row first; any chunk not owned by
/// `caller_id` is left in place. NotFound is silently skipped (idempotent
/// forget).
#[napi]
pub fn memory_forget_ids(
    db_path: String,
    caller_id: String,
    ids_json: String,
) -> napi::Result<u32> {
    let ids: Vec<String> = serde_json::from_str(&ids_json)
        .map_err(|e| napi::Error::from_reason(format!("invalid ids json: {e}")))?;
    run_blocking(async move {
        let p = provider(&db_path).await?;
        let mut deleted = 0u32;
        for id in ids {
            let owned = match p
                .get(&id)
                .await
                .map_err(|e| napi::Error::from_reason(format!("memory get: {e}")))?
            {
                Some(c) => c.caller_id == caller_id,
                None => continue,
            };
            if !owned {
                continue;
            }
            p.delete(&id, EvictionReason::UserDelete)
                .await
                .map_err(|e| napi::Error::from_reason(format!("memory delete: {e}")))?;
            deleted += 1;
        }
        Ok(deleted)
    })
}

/// `cel_recall` convenience helper — build the JSON for a `MemoryQuery`
/// from the small parameter set the MCP tool layer surfaces.
///
/// JS callers can construct the JSON directly; this is exposed as a thin
/// helper so the TypeScript surface stays small. Not strictly required.
#[allow(dead_code)]
#[napi]
pub fn memory_recall_quick(
    db_path: String,
    text: String,
    caller_id: String,
    scope: String,
    limit: u32,
) -> napi::Result<String> {
    let caller_scope = match scope.as_str() {
        "own" => CallerScope::Own,
        "own_plus_shared" => CallerScope::OwnPlusShared,
        "global" => CallerScope::Global,
        other => return Err(napi::Error::from_reason(format!("unknown scope: {other}"))),
    };
    let q = MemoryQuery {
        text,
        kinds: None,
        since: None,
        until: None,
        session_id: None,
        caller_scope,
        project_root_prefix: None,
        k: (limit as usize).max(1),
        include_rollups: true,
        min_importance: None,
        profile: RetrievalProfile::AgentChatTurn,
        caller_id,
    };
    run_blocking(async move {
        let p = provider(&db_path).await?;
        let hits = p
            .retrieve(q)
            .await
            .map_err(|e| napi::Error::from_reason(format!("memory retrieve: {e}")))?;
        serde_json::to_string(&hits)
            .map_err(|e| napi::Error::from_reason(format!("serialize hits: {e}")))
    })
}

/// `cel_remember` convenience helper for the simple text-only path.
#[allow(dead_code)]
#[napi]
pub fn memory_remember_quick(
    db_path: String,
    caller_id: String,
    content: String,
    kind: String,
    shareable: bool,
) -> napi::Result<String> {
    let kind = match kind.as_str() {
        "chat" => ChunkKind::Chat,
        "action" => ChunkKind::Action,
        "observation" => ChunkKind::Observation,
        "correction" => ChunkKind::Correction,
        "context" => ChunkKind::Context,
        "job_summary" => ChunkKind::JobSummary,
        "fire" => ChunkKind::Fire,
        "rollup" => ChunkKind::Rollup,
        other => return Err(napi::Error::from_reason(format!("unknown kind: {other}"))),
    };
    let nc = NewMemoryChunk {
        kind,
        source: ChunkSource::Mcp,
        session_id: None,
        project_root: None,
        caller_id,
        content,
        metadata: serde_json::Value::Null,
        importance: None,
        shareable,
        pinned: false,
    };
    run_blocking(async move {
        let p = provider(&db_path).await?;
        let chunk = p
            .write(nc)
            .await
            .map_err(|e| napi::Error::from_reason(format!("memory write: {e}")))?;
        serde_json::to_string(&chunk)
            .map_err(|e| napi::Error::from_reason(format!("serialize chunk: {e}")))
    })
}
