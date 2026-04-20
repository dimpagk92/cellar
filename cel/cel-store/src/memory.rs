//! Memory subsystem — working memory, observations, and knowledge with FTS5.
//!
//! Inspired by:
//! - Mastra's observational memory (compress run history into durable observations)
//! - Mastra's working memory (per-workflow scratchpad, always in context)
//! - OpenClaw's hybrid search (FTS5 + optional vector, union-based ranking)

use crate::StoreError;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// Observation priority (inspired by Mastra's emoji system).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ObservationPriority {
    /// Critical: user corrections, hard failures, explicit preferences
    High,
    /// Important: learned field mappings, timing patterns, app behaviors
    Medium,
    /// Minor: uncertain patterns, one-time occurrences
    Low,
}

/// A compressed observation derived from workflow run history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub id: i64,
    pub workflow_name: String,
    pub content: String,
    pub priority: ObservationPriority,
    pub source_run_ids: String,
    pub observed_at: String,
    pub referenced_at: Option<String>,
    pub superseded_by: Option<i64>,
    pub created_at: String,
}

/// Per-workflow working memory (scratchpad).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingMemory {
    pub id: i64,
    pub workflow_name: String,
    pub content: String,
    pub updated_at: String,
}

/// A knowledge fact with FTS5 search score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredKnowledge {
    pub id: i64,
    pub content: String,
    pub source: String,
    pub workflow_scope: Option<String>,
    pub score: f64,
    pub created_at: String,
}

/// Configuration for data eviction/TTL policies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvictionConfig {
    /// Days to keep run history (default: 90)
    pub run_retention_days: u32,
    /// Days to keep knowledge entries (default: 365)
    pub knowledge_retention_days: u32,
}

impl Default for EvictionConfig {
    fn default() -> Self {
        Self {
            run_retention_days: 90,
            knowledge_retention_days: 365,
        }
    }
}

/// Result of an eviction run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvictionResult {
    pub superseded_observations: usize,
    pub old_runs: usize,
    pub old_knowledge: usize,
}

impl EvictionResult {
    pub fn total(&self) -> usize {
        self.superseded_observations + self.old_runs + self.old_knowledge
    }
}

/// Initialize the memory tables and FTS5 indexes.
pub fn migrate_memory(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "
        -- Working memory: per-workflow scratchpad (always loaded into context)
        CREATE TABLE IF NOT EXISTS working_memory (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            workflow_name TEXT NOT NULL UNIQUE,
            content TEXT NOT NULL DEFAULT '',
            updated_at TEXT DEFAULT (datetime('now'))
        );

        -- Observations: compressed knowledge from past runs
        CREATE TABLE IF NOT EXISTS observations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            workflow_name TEXT NOT NULL,
            content TEXT NOT NULL,
            priority TEXT NOT NULL DEFAULT 'medium', -- high, medium, low
            source_run_ids TEXT NOT NULL DEFAULT '[]', -- JSON array of run IDs
            observed_at TEXT, -- when the event was observed
            referenced_at TEXT, -- when the event references (temporal anchoring)
            superseded_by INTEGER REFERENCES observations(id),
            created_at TEXT DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_observations_workflow
            ON observations(workflow_name, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_observations_priority
            ON observations(priority);

        -- Knowledge with workflow scoping
        -- Recreate with scope column if needed (additive migration)
        CREATE TABLE IF NOT EXISTS knowledge_scoped (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            content TEXT NOT NULL,
            source TEXT NOT NULL,
            workflow_scope TEXT, -- NULL = global, else workflow-specific
            tags TEXT,
            created_at TEXT DEFAULT (datetime('now'))
        );

        -- FTS5 full-text search index over knowledge
        CREATE VIRTUAL TABLE IF NOT EXISTS knowledge_fts USING fts5(
            content,
            source,
            tags,
            content=knowledge_scoped,
            content_rowid=id
        );

        -- Triggers to keep FTS5 in sync
        CREATE TRIGGER IF NOT EXISTS knowledge_fts_insert AFTER INSERT ON knowledge_scoped BEGIN
            INSERT INTO knowledge_fts(rowid, content, source, tags)
            VALUES (new.id, new.content, new.source, new.tags);
        END;

        CREATE TRIGGER IF NOT EXISTS knowledge_fts_delete AFTER DELETE ON knowledge_scoped BEGIN
            INSERT INTO knowledge_fts(knowledge_fts, rowid, content, source, tags)
            VALUES ('delete', old.id, old.content, old.source, old.tags);
        END;

        CREATE TRIGGER IF NOT EXISTS knowledge_fts_update AFTER UPDATE ON knowledge_scoped BEGIN
            INSERT INTO knowledge_fts(knowledge_fts, rowid, content, source, tags)
            VALUES ('delete', old.id, old.content, old.source, old.tags);
            INSERT INTO knowledge_fts(rowid, content, source, tags)
            VALUES (new.id, new.content, new.source, new.tags);
        END;

        -- Migrate existing agent_knowledge into knowledge_scoped
        INSERT OR IGNORE INTO knowledge_scoped (id, content, source, tags, created_at)
            SELECT id, content, source, tags, created_at FROM agent_knowledge
            WHERE id NOT IN (SELECT id FROM knowledge_scoped);
        ",
    )?;
    Ok(())
}

