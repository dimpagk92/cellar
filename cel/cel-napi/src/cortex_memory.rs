//! N-API exports for the cortex memory store.
//!
//! Thin pass-through to `cel_store::cortex_memory` via the cached
//! [`with_store`] helper used by the rest of cel-napi. Each function takes
//! the SQLite path as the first argument so the JS layer doesn't have to
//! manage a connection handle.
//!
//! All write operations are explicit. Auto-write paths (checkpoint outcome,
//! canonical-runner final outcome) live higher up — in
//! `mcp-server/src/tools/cel-perceive.ts` and
//! `cel-goal-runner/src/canonical_runner.rs` — and call into the same
//! `CelStore::insert_cortex_memory` wrapper. The JS surface here is what
//! the **explicit** `cel_think` MCP modes hit.

use std::collections::HashMap;
use std::sync::Mutex;

use napi_derive::napi;

use cel_store::{
    cortex_memory::{
        CortexMemory, MemoryKind, NewCortexMemory,
    },
    CelStore, StoreError,
};

// Reuse the same store cache as the other cel-napi modules — sharing the
// cache means migrations only run once per `db_path` per process.
static STORE_CACHE: std::sync::OnceLock<Mutex<HashMap<String, CelStore>>> =
    std::sync::OnceLock::new();

fn with_store<F, R>(db_path: &str, f: F) -> napi::Result<R>
where
    F: FnOnce(&CelStore) -> Result<R, StoreError>,
{
    let cache = STORE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = cache
        .lock()
        .map_err(|e| napi::Error::from_reason(format!("Store lock poisoned: {}", e)))?;
    if !map.contains_key(db_path) {
        let store = CelStore::open(db_path).map_err(|e| napi::Error::from_reason(e.to_string()))?;
        map.insert(db_path.to_string(), store);
    }
    let store = map
        .get(db_path)
        .ok_or_else(|| napi::Error::from_reason(format!("Store not found for path: {}", db_path)))?;
    f(store).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Insert a new cortex memory record. Returns the row id (i64).
///
/// `payload_json` is a serialized [`NewCortexMemory`]:
/// ```json
/// {
///   "workflow_id": "concur-expense",
///   "kind": "outcome" | "prior" | "failure" | "preference",
///   "content": { ... structured per kind ... },
///   "summary": "<optional one-liner>",
///   "tags": ["<optional>", "..."],
///   "source_ref": "<optional>",
///   "embedding": null
/// }
/// ```
#[napi]
pub fn cortex_memory_insert(db_path: String, payload_json: String) -> napi::Result<i64> {
    let payload: NewCortexMemory = serde_json::from_str(&payload_json)
        .map_err(|e| napi::Error::from_reason(format!("Invalid memory payload: {e}")))?;
    with_store(&db_path, |s| s.insert_cortex_memory(&payload))
}

/// List cortex memories for a workflow, most-recent-first.
///
/// `kinds_json` is an optional serialized JSON array of memory-kind strings
/// (`["outcome", "failure"]`); `null` or `"null"` matches any kind.
#[napi]
pub fn cortex_memory_list(
    db_path: String,
    workflow_id: String,
    kinds_json: Option<String>,
    limit: u32,
) -> napi::Result<String> {
    let kinds_owned: Option<Vec<MemoryKind>> = match kinds_json.as_deref() {
        Some(s) if !s.trim().is_empty() && s.trim() != "null" => {
            let raw: Vec<String> = serde_json::from_str(s)
                .map_err(|e| napi::Error::from_reason(format!("Invalid kinds JSON: {e}")))?;
            let parsed: Result<Vec<MemoryKind>, _> =
                raw.iter().map(|k| MemoryKind::parse(k)).collect();
            Some(parsed.map_err(|e| napi::Error::from_reason(e.to_string()))?)
        }
        _ => None,
    };
    let kinds_ref: Option<&[MemoryKind]> = kinds_owned.as_deref();
    with_store(&db_path, |s| {
        let memories: Vec<CortexMemory> =
            s.list_cortex_memories(&workflow_id, kinds_ref, limit as usize)?;
        serde_json::to_string(&memories).map_err(StoreError::Serialization)
    })
}

/// Free-text search over cortex memories' summary + content.
/// Returns memories as a serialized JSON array (most-recent-first).
#[napi]
pub fn cortex_memory_search(
    db_path: String,
    workflow_id: String,
    query: String,
    limit: u32,
) -> napi::Result<String> {
    with_store(&db_path, |s| {
        let memories: Vec<CortexMemory> =
            s.search_cortex_memory(&workflow_id, &query, limit as usize)?;
        serde_json::to_string(&memories).map_err(StoreError::Serialization)
    })
}

/// Fetch one cortex memory by id, updating `last_accessed_at` to now.
/// Returns serialized memory JSON, or the literal string `"null"` if the id
/// doesn't exist.
#[napi]
pub fn cortex_memory_touch(db_path: String, id: i64) -> napi::Result<String> {
    with_store(&db_path, |s| {
        let memory: Option<CortexMemory> = s.touch_cortex_memory(id)?;
        serde_json::to_string(&memory).map_err(StoreError::Serialization)
    })
}

/// Prune cortex memories whose decay score falls below `threshold`.
/// Returns the number of deleted rows. See
/// `cel_store::cortex_memory::prune_memories` for threshold guidance —
/// the plan recommends `0.01` to clear long-stale memories (cuts at
/// roughly 20 months given the default 90-day half-life).
#[napi]
pub fn cortex_memory_prune(db_path: String, threshold: f64) -> napi::Result<u32> {
    let n = with_store(&db_path, |s| s.prune_cortex_memories(threshold))?;
    Ok(n as u32)
}
