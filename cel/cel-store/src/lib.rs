//! CEL Store
//!
//! Embedded SQLite database for persisting agent memory, context maps,
//! run history, confidence data, and learned knowledge.
//!
//! ## Architecture
//! - **schema.rs** — Core tables, migrations, run/step tracking
//! - **memory.rs** — Working memory, observations, knowledge with FTS5
//! - **filesystem.rs** — Screenshot storage, JSONL run transcripts

pub mod cortex_memory;
mod filesystem;
mod memory;
pub mod pgvector;
mod schema;

pub use cortex_memory::{
    decay_score, insert_memory, list_memories, migrate_cortex_memories,
    migrate_cortex_memories_fts, now_unix_secs, prune_memories, safe_fts5_query_from_keywords,
    search_memory, touch_memory, CortexMemory, CortexMemoryStore, KnowledgeStore, MemoryKind,
    NewCortexMemory, RecentEventStore, DEFAULT_HALF_LIFE_DAYS,
};
pub use filesystem::FsStore;
pub use memory::ScoredKnowledge;
pub use memory::{EvictionConfig, EvictionResult, Observation, ObservationPriority, WorkingMemory};
pub use schema::{CelStore, KnowledgeFact, RunRecord, StepRecord};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Store not initialized")]
    NotInitialized,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Not found: {0}")]
    NotFound(String),
}
