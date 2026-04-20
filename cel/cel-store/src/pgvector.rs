//! PostgreSQL + pgvector schema and query definitions for scalable deployments.
//!
//! **Status: schema-only.** This module provides migration SQL and hybrid
//! search queries for a future PostgreSQL backend. It does NOT include a
//! connection pool or runtime implementation yet — use [`CelStore`] (SQLite)
//! for all current workloads.
//!
//! ## When to use (future)
//! - Multi-tenant / enterprise deployments
//! - When you need horizontal scaling
//! - When you want vector embeddings for semantic knowledge search
//! - When you need concurrent access from multiple agents
//!
//! ## Architecture
//! SQLite (default, implemented) → good for single-user, local, zero-ops
//! PostgreSQL + pgvector (planned) → good for production, multi-tenant, team deployments
//!
//! ## Setup (when implemented)
//! 1. Install PostgreSQL with pgvector extension
//! 2. Set `DATABASE_URL` environment variable
//! 3. Enable the `pgvector` feature: `cargo build -p cel-store --features pgvector`

use serde::{Deserialize, Serialize};

/// Configuration for the PostgreSQL vector store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PgVectorConfig {
    /// PostgreSQL connection string (e.g., postgres://user:pass@localhost/cellar)
    pub connection_url: String,
    /// Vector embedding dimensions (default: 1536 for text-embedding-3-small)
    pub dimensions: u32,
    /// Schema namespace for multi-tenant isolation (default: "public")
    pub schema: String,
    /// Index type: "ivfflat" (faster build, good for <1M rows) or "hnsw" (better recall)
    pub index_type: PgIndexType,
}

/// PostgreSQL vector index type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PgIndexType {
    /// IVFFlat — faster to build, good for datasets under 1M rows
    IvfFlat,
    /// HNSW — better recall, recommended for production
    Hnsw,
}

impl Default for PgVectorConfig {
    fn default() -> Self {
        Self {
            connection_url: String::new(),
            dimensions: 1536,
            schema: "public".to_string(),
            index_type: PgIndexType::Hnsw,
        }
    }
}

/// SQL statements for PostgreSQL + pgvector schema initialization.
/// These are the production equivalents of the SQLite tables.
pub fn migration_sql(config: &PgVectorConfig) -> String {
    let schema = &config.schema;
    let dims = config.dimensions;
    let index_sql = match config.index_type {
        PgIndexType::IvfFlat => format!(
            "CREATE INDEX IF NOT EXISTS idx_knowledge_embedding ON {schema}.knowledge_vectors USING ivfflat (embedding vector_cosine_ops) WITH (lists = 100);"
        ),
        PgIndexType::Hnsw => format!(
            "CREATE INDEX IF NOT EXISTS idx_knowledge_embedding ON {schema}.knowledge_vectors USING hnsw (embedding vector_cosine_ops) WITH (m = 16, ef_construction = 64);"
        ),
    };

    format!(
        r#"
        -- Enable pgvector extension
        CREATE EXTENSION IF NOT EXISTS vector;

        -- Knowledge with vector embeddings
        CREATE TABLE IF NOT EXISTS {schema}.knowledge_vectors (
            id BIGSERIAL PRIMARY KEY,
            content TEXT NOT NULL,
            source TEXT NOT NULL,
            workflow_scope TEXT,
            tags TEXT,
            embedding vector({dims}),
            created_at TIMESTAMPTZ DEFAULT NOW()
        );

        -- Vector similarity index
        {index_sql}

        -- Full-text search index (PostgreSQL native)
        CREATE INDEX IF NOT EXISTS idx_knowledge_fts
            ON {schema}.knowledge_vectors
            USING gin(to_tsvector('english', content));

        -- Run history (same schema as SQLite)
        CREATE TABLE IF NOT EXISTS {schema}.run_history (
            id BIGSERIAL PRIMARY KEY,
            workflow_name TEXT NOT NULL,
            started_at TIMESTAMPTZ DEFAULT NOW(),
            finished_at TIMESTAMPTZ,
            status TEXT NOT NULL DEFAULT 'running',
            steps_completed INTEGER DEFAULT 0,
            steps_total INTEGER DEFAULT 0,
            interventions INTEGER DEFAULT 0
        );

        -- Step results
        CREATE TABLE IF NOT EXISTS {schema}.step_results (
            id BIGSERIAL PRIMARY KEY,
            run_id BIGINT NOT NULL REFERENCES {schema}.run_history(id),
            step_index INTEGER NOT NULL,
            step_id TEXT NOT NULL,
            action JSONB NOT NULL,
            success BOOLEAN NOT NULL DEFAULT TRUE,
            confidence DOUBLE PRECISION NOT NULL DEFAULT 0.0,
            context_snapshot JSONB,
            error TEXT,
            executed_at TIMESTAMPTZ DEFAULT NOW()
        );

        -- Observations
        CREATE TABLE IF NOT EXISTS {schema}.observations (
            id BIGSERIAL PRIMARY KEY,
            workflow_name TEXT NOT NULL,
            content TEXT NOT NULL,
            priority TEXT NOT NULL DEFAULT 'medium',
            source_run_ids JSONB NOT NULL DEFAULT '[]',
            observed_at TIMESTAMPTZ,
            referenced_at TIMESTAMPTZ,
            superseded_by BIGINT REFERENCES {schema}.observations(id),
            created_at TIMESTAMPTZ DEFAULT NOW()
        );

        CREATE INDEX IF NOT EXISTS idx_observations_workflow
            ON {schema}.observations(workflow_name, created_at DESC);

        -- Working memory
        CREATE TABLE IF NOT EXISTS {schema}.working_memory (
            id BIGSERIAL PRIMARY KEY,
            workflow_name TEXT NOT NULL UNIQUE,
            content TEXT NOT NULL DEFAULT '',
            updated_at TIMESTAMPTZ DEFAULT NOW()
        );

        -- Composite indexes for common queries
        CREATE INDEX IF NOT EXISTS idx_run_history_workflow
            ON {schema}.run_history(workflow_name, started_at DESC);
        CREATE INDEX IF NOT EXISTS idx_step_results_run
            ON {schema}.step_results(run_id, step_index);
        "#
    )
}