/// Memory operations on the CEL Store.
impl crate::CelStore {
    // --- Working Memory ---

    /// Get working memory for a workflow. Creates empty entry if none exists.
    pub fn get_working_memory(&self, workflow_name: &str) -> Result<WorkingMemory, StoreError> {
        let result = self.conn.query_row(
            "SELECT id, workflow_name, content, updated_at FROM working_memory WHERE workflow_name = ?1",
            rusqlite::params![workflow_name],
            |row| {
                Ok(WorkingMemory {
                    id: row.get(0)?,
                    workflow_name: row.get(1)?,
                    content: row.get(2)?,
                    updated_at: row.get(3)?,
                })
            },
        );

        match result {
            Ok(wm) => Ok(wm),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                // Create empty working memory
                self.conn.execute(
                    "INSERT INTO working_memory (workflow_name, content) VALUES (?1, '')",
                    rusqlite::params![workflow_name],
                )?;
                let id = self.conn.last_insert_rowid();
                Ok(WorkingMemory {
                    id,
                    workflow_name: workflow_name.to_string(),
                    content: String::new(),
                    updated_at: String::new(),
                })
            }
            Err(e) => Err(StoreError::Database(e)),
        }
    }

    /// Update working memory for a workflow.
    pub fn update_working_memory(
        &self,
        workflow_name: &str,
        content: &str,
    ) -> Result<(), StoreError> {
        let affected = self.conn.execute(
            "UPDATE working_memory SET content = ?1, updated_at = datetime('now') WHERE workflow_name = ?2",
            rusqlite::params![content, workflow_name],
        )?;
        if affected == 0 {
            self.conn.execute(
                "INSERT INTO working_memory (workflow_name, content) VALUES (?1, ?2)",
                rusqlite::params![workflow_name, content],
            )?;
        }
        Ok(())
    }

    // --- Observations ---

    /// Add an observation derived from workflow runs.
    pub fn add_observation(
        &self,
        workflow_name: &str,
        content: &str,
        priority: &ObservationPriority,
        source_run_ids: &[i64],
        observed_at: Option<&str>,
        referenced_at: Option<&str>,
    ) -> Result<i64, StoreError> {
        let priority_str = match priority {
            ObservationPriority::High => "high",
            ObservationPriority::Medium => "medium",
            ObservationPriority::Low => "low",
        };
        let run_ids_json = serde_json::to_string(source_run_ids)?;
        self.conn.execute(
            "INSERT INTO observations (workflow_name, content, priority, source_run_ids, observed_at, referenced_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![workflow_name, content, priority_str, run_ids_json, observed_at, referenced_at],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Get active observations for a workflow (not superseded), ordered by priority then recency.
    pub fn get_observations(
        &self,
        workflow_name: &str,
        limit: u32,
    ) -> Result<Vec<Observation>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, workflow_name, content, priority, source_run_ids, observed_at, referenced_at, superseded_by, created_at
             FROM observations
             WHERE workflow_name = ?1 AND superseded_by IS NULL
             ORDER BY
                 CASE priority WHEN 'high' THEN 0 WHEN 'medium' THEN 1 WHEN 'low' THEN 2 END,
                 created_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![workflow_name, limit], |row: &rusqlite::Row| {
            let priority_str: String = row.get(3)?;
            let priority = match priority_str.as_str() {
                "high" => ObservationPriority::High,
                "low" => ObservationPriority::Low,
                _ => ObservationPriority::Medium,
            };
            Ok(Observation {
                id: row.get(0)?,
                workflow_name: row.get(1)?,
                content: row.get(2)?,
                priority,
                source_run_ids: row.get(4)?,
                observed_at: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                referenced_at: row.get(6)?,
                superseded_by: row.get(7)?,
                created_at: row.get::<_, Option<String>>(8)?.unwrap_or_default(),
            })
        })?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    /// Supersede an observation (mark it as replaced by a newer one).
    pub fn supersede_observation(
        &self,
        old_id: i64,
        new_id: i64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE observations SET superseded_by = ?1 WHERE id = ?2",
            rusqlite::params![new_id, old_id],
        )?;
        Ok(())
    }

    // --- Knowledge with FTS5 ---

    /// Add a scoped knowledge fact.
    pub fn add_scoped_knowledge(
        &self,
        content: &str,
        source: &str,
        workflow_scope: Option<&str>,
        tags: Option<&str>,
    ) -> Result<i64, StoreError> {
        self.conn.execute(
            "INSERT INTO knowledge_scoped (content, source, workflow_scope, tags) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![content, source, workflow_scope, tags],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    // --- TTL / Eviction ---

    /// Delete observations older than `days` for a workflow.
    /// Returns number of deleted rows.
    pub fn evict_old_observations(&self, workflow_name: &str, days: u32) -> Result<usize, StoreError> {
        let affected = self.conn.execute(
            "DELETE FROM observations WHERE workflow_name = ?1 AND created_at < datetime('now', ?2)",
            rusqlite::params![workflow_name, format!("-{} days", days)],
        )?;
        Ok(affected)
    }

    /// Delete low-priority superseded observations across all workflows.
    /// Returns number of deleted rows.
    pub fn evict_superseded_observations(&self) -> Result<usize, StoreError> {
        let affected = self.conn.execute(
            "DELETE FROM observations WHERE superseded_by IS NOT NULL",
            [],
        )?;
        Ok(affected)
    }

    /// Cap observations per workflow, keeping the most recent by priority.
    /// Returns number of deleted rows.
    pub fn cap_observations(&self, workflow_name: &str, max_count: u32) -> Result<usize, StoreError> {
        let affected = self.conn.execute(
            "DELETE FROM observations WHERE workflow_name = ?1 AND id NOT IN (
                SELECT id FROM observations WHERE workflow_name = ?1 AND superseded_by IS NULL
                ORDER BY
                    CASE priority WHEN 'high' THEN 0 WHEN 'medium' THEN 1 WHEN 'low' THEN 2 END,
                    created_at DESC
                LIMIT ?2
            )",
            rusqlite::params![workflow_name, max_count],
        )?;
        Ok(affected)
    }

    /// Delete old run history and step results older than `days`.
    /// Returns number of deleted runs.
    pub fn evict_old_runs(&self, days: u32) -> Result<usize, StoreError> {
        // Delete step results first (FK constraint)
        self.conn.execute(
            "DELETE FROM step_results WHERE run_id IN (
                SELECT id FROM run_history WHERE started_at < datetime('now', ?1)
            )",
            rusqlite::params![format!("-{} days", days)],
        )?;
        // Delete interventions
        self.conn.execute(
            "DELETE FROM interventions WHERE run_id IN (
                SELECT id FROM run_history WHERE started_at < datetime('now', ?1)
            )",
            rusqlite::params![format!("-{} days", days)],
        )?;
        // Delete runs
        let affected = self.conn.execute(
            "DELETE FROM run_history WHERE started_at < datetime('now', ?1)",
            rusqlite::params![format!("-{} days", days)],
        )?;
        Ok(affected)
    }

    /// Delete old knowledge entries older than `days`.
    /// Returns number of deleted rows.
    pub fn evict_old_knowledge(&self, days: u32) -> Result<usize, StoreError> {
        let affected = self.conn.execute(
            "DELETE FROM knowledge_scoped WHERE created_at < datetime('now', ?1)",
            rusqlite::params![format!("-{} days", days)],
        )?;
        Ok(affected)
    }

    /// Run all eviction policies. Returns total rows deleted.
    pub fn run_eviction(&self, config: &EvictionConfig) -> Result<EvictionResult, StoreError> {
        let mut result = EvictionResult::default();

        result.superseded_observations = self.evict_superseded_observations()?;
        result.old_runs = self.evict_old_runs(config.run_retention_days)?;
        result.old_knowledge = self.evict_old_knowledge(config.knowledge_retention_days)?;

        Ok(result)
    }

    /// Search knowledge using FTS5 full-text search.
    /// Returns results ranked by BM25 relevance score.
    pub fn search_knowledge(
        &self,
        query: &str,
        workflow_scope: Option<&str>,
        limit: u32,
    ) -> Result<Vec<ScoredKnowledge>, StoreError> {
        // FTS5 query with BM25 ranking
        let sql = if workflow_scope.is_some() {
            "SELECT ks.id, ks.content, ks.source, ks.workflow_scope, rank, ks.created_at
             FROM knowledge_fts
             JOIN knowledge_scoped ks ON knowledge_fts.rowid = ks.id
             WHERE knowledge_fts MATCH ?1
               AND (ks.workflow_scope IS NULL OR ks.workflow_scope = ?2)
             ORDER BY rank
             LIMIT ?3"
        } else {
            "SELECT ks.id, ks.content, ks.source, ks.workflow_scope, rank, ks.created_at
             FROM knowledge_fts
             JOIN knowledge_scoped ks ON knowledge_fts.rowid = ks.id
             WHERE knowledge_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2"
        };

        let mut stmt = self.conn.prepare(sql)?;
        let map_row = |row: &rusqlite::Row| {
            let bm25_rank: f64 = row.get(4)?;
            Ok(ScoredKnowledge {
                id: row.get(0)?,
                content: row.get(1)?,
                source: row.get(2)?,
                workflow_scope: row.get(3)?,
                score: 1.0 / (1.0 + bm25_rank.abs()),
                created_at: row.get(5)?,
            })
        };

        let rows = if let Some(scope) = workflow_scope {
            stmt.query_map(rusqlite::params![query, scope, limit], map_row)?
        } else {
            stmt.query_map(rusqlite::params![query, limit], map_row)?
        };

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CelStore;

    #[test]
    fn test_working_memory_create_and_get() {
        let store = CelStore::open_memory().unwrap();
        let wm = store.get_working_memory("daily-po").unwrap();
        assert_eq!(wm.workflow_name, "daily-po");
        assert_eq!(wm.content, "");
    }

    #[test]
    fn test_working_memory_update() {
        let store = CelStore::open_memory().unwrap();
        store.update_working_memory("daily-po", "# Field Mappings\n- Vendor X → 10045").unwrap();
        let wm = store.get_working_memory("daily-po").unwrap();
        assert!(wm.content.contains("Vendor X"));
    }

    #[test]
    fn test_working_memory_update_existing() {
        let store = CelStore::open_memory().unwrap();
        store.update_working_memory("wf", "v1").unwrap();
        store.update_working_memory("wf", "v2").unwrap();
        let wm = store.get_working_memory("wf").unwrap();
        assert_eq!(wm.content, "v2");
    }

    #[test]
    fn test_working_memory_isolation() {
        let store = CelStore::open_memory().unwrap();
        store.update_working_memory("wf-a", "data A").unwrap();
        store.update_working_memory("wf-b", "data B").unwrap();
        assert_eq!(store.get_working_memory("wf-a").unwrap().content, "data A");
        assert_eq!(store.get_working_memory("wf-b").unwrap().content, "data B");
    }

    #[test]
    fn test_add_and_get_observations() {
        let store = CelStore::open_memory().unwrap();
        store.add_observation(
            "daily-po", "Vendor X always maps to code 10045",
            &ObservationPriority::High, &[1, 2, 3],
            Some("2024-06-15"), None,
        ).unwrap();
        store.add_observation(
            "daily-po", "SAP dialog takes ~3s after Submit click",
            &ObservationPriority::Medium, &[2, 3],
            None, None,
        ).unwrap();
        store.add_observation(
            "other-wf", "Should not appear",
            &ObservationPriority::Low, &[99],
            None, None,
        ).unwrap();

        let obs = store.get_observations("daily-po", 10).unwrap();
        assert_eq!(obs.len(), 2);
        // High priority first
        assert_eq!(obs[0].priority, ObservationPriority::High);
        assert!(obs[0].content.contains("Vendor X"));
    }

    #[test]
    fn test_observation_supersede() {
        let store = CelStore::open_memory().unwrap();
        let old_id = store.add_observation(
            "wf", "Vendor X → code 10045",
            &ObservationPriority::High, &[1], None, None,
        ).unwrap();
        let new_id = store.add_observation(
            "wf", "Vendor X → code 10046 (updated Jan 2025)",
            &ObservationPriority::High, &[1, 5], None, None,
        ).unwrap();
        store.supersede_observation(old_id, new_id).unwrap();

        let obs = store.get_observations("wf", 10).unwrap();
        assert_eq!(obs.len(), 1);
        assert!(obs[0].content.contains("10046"));
    }

    #[test]
    fn test_scoped_knowledge_fts5_search() {
        let store = CelStore::open_memory().unwrap();
        store.add_scoped_knowledge(
            "Vendor X maps to code 10045 in the SAP system",
            "learned", Some("daily-po"), Some("sap,vendor"),
        ).unwrap();
        store.add_scoped_knowledge(
            "Bloomberg terminal requires F10 to confirm trades",
            "manual", Some("trade-wf"), Some("bloomberg"),
        ).unwrap();
        store.add_scoped_knowledge(
            "Always check for stale prices before submitting",
            "learned", None, Some("trading,risk"),
        ).unwrap();

        // Search for "vendor" — should find the SAP entry
        let results = store.search_knowledge("vendor", None, 10).unwrap();
        assert!(!results.is_empty());
        assert!(results[0].content.contains("Vendor X"));

        // Scoped search — only daily-po and global
        let results = store.search_knowledge("SAP OR stale", Some("daily-po"), 10).unwrap();
        assert!(results.len() >= 1);
    }

    #[test]
    fn test_knowledge_fts5_no_results() {
        let store = CelStore::open_memory().unwrap();
        store.add_scoped_knowledge("Hello world", "test", None, None).unwrap();
        let results = store.search_knowledge("nonexistent_xyz", None, 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_knowledge_score_ranking() {
        let store = CelStore::open_memory().unwrap();
        store.add_scoped_knowledge("Excel cell A1 contains revenue data", "test", None, None).unwrap();
        store.add_scoped_knowledge("Revenue report Excel macro runs weekly", "test", None, None).unwrap();
        store.add_scoped_knowledge("Something unrelated about weather", "test", None, None).unwrap();

        let results = store.search_knowledge("Excel revenue", None, 10).unwrap();
        assert!(results.len() >= 2);
        // All results should have positive scores
        for r in &results {
            assert!(r.score > 0.0);
        }
    }

    #[test]
    fn test_observation_priority_ordering() {
        let store = CelStore::open_memory().unwrap();
        store.add_observation("wf", "Low fact", &ObservationPriority::Low, &[1], None, None).unwrap();
        store.add_observation("wf", "High fact", &ObservationPriority::High, &[1], None, None).unwrap();
        store.add_observation("wf", "Medium fact", &ObservationPriority::Medium, &[1], None, None).unwrap();

        let obs = store.get_observations("wf", 10).unwrap();
        assert_eq!(obs[0].priority, ObservationPriority::High);
        assert_eq!(obs[1].priority, ObservationPriority::Medium);
        assert_eq!(obs[2].priority, ObservationPriority::Low);
    }

    #[test]
    fn test_evict_superseded_observations() {
        let store = CelStore::open_memory().unwrap();
        let old_id = store.add_observation("wf", "old", &ObservationPriority::High, &[1], None, None).unwrap();
        let new_id = store.add_observation("wf", "new", &ObservationPriority::High, &[2], None, None).unwrap();
        store.supersede_observation(old_id, new_id).unwrap();

        let deleted = store.evict_superseded_observations().unwrap();
        assert_eq!(deleted, 1);

        let obs = store.get_observations("wf", 10).unwrap();
        assert_eq!(obs.len(), 1);
        assert!(obs[0].content.contains("new"));
    }

    #[test]
    fn test_cap_observations() {
        let store = CelStore::open_memory().unwrap();
        for i in 0..10 {
            store.add_observation("wf", &format!("obs {}", i), &ObservationPriority::Low, &[1], None, None).unwrap();
        }
        store.add_observation("wf", "important", &ObservationPriority::High, &[1], None, None).unwrap();

        let deleted = store.cap_observations("wf", 3).unwrap();
        assert!(deleted > 0);

        let obs = store.get_observations("wf", 10).unwrap();
        assert!(obs.len() <= 3);
        // High priority should survive
        assert!(obs.iter().any(|o| o.content == "important"));
    }

    #[test]
    fn test_evict_old_runs() {
        let store = CelStore::open_memory().unwrap();
        // Backdate the run to 100 days ago
        store.conn.execute(
            "INSERT INTO run_history (workflow_name, status, steps_total, started_at) VALUES ('wf', 'completed', 2, datetime('now', '-100 days'))",
            [],
        ).unwrap();

        // 90 days retention — should evict the 100-day-old run
        let deleted = store.evict_old_runs(90).unwrap();
        assert_eq!(deleted, 1);
        assert!(store.get_run_history(10).unwrap().is_empty());
    }

    #[test]
    fn test_evict_old_runs_keeps_recent() {
        let store = CelStore::open_memory().unwrap();
        store.start_run("wf", 1).unwrap(); // created now

        // 90 days retention — should keep the just-created run
        let deleted = store.evict_old_runs(90).unwrap();
        assert_eq!(deleted, 0);
        assert_eq!(store.get_run_history(10).unwrap().len(), 1);
    }

    #[test]
    fn test_run_eviction_with_config() {
        let store = CelStore::open_memory().unwrap();
        // Backdate data so it's evictable
        store.conn.execute(
            "INSERT INTO observations (workflow_name, content, priority, source_run_ids, created_at) VALUES ('wf', 'old obs', 'low', '[]', datetime('now', '-200 days'))",
            [],
        ).unwrap();
        let old_id = store.conn.last_insert_rowid();
        let new_id = store.add_observation("wf", "new obs", &ObservationPriority::High, &[1], None, None).unwrap();
        store.supersede_observation(old_id, new_id).unwrap();

        store.conn.execute(
            "INSERT INTO run_history (workflow_name, status, steps_total, started_at) VALUES ('wf', 'completed', 1, datetime('now', '-200 days'))",
            [],
        ).unwrap();

        let config = EvictionConfig {
            run_retention_days: 90,
            knowledge_retention_days: 90,
        };
        let result = store.run_eviction(&config).unwrap();
        assert!(result.total() > 0);
    }

    #[test]
    fn test_eviction_config_defaults() {
        let config = EvictionConfig::default();
        assert_eq!(config.run_retention_days, 90);
        assert_eq!(config.knowledge_retention_days, 365);
    }
}
