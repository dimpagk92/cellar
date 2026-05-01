use napi_derive::napi;
use std::collections::HashMap;
use std::sync::Mutex;

/// Persistent store cache — avoids re-opening SQLite and re-running migrations on every call.
static STORE_CACHE: std::sync::OnceLock<Mutex<HashMap<String, cel_store::CelStore>>> =
    std::sync::OnceLock::new();

fn with_store<F, R>(db_path: &str, f: F) -> napi::Result<R>
where
    F: FnOnce(&cel_store::CelStore) -> Result<R, cel_store::StoreError>,
{
    let cache = STORE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = cache
        .lock()
        .map_err(|e| napi::Error::from_reason(format!("Store lock poisoned: {}", e)))?;
    if !map.contains_key(db_path) {
        let store = cel_store::CelStore::open(db_path)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        map.insert(db_path.to_string(), store);
    }
    let store = map.get(db_path).ok_or_else(|| {
        napi::Error::from_reason(format!("Store not found for path: {}", db_path))
    })?;
    f(store).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Query knowledge facts by keyword. Returns JSON string.
#[napi]
pub fn query_knowledge(db_path: String, query: String) -> napi::Result<String> {
    with_store(&db_path, |s| {
        let facts = s.query_knowledge(&query)?;
        serde_json::to_string(&facts).map_err(cel_store::StoreError::Serialization)
    })
}

/// Add a knowledge fact. Returns the row ID.
#[napi]
pub fn add_knowledge(db_path: String, content: String, source: String) -> napi::Result<i64> {
    with_store(&db_path, |s| s.add_knowledge(&content, &source))
}

/// Start a workflow run. Returns the run ID.
#[napi]
pub fn start_run(db_path: String, workflow_name: String, steps_total: u32) -> napi::Result<i64> {
    with_store(&db_path, |s| s.start_run(&workflow_name, steps_total))
}

/// Finish a workflow run.
#[napi]
pub fn finish_run(db_path: String, run_id: i64, status: String) -> napi::Result<()> {
    with_store(&db_path, |s| s.finish_run(run_id, &status))
}

/// Log a step result during a workflow run. Returns the step row ID.
#[napi]
pub fn log_step(
    db_path: String,
    run_id: i64,
    step_index: u32,
    step_id: String,
    action: String,
    success: bool,
    confidence: f64,
    context_snapshot: Option<String>,
    error: Option<String>,
) -> napi::Result<i64> {
    with_store(&db_path, |s| {
        s.log_step(
            run_id,
            step_index,
            &step_id,
            &action,
            success,
            confidence,
            context_snapshot.as_deref(),
            error.as_deref(),
        )
    })
}

/// Get run history, most recent first. Returns JSON string.
#[napi]
pub fn get_run_history(db_path: String, limit: u32) -> napi::Result<String> {
    with_store(&db_path, |s| {
        let history = s.get_run_history(limit)?;
        serde_json::to_string(&history).map_err(cel_store::StoreError::Serialization)
    })
}

/// Get step results for a specific run. Returns JSON string.
#[napi]
pub fn get_step_results(db_path: String, run_id: i64) -> napi::Result<String> {
    with_store(&db_path, |s| {
        let steps = s.get_step_results(run_id)?;
        serde_json::to_string(&steps).map_err(cel_store::StoreError::Serialization)
    })
}

/// Get working memory for a workflow. Returns JSON string.
#[napi]
pub fn get_working_memory(db_path: String, workflow_name: String) -> napi::Result<String> {
    with_store(&db_path, |s| {
        let wm = s.get_working_memory(&workflow_name)?;
        serde_json::to_string(&wm).map_err(cel_store::StoreError::Serialization)
    })
}

/// Update working memory for a workflow.
#[napi]
pub fn update_working_memory(
    db_path: String,
    workflow_name: String,
    content: String,
) -> napi::Result<()> {
    with_store(&db_path, |s| {
        s.update_working_memory(&workflow_name, &content)
    })
}

/// Add an observation. Returns the observation ID.
#[napi]
pub fn add_observation(
    db_path: String,
    workflow_name: String,
    content: String,
    priority: String,
    source_run_ids: Vec<i64>,
) -> napi::Result<i64> {
    let p = match priority.as_str() {
        "high" => cel_store::ObservationPriority::High,
        "low" => cel_store::ObservationPriority::Low,
        _ => cel_store::ObservationPriority::Medium,
    };
    with_store(&db_path, |s| {
        s.add_observation(&workflow_name, &content, &p, &source_run_ids, None, None)
    })
}

/// Get active observations for a workflow. Returns JSON string.
#[napi]
pub fn get_observations(
    db_path: String,
    workflow_name: String,
    limit: u32,
) -> napi::Result<String> {
    with_store(&db_path, |s| {
        let obs = s.get_observations(&workflow_name, limit)?;
        serde_json::to_string(&obs).map_err(cel_store::StoreError::Serialization)
    })
}

/// Full-text search over knowledge using FTS5. Returns JSON string.
#[napi]
pub fn search_knowledge(
    db_path: String,
    query: String,
    workflow_scope: Option<String>,
    limit: u32,
) -> napi::Result<String> {
    with_store(&db_path, |s| {
        let results = s.search_knowledge(&query, workflow_scope.as_deref(), limit)?;
        serde_json::to_string(&results).map_err(cel_store::StoreError::Serialization)
    })
}

/// Run TTL eviction policies. Returns JSON with deleted row counts.
#[napi]
pub fn run_eviction(
    db_path: String,
    run_retention_days: u32,
    knowledge_retention_days: u32,
) -> napi::Result<String> {
    let config = cel_store::EvictionConfig {
        run_retention_days,
        knowledge_retention_days,
    };
    with_store(&db_path, |s| {
        let result = s.run_eviction(&config)?;
        serde_json::to_string(&result).map_err(cel_store::StoreError::Serialization)
    })
}

/// Add a scoped knowledge fact. Returns the row ID.
#[napi]
pub fn add_scoped_knowledge(
    db_path: String,
    content: String,
    source: String,
    workflow_scope: Option<String>,
    tags: Option<String>,
) -> napi::Result<i64> {
    with_store(&db_path, |s| {
        s.add_scoped_knowledge(
            &content,
            &source,
            workflow_scope.as_deref(),
            tags.as_deref(),
        )
    })
}
