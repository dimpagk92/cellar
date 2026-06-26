//! Implement a custom `MemoryProvider` by wrapping another provider.
//!
//! Run with: `cargo run -p cel-memory --example custom_provider`

use async_trait::async_trait;
use cel_memory::{
    AgingReport, BasicMemoryProvider, ChunkKind, ChunkSource, EvictionReason, ExportBundle,
    ExportFilter, MemoryChunk, MemoryPredicate, MemoryProvider, MemoryQuery, MemorySession,
    MemoryStats, NewMemoryChunk, NewMemorySession, PurgeReport, ReEmbedReport, SessionFilter,
    SessionOutcome,
};
use chrono::NaiveDate;
use serde_json::json;

#[derive(Clone)]
struct PrefixingProvider {
    inner: BasicMemoryProvider,
    prefix: String,
}

impl PrefixingProvider {
    fn new(prefix: impl Into<String>) -> Self {
        Self {
            inner: BasicMemoryProvider::new(),
            prefix: prefix.into(),
        }
    }
}

#[async_trait]
impl MemoryProvider for PrefixingProvider {
    async fn retrieve(&self, query: MemoryQuery) -> cel_memory::Result<Vec<MemoryChunk>> {
        self.inner.retrieve(query).await
    }

    async fn get(&self, chunk_id: &str) -> cel_memory::Result<Option<MemoryChunk>> {
        self.inner.get(chunk_id).await
    }

    async fn get_session(&self, session_id: &str) -> cel_memory::Result<Option<MemorySession>> {
        self.inner.get_session(session_id).await
    }

    async fn list_sessions(&self, filter: SessionFilter) -> cel_memory::Result<Vec<MemorySession>> {
        self.inner.list_sessions(filter).await
    }

    async fn write(&self, mut chunk: NewMemoryChunk) -> cel_memory::Result<MemoryChunk> {
        chunk.content = format!("{}{}", self.prefix, chunk.content);
        self.inner.write(chunk).await
    }

    async fn write_batch(
        &self,
        chunks: Vec<NewMemoryChunk>,
    ) -> cel_memory::Result<Vec<MemoryChunk>> {
        let mut out = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            out.push(self.write(chunk).await?);
        }
        Ok(out)
    }

    async fn open_session(&self, init: NewMemorySession) -> cel_memory::Result<MemorySession> {
        self.inner.open_session(init).await
    }

    async fn close_session(
        &self,
        session_id: &str,
        outcome: SessionOutcome,
    ) -> cel_memory::Result<()> {
        self.inner.close_session(session_id, outcome).await
    }

    async fn rename_session(&self, session_id: &str, title: &str) -> cel_memory::Result<()> {
        self.inner.rename_session(session_id, title).await
    }

    async fn pin(&self, chunk_id: &str, pinned: bool) -> cel_memory::Result<()> {
        self.inner.pin(chunk_id, pinned).await
    }

    async fn update_importance(&self, chunk_id: &str, importance: f32) -> cel_memory::Result<()> {
        self.inner.update_importance(chunk_id, importance).await
    }

    async fn supersede(&self, old_id: &str, new_id: &str) -> cel_memory::Result<()> {
        self.inner.supersede(old_id, new_id).await
    }

    async fn record_access(
        &self,
        chunk_id: &str,
        retrieved_by: &str,
        used: bool,
    ) -> cel_memory::Result<()> {
        self.inner.record_access(chunk_id, retrieved_by, used).await
    }

    async fn delete(&self, chunk_id: &str, reason: EvictionReason) -> cel_memory::Result<()> {
        self.inner.delete(chunk_id, reason).await
    }

    async fn delete_matching(
        &self,
        predicate: MemoryPredicate,
        reason: EvictionReason,
    ) -> cel_memory::Result<usize> {
        self.inner.delete_matching(predicate, reason).await
    }

    async fn purge_all(&self) -> cel_memory::Result<PurgeReport> {
        self.inner.purge_all().await
    }

    async fn summarize_session(&self, session_id: &str) -> cel_memory::Result<MemoryChunk> {
        self.inner.summarize_session(session_id).await
    }

    async fn rollup_day(&self, date: NaiveDate) -> cel_memory::Result<Vec<MemoryChunk>> {
        self.inner.rollup_day(date).await
    }

    async fn rollup_rule_week(
        &self,
        rule_id: &str,
        week_start: NaiveDate,
    ) -> cel_memory::Result<MemoryChunk> {
        self.inner.rollup_rule_week(rule_id, week_start).await
    }

    async fn run_aging_sweep(&self) -> cel_memory::Result<AgingReport> {
        self.inner.run_aging_sweep().await
    }

    async fn re_embed_all(&self, target_model: &str) -> cel_memory::Result<ReEmbedReport> {
        self.inner.re_embed_all(target_model).await
    }

    async fn export(&self, filter: ExportFilter) -> cel_memory::Result<ExportBundle> {
        self.inner.export(filter).await
    }

    async fn stats(&self) -> cel_memory::Result<MemoryStats> {
        self.inner.stats().await
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let memory = PrefixingProvider::new("[tenant:acme] ");
    let chunk = memory
        .write(NewMemoryChunk {
            kind: ChunkKind::Context,
            source: ChunkSource::Embedded,
            session_id: None,
            project_root: None,
            caller_id: "example-agent".into(),
            content: "deployment window is 14:00 UTC".into(),
            metadata: json!(null),
            importance: Some(0.8),
            shareable: false,
            pinned: false,
        })
        .await?;

    println!("{}", chunk.content);
    Ok(())
}