/// SQL for hybrid search: combines vector similarity (70%) with full-text BM25 (30%).
/// This matches OpenClaw's proven hybrid search pattern.
pub fn hybrid_search_sql(schema: &str) -> String {
    format!(
        r#"
        WITH vector_results AS (
            SELECT id, content, source, workflow_scope, tags, created_at,
                   1 - (embedding <=> $1::vector) AS vector_score
            FROM {schema}.knowledge_vectors
            WHERE ($2::text IS NULL OR workflow_scope IS NULL OR workflow_scope = $2)
            ORDER BY embedding <=> $1::vector
            LIMIT $3 * 4
        ),
        text_results AS (
            SELECT id, content, source, workflow_scope, tags, created_at,
                   ts_rank(to_tsvector('english', content), plainto_tsquery('english', $4)) AS text_score
            FROM {schema}.knowledge_vectors
            WHERE to_tsvector('english', content) @@ plainto_tsquery('english', $4)
              AND ($2::text IS NULL OR workflow_scope IS NULL OR workflow_scope = $2)
            LIMIT $3 * 4
        ),
        combined AS (
            SELECT COALESCE(v.id, t.id) AS id,
                   COALESCE(v.content, t.content) AS content,
                   COALESCE(v.source, t.source) AS source,
                   COALESCE(v.workflow_scope, t.workflow_scope) AS workflow_scope,
                   COALESCE(v.created_at, t.created_at) AS created_at,
                   0.7 * COALESCE(v.vector_score, 0) + 0.3 * COALESCE(t.text_score, 0) AS score
            FROM vector_results v
            FULL OUTER JOIN text_results t ON v.id = t.id
        )
        SELECT id, content, source, workflow_scope, score, created_at
        FROM combined
        ORDER BY score DESC
        LIMIT $3
        "#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = PgVectorConfig::default();
        assert_eq!(config.dimensions, 1536);
        assert_eq!(config.schema, "public");
        assert!(matches!(config.index_type, PgIndexType::Hnsw));
    }

    #[test]
    fn test_migration_sql_hnsw() {
        let config = PgVectorConfig::default();
        let sql = migration_sql(&config);
        assert!(sql.contains("CREATE EXTENSION IF NOT EXISTS vector"));
        assert!(sql.contains("vector(1536)"));
        assert!(sql.contains("USING hnsw"));
        assert!(sql.contains("knowledge_vectors"));
        assert!(sql.contains("run_history"));
        assert!(sql.contains("step_results"));
        assert!(sql.contains("observations"));
        assert!(sql.contains("working_memory"));
    }

    #[test]
    fn test_migration_sql_ivfflat() {
        let config = PgVectorConfig {
            index_type: PgIndexType::IvfFlat,
            ..Default::default()
        };
        let sql = migration_sql(&config);
        assert!(sql.contains("USING ivfflat"));
    }

    #[test]
    fn test_migration_sql_custom_schema() {
        let config = PgVectorConfig {
            schema: "cellar_prod".to_string(),
            dimensions: 3072,
            ..Default::default()
        };
        let sql = migration_sql(&config);
        assert!(sql.contains("cellar_prod.knowledge_vectors"));
        assert!(sql.contains("vector(3072)"));
    }

    #[test]
    fn test_hybrid_search_sql() {
        let sql = hybrid_search_sql("public");
        assert!(sql.contains("vector_score"));
        assert!(sql.contains("text_score"));
        assert!(sql.contains("0.7"));
        assert!(sql.contains("0.3"));
        assert!(sql.contains("FULL OUTER JOIN"));
    }

    #[test]
    fn test_config_serialization() {
        let config = PgVectorConfig {
            connection_url: "postgres://localhost/cellar".into(),
            dimensions: 768,
            schema: "test".into(),
            index_type: PgIndexType::IvfFlat,
        };
        let json = serde_json::to_string(&config).unwrap();
        let back: PgVectorConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.dimensions, 768);
        assert_eq!(back.connection_url, "postgres://localhost/cellar");
    }
}
