use crate::StoreError;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// The main CEL Store handle.
pub struct CelStore {
    pub(crate) conn: Connection,
}

/// A fact stored in the agent knowledge layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeFact {
    pub id: i64,
    pub content: String,
    pub source: String,
    pub created_at: String,
}

/// A run history entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: i64,
    pub workflow_name: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub steps_completed: u32,
    pub steps_total: u32,
    pub interventions: u32,
}

/// A single step result logged during a workflow run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRecord {
    pub id: i64,
    pub run_id: i64,
    pub step_index: u32,
    pub step_id: String,
    pub action: String,
    pub success: bool,
    pub confidence: f64,
    pub context_snapshot: Option<String>,
    pub error: Option<String>,
    pub executed_at: String,
}

impl CelStore {
    /// Open or create a CEL Store database at the given path.
    pub fn open(path: &str) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             PRAGMA synchronous = NORMAL;
             PRAGMA cache_size = -64000;
             PRAGMA temp_store = MEMORY;",
        )?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Open an in-memory database (for testing).
    pub fn open_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Current schema version.
    const SCHEMA_VERSION: u32 = 1;

    /// Run database migrations with version tracking.
    fn migrate(&self) -> Result<(), StoreError> {
        // Create migration tracking table
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT DEFAULT (datetime('now'))
            );",
        )?;

        let current: u32 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if current >= Self::SCHEMA_VERSION {
            return Ok(());
        }

        // Version 1: initial schema
        if current < 1 {
            self.migrate_v1()?;
            self.conn.execute(
                "INSERT INTO schema_migrations (version) VALUES (?1)",
                rusqlite::params![1],
            )?;
        }

        // Future migrations go here:
        // if current < 2 { self.migrate_v2()?; ... }

        Ok(())
    }

    /// Version 1: initial schema.
    fn migrate_v1(&self) -> Result<(), StoreError> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS context_maps (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workflow_name TEXT NOT NULL,
                app_name TEXT NOT NULL,
                element_map TEXT NOT NULL, -- JSON
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS run_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workflow_name TEXT NOT NULL,
                started_at TEXT DEFAULT (datetime('now')),
                finished_at TEXT,
                status TEXT NOT NULL DEFAULT 'running',
                steps_completed INTEGER DEFAULT 0,
                steps_total INTEGER DEFAULT 0,
                interventions INTEGER DEFAULT 0,
                log TEXT -- JSON array of step logs
            );

            CREATE TABLE IF NOT EXISTS confidence_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                element_id TEXT NOT NULL,
                app_name TEXT NOT NULL,
                confidence REAL NOT NULL,
                source TEXT NOT NULL,
                recorded_at TEXT DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS agent_knowledge (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                content TEXT NOT NULL,
                source TEXT NOT NULL,
                tags TEXT, -- comma-separated
                created_at TEXT DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS interventions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id INTEGER REFERENCES run_history(id),
                step_index INTEGER NOT NULL,
                agent_context TEXT NOT NULL, -- JSON: what the agent saw
                user_action TEXT NOT NULL, -- JSON: what the user did
                correct_action TEXT, -- JSON: derived correct action
                recorded_at TEXT DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS workflow_state (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workflow_name TEXT NOT NULL UNIQUE,
                current_step INTEGER DEFAULT 0,
                state TEXT NOT NULL DEFAULT 'idle', -- idle, running, paused, queued
                queue_priority INTEGER DEFAULT 0,
                context TEXT, -- JSON: serialized execution context
                updated_at TEXT DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS credential_refs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                store_type TEXT NOT NULL, -- 'env', 'keychain', 'vault'
                reference TEXT NOT NULL, -- env var name or keychain entry
                created_at TEXT DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS step_results (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id INTEGER NOT NULL REFERENCES run_history(id),
                step_index INTEGER NOT NULL,
                step_id TEXT NOT NULL,
                action TEXT NOT NULL, -- JSON: the action taken
                success INTEGER NOT NULL DEFAULT 1,
                confidence REAL NOT NULL DEFAULT 0.0,
                context_snapshot TEXT, -- JSON: screen context at time of step
                error TEXT,
                executed_at TEXT DEFAULT (datetime('now'))
            );
            ",
        )?;

        // Memory subsystem tables (FTS5, working memory, observations)
        crate::memory::migrate_memory(&self.conn)?;

        Ok(())
    }

    /// Store a knowledge fact (writes to knowledge_scoped for FTS5 indexing).
    pub fn add_knowledge(&self, content: &str, source: &str) -> Result<i64, StoreError> {
        self.conn.execute(
            "INSERT INTO knowledge_scoped (content, source) VALUES (?1, ?2)",
            rusqlite::params![content, source],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Query knowledge facts by keyword search.
    pub fn query_knowledge(&self, query: &str) -> Result<Vec<KnowledgeFact>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, source, created_at FROM knowledge_scoped WHERE content LIKE ?1",
        )?;
        let pattern = format!("%{}%", query);
        let rows = stmt.query_map(rusqlite::params![pattern], |row| {
            Ok(KnowledgeFact {
                id: row.get(0)?,
                content: row.get(1)?,
                source: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        Ok(facts)
    }

    /// Record a new workflow run.
    pub fn start_run(&self, workflow_name: &str, steps_total: u32) -> Result<i64, StoreError> {
        self.conn.execute(
            "INSERT INTO run_history (workflow_name, status, steps_total) VALUES (?1, 'running', ?2)",
            rusqlite::params![workflow_name, steps_total],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Complete a workflow run.
    pub fn finish_run(&self, run_id: i64, status: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE run_history SET status = ?1, finished_at = datetime('now') WHERE id = ?2",
            rusqlite::params![status, run_id],
        )?;
        Ok(())
    }

    /// Log a step result during a workflow run.
    #[allow(clippy::too_many_arguments)]
    pub fn log_step(
        &self,
        run_id: i64,
        step_index: u32,
        step_id: &str,
        action: &str,
        success: bool,
        confidence: f64,
        context_snapshot: Option<&str>,
        error: Option<&str>,
    ) -> Result<i64, StoreError> {
        self.conn.execute_batch("BEGIN")?;
        let result = (|| -> Result<i64, StoreError> {
            self.conn.execute(
                "INSERT INTO step_results (run_id, step_index, step_id, action, success, confidence, context_snapshot, error) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![run_id, step_index, step_id, action, success as i32, confidence, context_snapshot, error],
            )?;
            let step_row_id = self.conn.last_insert_rowid();
            self.conn.execute(
                "UPDATE run_history SET steps_completed = (SELECT COUNT(*) FROM step_results WHERE run_id = ?1 AND success = 1) WHERE id = ?1",
                rusqlite::params![run_id],
            )?;
            Ok(step_row_id)
        })();
        match &result {
            Ok(_) => self.conn.execute_batch("COMMIT")?,
            Err(_) => {
                let _ = self.conn.execute_batch("ROLLBACK");
            }
        }
        result
    }

    /// Get step results for a workflow run.
    pub fn get_step_results(&self, run_id: i64) -> Result<Vec<StepRecord>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, step_index, step_id, action, success, confidence, context_snapshot, error, executed_at FROM step_results WHERE run_id = ?1 ORDER BY step_index",
        )?;
        let rows = stmt.query_map(rusqlite::params![run_id], |row| {
            Ok(StepRecord {
                id: row.get(0)?,
                run_id: row.get(1)?,
                step_index: row.get(2)?,
                step_id: row.get(3)?,
                action: row.get(4)?,
                success: row.get::<_, i32>(5)? != 0,
                confidence: row.get(6)?,
                context_snapshot: row.get(7)?,
                error: row.get(8)?,
                executed_at: row.get(9)?,
            })
        })?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    /// Get run history, most recent first.
    pub fn get_run_history(&self, limit: u32) -> Result<Vec<RunRecord>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, workflow_name, started_at, finished_at, status, steps_completed, steps_total, interventions FROM run_history ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit], |row| {
            Ok(RunRecord {
                id: row.get(0)?,
                workflow_name: row.get(1)?,
                started_at: row.get(2)?,
                finished_at: row.get(3)?,
                status: row.get(4)?,
                steps_completed: row.get(5)?,
                steps_total: row.get(6)?,
                interventions: row.get(7)?,
            })
        })?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    /// Record an intervention (user correction during a run).
    pub fn record_intervention(
        &self,
        run_id: i64,
        step_index: u32,
        agent_context: &str,
        user_action: &str,
        correct_action: Option<&str>,
    ) -> Result<i64, StoreError> {
        self.conn.execute(
            "INSERT INTO interventions (run_id, step_index, agent_context, user_action, correct_action) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![run_id, step_index, agent_context, user_action, correct_action],
        )?;
        // Increment interventions counter on the run
        self.conn.execute(
            "UPDATE run_history SET interventions = interventions + 1 WHERE id = ?1",
            rusqlite::params![run_id],
        )?;
        Ok(self.conn.last_insert_rowid())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_open_and_migrate() {
        let store = CelStore::open_memory().expect("Failed to open in-memory store");
        // Verify tables exist by inserting
        store
            .add_knowledge("test fact", "test")
            .expect("Failed to add knowledge");
    }

    #[test]
    fn test_knowledge_roundtrip() {
        let store = CelStore::open_memory().unwrap();
        store
            .add_knowledge("Vendor X maps to code 10045", "manual")
            .unwrap();
        store
            .add_knowledge("Vendor Y requires approval over 50000", "learned")
            .unwrap();

        let results = store.query_knowledge("Vendor").unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].content.contains("Vendor"));
    }

    #[test]
    fn test_run_tracking() {
        let store = CelStore::open_memory().unwrap();
        let run_id = store.start_run("daily-po", 5).unwrap();
        assert!(run_id > 0);
        store.finish_run(run_id, "completed").unwrap();
    }

    #[test]
    fn test_log_step_and_retrieve() {
        let store = CelStore::open_memory().unwrap();
        let run_id = store.start_run("test-wf", 3).unwrap();

        store
            .log_step(
                run_id,
                0,
                "step-1",
                r#"{"type":"click"}"#,
                true,
                0.95,
                Some(r#"{"app":"Excel"}"#),
                None,
            )
            .unwrap();
        store
            .log_step(
                run_id,
                1,
                "step-2",
                r#"{"type":"type"}"#,
                true,
                0.88,
                None,
                None,
            )
            .unwrap();
        store
            .log_step(
                run_id,
                2,
                "step-3",
                r#"{"type":"key"}"#,
                false,
                0.45,
                None,
                Some("Element not found"),
            )
            .unwrap();

        let steps = store.get_step_results(run_id).unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].step_id, "step-1");
        assert!(steps[0].success);
        assert_eq!(steps[0].confidence, 0.95);
        assert!(steps[0].context_snapshot.is_some());
        assert!(!steps[2].success);
        assert!(steps[2].error.as_deref() == Some("Element not found"));
    }

    #[test]
    fn test_steps_completed_auto_updates() {
        let store = CelStore::open_memory().unwrap();
        let run_id = store.start_run("test-wf", 3).unwrap();

        store
            .log_step(run_id, 0, "s1", "{}", true, 0.9, None, None)
            .unwrap();
        store
            .log_step(run_id, 1, "s2", "{}", true, 0.9, None, None)
            .unwrap();
        store
            .log_step(run_id, 2, "s3", "{}", false, 0.4, None, Some("fail"))
            .unwrap();

        let history = store.get_run_history(10).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].steps_completed, 2); // only 2 succeeded
    }

    #[test]
    fn test_get_run_history() {
        let store = CelStore::open_memory().unwrap();
        store.start_run("wf-1", 3).unwrap();
        store.start_run("wf-2", 5).unwrap();
        store.start_run("wf-3", 1).unwrap();

        let history = store.get_run_history(2).unwrap();
        assert_eq!(history.len(), 2);
        // Most recent first
        assert_eq!(history[0].workflow_name, "wf-3");
        assert_eq!(history[1].workflow_name, "wf-2");
    }

    #[test]
    fn test_record_intervention() {
        let store = CelStore::open_memory().unwrap();
        let run_id = store.start_run("test-wf", 3).unwrap();

        let id = store
            .record_intervention(
                run_id,
                1,
                r#"{"elements":[]}"#,
                r#"{"type":"click","x":100,"y":200}"#,
                Some(r#"{"type":"click","target":"submit-btn"}"#),
            )
            .unwrap();
        assert!(id > 0);

        let history = store.get_run_history(10).unwrap();
        assert_eq!(history[0].interventions, 1);
    }
}
