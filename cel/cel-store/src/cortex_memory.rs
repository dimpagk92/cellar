//! Cortex memory — durable, workflow-scoped memory the cortex selector
//! can hydrate into a `PlanningView`.
//!
//! This is the **storage** layer. The selection layer (PR3) reads from here;
//! the cognition runtime (PR4 if justified) writes from here. The
//! `cortex_memories` table is explicitly separate from `observations` and
//! `working_memory` — those serve other purposes (compressed run summaries
//! and per-workflow scratchpads). Cortex memories are the host's durable
//! priors that survive cortex restarts.
//!
//! Privacy: writes are opt-in. The MCP `cel_perceive start` only auto-writes
//! when the caller passes `enable_memory: true` plus a `workflow_id`. The
//! explicit `cel_think store_memory` mode requires the caller to name the
//! workflow.
//!
//! Decay: exponential half-life with a 90-day default. A 90-day-old memory
//! scores 0.5; a 365-day-old memory scores ~0.06. Decay never hard-deletes
//! — it influences ranking and pruning, never selection eligibility.

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::StoreError;

// ─── Types ───────────────────────────────────────────────────────────────────

/// Memory kind — discriminates the structured `content` payload.
///
/// See `COGNITION_LAYER_PLAN.md` (`Memory Schema` section) for the full
/// content shapes per kind.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    /// What happened — replayable action + result + ts.
    Outcome,
    /// A generalisation derived from one or more outcomes.
    Prior,
    /// Something to avoid (with workaround if known).
    Failure,
    /// User preference — informs future planning.
    Preference,
}

impl MemoryKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Outcome => "outcome",
            Self::Prior => "prior",
            Self::Failure => "failure",
            Self::Preference => "preference",
        }
    }

    pub fn parse(s: &str) -> Result<Self, StoreError> {
        match s {
            "outcome" => Ok(Self::Outcome),
            "prior" => Ok(Self::Prior),
            "failure" => Ok(Self::Failure),
            "preference" => Ok(Self::Preference),
            other => Err(StoreError::NotFound(format!(
                "unknown memory kind: {other}"
            ))),
        }
    }
}

/// A single cortex memory record as stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CortexMemory {
    pub id: i64,
    pub workflow_id: String,
    pub kind: MemoryKind,
    /// Structured payload, shape depends on `kind`.
    pub content: serde_json::Value,
    /// Optional one-line summary for the catalog (selector pre-filter input).
    #[serde(default)]
    pub summary: Option<String>,
    /// Optional tags for retrieval — populated by the tag-generator enricher
    /// in PR4 if it lands.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Optional reference to the source record (transcript span,
    /// checkpoint id, adapter fact id) so selection can hydrate evidence.
    #[serde(default)]
    pub source_ref: Option<String>,
    /// Unix epoch seconds when the memory was first written.
    pub created_at: i64,
    /// Unix epoch seconds when the memory was last hydrated by the selector.
    pub last_accessed_at: i64,
}

/// Insert payload — caller-supplied fields. Server fills `id`,
/// `created_at`, `last_accessed_at`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewCortexMemory {
    pub workflow_id: String,
    pub kind: MemoryKind,
    pub content: serde_json::Value,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub source_ref: Option<String>,
    /// Optional embedding for vector pre-filter (PR3). NULL when the
    /// embedder is unavailable; selection falls back to keyword search.
    #[serde(default)]
    pub embedding: Option<Vec<u8>>,
}

// ─── Migration ───────────────────────────────────────────────────────────────

/// Idempotent — safe to call on every startup. Creates the table and the
/// two indexes the selector + pruner need.
pub fn migrate_cortex_memories(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS cortex_memories (
            id               INTEGER PRIMARY KEY AUTOINCREMENT,
            workflow_id      TEXT NOT NULL,
            kind             TEXT NOT NULL,
            content          TEXT NOT NULL,
            summary          TEXT,
            tags             TEXT,
            embedding        BLOB,
            source_ref       TEXT,
            created_at       INTEGER NOT NULL,
            last_accessed_at INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_cortex_memories_workflow
            ON cortex_memories(workflow_id, created_at DESC);

        CREATE INDEX IF NOT EXISTS idx_cortex_memories_age
            ON cortex_memories(created_at);
        ",
    )?;
    Ok(())
}

// ─── CRUD ────────────────────────────────────────────────────────────────────

/// Insert a new memory. Returns the newly-allocated id.
///
/// `created_at` and `last_accessed_at` are set to `now_secs`, lettings
/// callers (and tests) inject deterministic timestamps.
pub fn insert_memory(
    conn: &Connection,
    m: &NewCortexMemory,
    now_secs: i64,
) -> Result<i64, StoreError> {
    let content_json = serde_json::to_string(&m.content)?;
    let tags_json = if m.tags.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&m.tags)?)
    };
    conn.execute(
        "INSERT INTO cortex_memories
            (workflow_id, kind, content, summary, tags, embedding, source_ref,
             created_at, last_accessed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
        params![
            m.workflow_id,
            m.kind.as_str(),
            content_json,
            m.summary,
            tags_json,
            m.embedding,
            m.source_ref,
            now_secs,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// List memories for a workflow, most-recent-first, capped at `limit`.
/// Optionally filter by `kinds` (any kind matches when `None`).
pub fn list_memories(
    conn: &Connection,
    workflow_id: &str,
    kinds: Option<&[MemoryKind]>,
    limit: usize,
) -> Result<Vec<CortexMemory>, StoreError> {
    let limit_i64 = limit as i64;
    if let Some(ks) = kinds {
        if ks.is_empty() {
            return Ok(Vec::new());
        }
        // Build a parameterised IN clause — small, bounded list.
        let placeholders = ks.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let sql = format!(
            "SELECT id, workflow_id, kind, content, summary, tags, source_ref,
                    created_at, last_accessed_at
             FROM cortex_memories
             WHERE workflow_id = ?1 AND kind IN ({placeholders})
             ORDER BY created_at DESC
             LIMIT ?{}",
            ks.len() + 2
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut params_vec: Vec<&dyn rusqlite::ToSql> = vec![&workflow_id];
        let kind_strs: Vec<&str> = ks.iter().map(|k| k.as_str()).collect();
        for k in &kind_strs {
            params_vec.push(k);
        }
        params_vec.push(&limit_i64);
        let rows = stmt.query_map(rusqlite::params_from_iter(params_vec), row_to_memory)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)?
            .into_iter()
            .map(Ok)
            .collect()
    } else {
        let mut stmt = conn.prepare(
            "SELECT id, workflow_id, kind, content, summary, tags, source_ref,
                    created_at, last_accessed_at
             FROM cortex_memories
             WHERE workflow_id = ?1
             ORDER BY created_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![workflow_id, limit_i64], row_to_memory)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)?
            .into_iter()
            .map(Ok)
            .collect()
    }
}

/// Fetch one memory by id. Updates `last_accessed_at` to `now_secs`.
/// Returns `None` if the row doesn't exist.
pub fn touch_memory(
    conn: &Connection,
    id: i64,
    now_secs: i64,
) -> Result<Option<CortexMemory>, StoreError> {
    conn.execute(
        "UPDATE cortex_memories SET last_accessed_at = ?1 WHERE id = ?2",
        params![now_secs, id],
    )?;
    let mut stmt = conn.prepare(
        "SELECT id, workflow_id, kind, content, summary, tags, source_ref,
                created_at, last_accessed_at
         FROM cortex_memories
         WHERE id = ?1",
    )?;
    let row = stmt.query_row(params![id], row_to_memory).optional()?;
    Ok(row)
}

/// Free-text search by `summary` (case-insensitive substring). Sufficient
/// for v1; PR3 may upgrade to FTS5 if recall quality demands it. Returns
/// the most-recent matches first, capped at `limit`.
pub fn search_memory(
    conn: &Connection,
    workflow_id: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<CortexMemory>, StoreError> {
    let pattern = format!("%{}%", query.trim());
    let limit_i64 = limit as i64;
    let mut stmt = conn.prepare(
        "SELECT id, workflow_id, kind, content, summary, tags, source_ref,
                created_at, last_accessed_at
         FROM cortex_memories
         WHERE workflow_id = ?1
           AND (summary LIKE ?2 COLLATE NOCASE
                OR content LIKE ?2 COLLATE NOCASE)
         ORDER BY created_at DESC
         LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![workflow_id, pattern, limit_i64], row_to_memory)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)?
        .into_iter()
        .map(Ok)
        .collect()
}

/// Prune memories whose decay score (computed at `now_secs` against
/// `last_accessed_at`) falls below `threshold`. Returns the number of
/// deleted rows.
///
/// `threshold = 0.0` keeps everything; `threshold = 1.0` deletes
/// everything older than an instant. With the 90-day half-life:
///
///   - `0.5` cuts at ~3 months
///   - `0.125` cuts at ~9 months
///   - `0.01` cuts at ~20 months (about 1.6 years)
///
/// The plan recommends `0.01` as a sensible "stop tracking long-stale
/// memories" default; tighter thresholds prune sooner.
pub fn prune_memories(
    conn: &Connection,
    threshold: f64,
    now_secs: i64,
) -> Result<usize, StoreError> {
    if threshold <= 0.0 {
        return Ok(0);
    }
    // Pull candidate ids sorted by created_at ASC (oldest first), compute
    // decay in Rust, delete those below threshold. Avoids requiring SQLite
    // math extensions for `EXP`/`LN`.
    let mut stmt = conn.prepare(
        "SELECT id, last_accessed_at
         FROM cortex_memories
         ORDER BY last_accessed_at ASC",
    )?;
    let candidates: Vec<(i64, i64)> = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;

    let mut to_delete: Vec<i64> = Vec::new();
    for (id, ts) in candidates {
        if decay_score(ts, now_secs) < threshold {
            to_delete.push(id);
        }
    }
    let mut deleted = 0usize;
    for id in to_delete {
        let n = conn.execute("DELETE FROM cortex_memories WHERE id = ?1", params![id])?;
        deleted += n;
    }
    Ok(deleted)
}

// ─── Decay ───────────────────────────────────────────────────────────────────

/// Half-life for memory decay, in days.
pub const DEFAULT_HALF_LIFE_DAYS: f64 = 90.0;

/// Compute the decay score for a memory of `created_at` at the moment
/// `now_secs`. Score is in `(0, 1]`; never reaches 0 in finite time.
///
/// `score = exp(-ln(2) * age_days / half_life_days)`
///
/// At age = half_life: 0.5. At age = 2 × half_life: 0.25. Etc.
pub fn decay_score(created_at_secs: i64, now_secs: i64) -> f64 {
    let age_secs = (now_secs - created_at_secs).max(0) as f64;
    let age_days = age_secs / 86_400.0;
    let half_life = DEFAULT_HALF_LIFE_DAYS;
    (-(std::f64::consts::LN_2) * age_days / half_life).exp()
}

/// Convenience — current Unix epoch seconds.
pub fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ─── Row mapping ─────────────────────────────────────────────────────────────

fn row_to_memory(row: &Row<'_>) -> rusqlite::Result<CortexMemory> {
    let id: i64 = row.get(0)?;
    let workflow_id: String = row.get(1)?;
    let kind_str: String = row.get(2)?;
    let kind = MemoryKind::parse(&kind_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(e.to_string())),
        )
    })?;
    let content_str: String = row.get(3)?;
    let content: serde_json::Value = serde_json::from_str(&content_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let summary: Option<String> = row.get(4)?;
    let tags_raw: Option<String> = row.get(5)?;
    let tags: Vec<String> = tags_raw
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
        })?
        .unwrap_or_default();
    let source_ref: Option<String> = row.get(6)?;
    let created_at: i64 = row.get(7)?;
    let last_accessed_at: i64 = row.get(8)?;
    Ok(CortexMemory {
        id,
        workflow_id,
        kind,
        content,
        summary,
        tags,
        source_ref,
        created_at,
        last_accessed_at,
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn open_test_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        migrate_cortex_memories(&conn).expect("migrate");
        conn
    }

    fn outcome(text: &str) -> serde_json::Value {
        serde_json::json!({
            "kind": "outcome",
            "action": "click",
            "target": text,
            "result": "ok",
        })
    }

    #[test]
    fn insert_then_list_round_trips() {
        let conn = open_test_db();
        let now = 1_700_000_000;
        let id = insert_memory(
            &conn,
            &NewCortexMemory {
                workflow_id: "wf-1".into(),
                kind: MemoryKind::Outcome,
                content: outcome("Save"),
                summary: Some("clicked Save".into()),
                tags: vec!["form".into(), "save".into()],
                source_ref: Some("transcript:42".into()),
                embedding: None,
            },
            now,
        )
        .unwrap();
        assert!(id > 0);

        let items = list_memories(&conn, "wf-1", None, 10).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, id);
        assert_eq!(items[0].kind, MemoryKind::Outcome);
        assert_eq!(items[0].summary.as_deref(), Some("clicked Save"));
        assert_eq!(items[0].tags, vec!["form".to_string(), "save".to_string()]);
        assert_eq!(items[0].created_at, now);
        assert_eq!(items[0].last_accessed_at, now);
    }

    #[test]
    fn list_orders_most_recent_first() {
        let conn = open_test_db();
        for (i, ts) in [(1, 1_700_000_000), (2, 1_700_001_000), (3, 1_699_999_000)] {
            insert_memory(
                &conn,
                &NewCortexMemory {
                    workflow_id: "wf-1".into(),
                    kind: MemoryKind::Outcome,
                    content: outcome(&format!("ev{i}")),
                    summary: Some(format!("ev{i}")),
                    tags: vec![],
                    source_ref: None,
                    embedding: None,
                },
                ts,
            )
            .unwrap();
        }
        let items = list_memories(&conn, "wf-1", None, 10).unwrap();
        let summaries: Vec<&str> = items.iter().filter_map(|m| m.summary.as_deref()).collect();
        assert_eq!(summaries, vec!["ev2", "ev1", "ev3"]);
    }

    #[test]
    fn list_filters_by_kind() {
        let conn = open_test_db();
        let now = 1_700_000_000;
        for (kind, summary) in [
            (MemoryKind::Outcome, "out"),
            (MemoryKind::Failure, "fail"),
            (MemoryKind::Preference, "pref"),
        ] {
            insert_memory(
                &conn,
                &NewCortexMemory {
                    workflow_id: "wf".into(),
                    kind,
                    content: serde_json::json!({}),
                    summary: Some(summary.into()),
                    tags: vec![],
                    source_ref: None,
                    embedding: None,
                },
                now,
            )
            .unwrap();
        }
        let only_failures = list_memories(&conn, "wf", Some(&[MemoryKind::Failure]), 10).unwrap();
        assert_eq!(only_failures.len(), 1);
        assert_eq!(only_failures[0].kind, MemoryKind::Failure);

        let two = list_memories(
            &conn,
            "wf",
            Some(&[MemoryKind::Outcome, MemoryKind::Preference]),
            10,
        )
        .unwrap();
        assert_eq!(two.len(), 2);
    }

    #[test]
    fn list_scopes_to_workflow() {
        let conn = open_test_db();
        let now = 1_700_000_000;
        for wf in ["a", "b", "a"] {
            insert_memory(
                &conn,
                &NewCortexMemory {
                    workflow_id: wf.into(),
                    kind: MemoryKind::Outcome,
                    content: serde_json::json!({}),
                    summary: None,
                    tags: vec![],
                    source_ref: None,
                    embedding: None,
                },
                now,
            )
            .unwrap();
        }
        assert_eq!(list_memories(&conn, "a", None, 10).unwrap().len(), 2);
        assert_eq!(list_memories(&conn, "b", None, 10).unwrap().len(), 1);
        assert_eq!(list_memories(&conn, "c", None, 10).unwrap().len(), 0);
    }

    #[test]
    fn touch_updates_last_accessed_at() {
        let conn = open_test_db();
        let written_at = 1_700_000_000;
        let id = insert_memory(
            &conn,
            &NewCortexMemory {
                workflow_id: "wf".into(),
                kind: MemoryKind::Outcome,
                content: serde_json::json!({}),
                summary: None,
                tags: vec![],
                source_ref: None,
                embedding: None,
            },
            written_at,
        )
        .unwrap();
        let touched_at = written_at + 86_400; // one day later
        let m = touch_memory(&conn, id, touched_at).unwrap().unwrap();
        assert_eq!(m.created_at, written_at);
        assert_eq!(m.last_accessed_at, touched_at);
    }

    #[test]
    fn touch_returns_none_for_unknown_id() {
        let conn = open_test_db();
        assert!(touch_memory(&conn, 99_999, 1).unwrap().is_none());
    }

    #[test]
    fn search_finds_by_summary_substring_case_insensitive() {
        let conn = open_test_db();
        let now = 1_700_000_000;
        insert_memory(
            &conn,
            &NewCortexMemory {
                workflow_id: "wf".into(),
                kind: MemoryKind::Prior,
                content: serde_json::json!({}),
                summary: Some("Concur uses two-step submit".into()),
                tags: vec![],
                source_ref: None,
                embedding: None,
            },
            now,
        )
        .unwrap();
        insert_memory(
            &conn,
            &NewCortexMemory {
                workflow_id: "wf".into(),
                kind: MemoryKind::Outcome,
                content: serde_json::json!({}),
                summary: Some("clicked Submit on payroll form".into()),
                tags: vec![],
                source_ref: None,
                embedding: None,
            },
            now,
        )
        .unwrap();
        let hits = search_memory(&conn, "wf", "submit", 10).unwrap();
        assert_eq!(hits.len(), 2);
        let hits = search_memory(&conn, "wf", "concur", 10).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn decay_at_half_life_is_one_half() {
        let now = 1_700_000_000;
        let written = now - (90 * 86_400);
        let s = decay_score(written, now);
        assert!(
            (s - 0.5).abs() < 1e-6,
            "expected ~0.5 at half-life, got {s}"
        );
    }

    #[test]
    fn decay_at_one_year_is_about_six_percent() {
        let now = 1_700_000_000;
        let written = now - (365 * 86_400);
        let s = decay_score(written, now);
        assert!(s > 0.05 && s < 0.07, "expected ~0.06 at 365d, got {s}");
    }

    #[test]
    fn decay_at_zero_age_is_one() {
        let now = 1_700_000_000;
        assert!((decay_score(now, now) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn decay_handles_future_dated_memories_as_score_one() {
        let now = 1_700_000_000;
        let s = decay_score(now + 100, now);
        assert!((s - 1.0).abs() < 1e-9);
    }

    #[test]
    fn prune_drops_memories_below_threshold_only() {
        let conn = open_test_db();
        let now = 1_700_000_000;
        // Fresh — keep
        insert_memory(
            &conn,
            &NewCortexMemory {
                workflow_id: "wf".into(),
                kind: MemoryKind::Outcome,
                content: serde_json::json!({}),
                summary: Some("fresh".into()),
                tags: vec![],
                source_ref: None,
                embedding: None,
            },
            now,
        )
        .unwrap();
        // Very old — at 1000d (~2.7 years), score is exp(-ln(2)*1000/90)
        // ≈ 0.00046, well below the 0.01 threshold the comment in
        // `prune_memories` recommends. ~600d is the actual cutoff for
        // 0.01; anything older falls off.
        insert_memory(
            &conn,
            &NewCortexMemory {
                workflow_id: "wf".into(),
                kind: MemoryKind::Outcome,
                content: serde_json::json!({}),
                summary: Some("ancient".into()),
                tags: vec![],
                source_ref: None,
                embedding: None,
            },
            now - (1_000 * 86_400),
        )
        .unwrap();

        let deleted = prune_memories(&conn, 0.01, now).unwrap();
        assert_eq!(deleted, 1);
        let remaining = list_memories(&conn, "wf", None, 10).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].summary.as_deref(), Some("fresh"));
    }

    #[test]
    fn prune_with_zero_threshold_is_a_noop() {
        let conn = open_test_db();
        let now = 1_700_000_000;
        insert_memory(
            &conn,
            &NewCortexMemory {
                workflow_id: "wf".into(),
                kind: MemoryKind::Outcome,
                content: serde_json::json!({}),
                summary: None,
                tags: vec![],
                source_ref: None,
                embedding: None,
            },
            now - (10_000 * 86_400),
        )
        .unwrap();
        assert_eq!(prune_memories(&conn, 0.0, now).unwrap(), 0);
        assert_eq!(list_memories(&conn, "wf", None, 10).unwrap().len(), 1);
    }

    #[test]
    fn migration_is_idempotent() {
        let conn = open_test_db();
        // Calling again should not error.
        migrate_cortex_memories(&conn).unwrap();
        migrate_cortex_memories(&conn).unwrap();
    }

    #[test]
    fn memory_kind_round_trips_through_json() {
        for k in [
            MemoryKind::Outcome,
            MemoryKind::Prior,
            MemoryKind::Failure,
            MemoryKind::Preference,
        ] {
            let s = serde_json::to_string(&k).unwrap();
            let back: MemoryKind = serde_json::from_str(&s).unwrap();
            assert_eq!(k, back);
        }
    }
}
