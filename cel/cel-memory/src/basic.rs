//! `BasicMemoryProvider` — the v1 backing implementation.
//!
//! In-process, in-memory, deliberately simple. Implements retrieval, writes,
//! session lifecycle, simple deletes, export, and stats; returns
//! `Err(NotImplemented)` for summarization and re-embed; no-ops for
//! `update_importance` and `supersede`.
//!
//! Lexical retrieval only — substring + recency. A full storage backend
//! (e.g. the `cel-memory-sqlite` crate) replaces this with hybrid
//! (vector + FTS + recency) retrieval.
//!
//! The persistence layer here is `Arc<Mutex<State>>`. It is useful for tests,
//! examples, and lightweight agents that do not need durability across process
//! restarts.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{NaiveDate, Utc};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::chunk::{ChunkKind, MemoryChunk, MemoryTier, NewMemoryChunk};
use crate::error::{MemoryError, Result};
use crate::ops::{
    AccessEntry, AgingReport, EvictionEntry, EvictionReason, ExportBundle, ExportFilter,
    MemoryStats, PurgeReport, ReEmbedReport,
};
use crate::provider::MemoryProvider;
use crate::query::{MemoryPredicate, MemoryQuery};
use crate::session::{MemorySession, NewMemorySession, SessionFilter, SessionOutcome};

/// The v1 backing implementation. Cheap to construct; `Clone` is shallow
/// (shares the underlying state). Safe to share across tasks.
#[derive(Clone, Default)]
pub struct BasicMemoryProvider {
    state: Arc<Mutex<State>>,
    /// Optional pre-write hook (redaction, governance). When unset, every
    /// write proceeds verbatim.
    write_hook: Option<Arc<dyn crate::MemoryWriteHook>>,
}

impl std::fmt::Debug for BasicMemoryProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BasicMemoryProvider")
            .field("state", &self.state)
            .field("write_hook", &self.write_hook.as_ref().map(|_| "<hook>"))
            .finish()
    }
}

#[derive(Debug, Default)]
struct State {
    chunks: HashMap<String, MemoryChunk>,
    sessions: HashMap<String, MemorySession>,
    evictions: Vec<EvictionEntry>,
    accesses: Vec<AccessEntry>,
}

impl BasicMemoryProvider {
    /// Construct an empty provider. Equivalent to `Default::default()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach a [`MemoryWriteHook`](crate::MemoryWriteHook) consulted
    /// before every write. Use this for policy-driven memory redaction or
    /// application-specific write filtering.
    pub fn with_write_hook(mut self, hook: Arc<dyn crate::MemoryWriteHook>) -> Self {
        self.write_hook = Some(hook);
        self
    }

    fn next_id() -> String {
        Uuid::now_v7().to_string()
    }
}

#[async_trait]
impl MemoryProvider for BasicMemoryProvider {
    // ─────────────── Reads ───────────────

    async fn retrieve(&self, query: MemoryQuery) -> Result<Vec<MemoryChunk>> {
        if query.text.trim().is_empty() {
            return Err(MemoryError::InvalidArgument(
                "query.text must not be empty".into(),
            ));
        }
        let needle = query.text.to_lowercase();
        let state = self.state.lock().await;

        let mut hits: Vec<&MemoryChunk> = state
            .chunks
            .values()
            .filter(|c| {
                // Kind filter.
                if let Some(kinds) = &query.kinds {
                    if !kinds.contains(&c.kind) {
                        return false;
                    }
                }
                // include_rollups=false suppresses rollup chunks.
                if !query.include_rollups && c.kind == ChunkKind::Rollup {
                    return false;
                }
                // Time bounds.
                if let Some(since) = query.since {
                    if c.created_at < since {
                        return false;
                    }
                }
                if let Some(until) = query.until {
                    if c.created_at > until {
                        return false;
                    }
                }
                // Session restriction.
                if let Some(sid) = &query.session_id {
                    if c.session_id.as_deref() != Some(sid.as_str()) {
                        return false;
                    }
                }
                // Project root prefix.
                if let Some(prefix) = &query.project_root_prefix {
                    match &c.project_root {
                        Some(root) if root.starts_with(prefix.as_str()) => {}
                        _ => return false,
                    }
                }
                // Min importance.
                if let Some(min) = query.min_importance {
                    if c.importance < min {
                        return false;
                    }
                }
                // CallerScope enforcement. `Own` restricts to the caller's
                // own chunks. `OwnPlusShared` permits the caller's own
                // chunks *and* any chunk tagged `shareable=true` (the
                // Phase 4 multi-agent surface).
                // `Global` permits everything.
                match query.caller_scope {
                    crate::query::CallerScope::Own => {
                        if c.caller_id != query.caller_id {
                            return false;
                        }
                    }
                    crate::query::CallerScope::OwnPlusShared => {
                        if c.caller_id != query.caller_id && !c.shareable {
                            return false;
                        }
                    }
                    crate::query::CallerScope::Global => {}
                }
                // Lexical: substring of content (case-insensitive).
                c.content.to_lowercase().contains(&needle)
            })
            .collect();

        // Order by recency (newest first). The v1 stub does not perform
        // hybrid scoring — `RetrievalProfile` is accepted but ignored.
        hits.sort_by_key(|c| std::cmp::Reverse(c.created_at));
        hits.truncate(query.k);
        Ok(hits.into_iter().cloned().collect())
    }

    async fn get(&self, chunk_id: &str) -> Result<Option<MemoryChunk>> {
        let state = self.state.lock().await;
        Ok(state.chunks.get(chunk_id).cloned())
    }

    async fn get_session(&self, session_id: &str) -> Result<Option<MemorySession>> {
        let state = self.state.lock().await;
        Ok(state.sessions.get(session_id).cloned())
    }

    async fn list_sessions(&self, filter: SessionFilter) -> Result<Vec<MemorySession>> {
        let state = self.state.lock().await;
        let mut out: Vec<MemorySession> = state
            .sessions
            .values()
            .filter(|s| {
                if let Some(c) = &filter.caller_id {
                    if &s.caller_id != c {
                        return false;
                    }
                }
                if let Some(o) = filter.outcome {
                    if s.outcome != o {
                        return false;
                    }
                }
                if filter.open_only && s.outcome != SessionOutcome::Open {
                    return false;
                }
                if let Some(since) = filter.since {
                    if s.started_at < since {
                        return false;
                    }
                }
                if let Some(until) = filter.until {
                    if s.started_at > until {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();
        out.sort_by_key(|s| std::cmp::Reverse(s.started_at));
        if let Some(n) = filter.limit {
            out.truncate(n);
        }
        Ok(out)
    }

    // ─────────────── Writes ───────────────

    async fn write(&self, new_chunk: NewMemoryChunk) -> Result<MemoryChunk> {
        if new_chunk.content.trim().is_empty() {
            return Err(MemoryError::InvalidArgument(
                "content must not be empty".into(),
            ));
        }

        // Consult the optional pre-write hook (memory_write_attempted seam).
        // On Redact, return a synthetic chunk with a `redacted: true` marker
        // and DO NOT persist it. Callers that care can inspect the returned
        // chunk; most won't notice the difference.
        let importance = crate::importance::score(&new_chunk);
        if let Some(hook) = &self.write_hook {
            match hook.before_write(&new_chunk).await? {
                crate::WriteDecision::Allow => {}
                crate::WriteDecision::Redact { reason } => {
                    return Ok(MemoryChunk {
                        id: Self::next_id(),
                        created_at: Utc::now(),
                        kind: new_chunk.kind,
                        tier: MemoryTier::Session,
                        source: new_chunk.source,
                        session_id: new_chunk.session_id,
                        project_root: new_chunk.project_root,
                        caller_id: new_chunk.caller_id,
                        content: format!("<redacted: {reason}>"),
                        metadata: serde_json::json!({"redacted": true, "reason": reason}),
                        importance: 0.0,
                        pinned: false,
                        shareable: false,
                        superseded_by: None,
                        embedding_model: "none".into(),
                        embedding_dim: 0,
                    });
                }
            }
        }

        let chunk = MemoryChunk {
            id: Self::next_id(),
            created_at: Utc::now(),
            kind: new_chunk.kind,
            tier: MemoryTier::Session,
            source: new_chunk.source,
            session_id: new_chunk.session_id,
            project_root: new_chunk.project_root,
            caller_id: new_chunk.caller_id,
            content: new_chunk.content,
            metadata: new_chunk.metadata,
            importance,
            pinned: new_chunk.pinned,
            shareable: new_chunk.shareable,
            superseded_by: None,
            embedding_model: "none".into(),
            embedding_dim: 0,
        };
        let mut state = self.state.lock().await;
        state.chunks.insert(chunk.id.clone(), chunk.clone());
        Ok(chunk)
    }

    async fn write_batch(&self, chunks: Vec<NewMemoryChunk>) -> Result<Vec<MemoryChunk>> {
        // The v1 stub doesn't batch embeddings (there are none). Process
        // one-by-one to keep the impl honest.
        let mut out = Vec::with_capacity(chunks.len());
        for nc in chunks {
            out.push(self.write(nc).await?);
        }
        Ok(out)
    }

    async fn open_session(&self, init: NewMemorySession) -> Result<MemorySession> {
        let s = MemorySession {
            id: Self::next_id(),
            started_at: Utc::now(),
            ended_at: None,
            caller_id: init.caller_id,
            title: init.title,
            summary: None,
            outcome: SessionOutcome::Open,
            metadata: init.metadata,
        };
        let mut state = self.state.lock().await;
        state.sessions.insert(s.id.clone(), s.clone());
        Ok(s)
    }

    async fn close_session(&self, session_id: &str, outcome: SessionOutcome) -> Result<()> {
        let mut state = self.state.lock().await;
        let s = state
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| MemoryError::NotFound(format!("session {session_id}")))?;
        s.ended_at = Some(Utc::now());
        s.outcome = match outcome {
            // Refuse to close into `Open`.
            SessionOutcome::Open => SessionOutcome::Aborted,
            other => other,
        };
        Ok(())
    }

    async fn rename_session(&self, session_id: &str, title: &str) -> Result<()> {
        let mut state = self.state.lock().await;
        let s = state
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| MemoryError::NotFound(format!("session {session_id}")))?;
        s.title = Some(title.to_string());
        Ok(())
    }

    // ─────────────── Updates ───────────────

    async fn pin(&self, chunk_id: &str, pinned: bool) -> Result<()> {
        let mut state = self.state.lock().await;
        let c = state
            .chunks
            .get_mut(chunk_id)
            .ok_or_else(|| MemoryError::NotFound(format!("chunk {chunk_id}")))?;
        c.pinned = pinned;
        Ok(())
    }

    async fn update_importance(&self, _chunk_id: &str, _importance: f32) -> Result<()> {
        // v1 does not score importance. No-op rather than NotImplemented:
        // callers can fire-and-forget without conditional logic.
        Ok(())
    }

    async fn supersede(&self, _old_id: &str, _new_id: &str) -> Result<()> {
        // v1 does not track supersession. No-op (callers may set it via the
        // full impl later; safe to call from any v1 caller).
        Ok(())
    }

    async fn record_access(&self, chunk_id: &str, retrieved_by: &str, used: bool) -> Result<()> {
        let mut state = self.state.lock().await;
        if !state.chunks.contains_key(chunk_id) {
            return Err(MemoryError::NotFound(format!("chunk {chunk_id}")));
        }
        state.accesses.push(AccessEntry {
            ts: Utc::now(),
            chunk_id: chunk_id.to_string(),
            retrieved_by: retrieved_by.to_string(),
            query_hash: String::new(), // v1 stub doesn't compute query hashes
            rank: 0,
            used,
        });
        Ok(())
    }

    // ─────────────── Deletes ───────────────

    async fn delete(&self, chunk_id: &str, reason: EvictionReason) -> Result<()> {
        let mut state = self.state.lock().await;
        if state.chunks.remove(chunk_id).is_none() {
            return Err(MemoryError::NotFound(format!("chunk {chunk_id}")));
        }
        state.evictions.push(EvictionEntry {
            ts: Utc::now(),
            chunk_id: chunk_id.to_string(),
            reason,
            metadata: serde_json::Value::Null,
        });
        Ok(())
    }

    async fn delete_matching(
        &self,
        predicate: MemoryPredicate,
        reason: EvictionReason,
    ) -> Result<usize> {
        // Footgun guard: an empty predicate would match every chunk; callers
        // who want "delete everything" must use `purge_all`.
        if predicate.is_empty() {
            return Ok(0);
        }
        let mut state = self.state.lock().await;
        let now = Utc::now();
        let to_delete: Vec<String> = state
            .chunks
            .values()
            .filter(|c| chunk_matches(c, &predicate))
            .map(|c| c.id.clone())
            .collect();
        let count = to_delete.len();
        for id in to_delete {
            state.chunks.remove(&id);
            state.evictions.push(EvictionEntry {
                ts: now,
                chunk_id: id,
                reason,
                metadata: serde_json::Value::Null,
            });
        }
        Ok(count)
    }

    async fn purge_all(&self) -> Result<PurgeReport> {
        let mut state = self.state.lock().await;
        let report = PurgeReport {
            chunks_deleted: state.chunks.len(),
            sessions_deleted: state.sessions.len(),
            access_log_deleted: state.accesses.len(),
            eviction_log_deleted: state.evictions.len(),
        };
        state.chunks.clear();
        state.sessions.clear();
        state.accesses.clear();
        state.evictions.clear();
        Ok(report)
    }

    // ─────────────── Summarization (NotImplemented in v1) ───────────────

    async fn summarize_session(&self, _session_id: &str) -> Result<MemoryChunk> {
        Err(MemoryError::NotImplemented("summarize_session"))
    }

    async fn rollup_day(&self, _date: NaiveDate) -> Result<Vec<MemoryChunk>> {
        Err(MemoryError::NotImplemented("rollup_day"))
    }

    async fn rollup_rule_week(
        &self,
        _rule_id: &str,
        _week_start: NaiveDate,
    ) -> Result<MemoryChunk> {
        Err(MemoryError::NotImplemented("rollup_rule_week"))
    }

    // ─────────────── Maintenance ───────────────

    async fn run_aging_sweep(&self) -> Result<AgingReport> {
        // v1 stub: a 30-day retention sweep over non-pinned non-correction
        // chunks. No importance scoring; honest minimum.
        const RETENTION_DAYS: i64 = 30;
        let cutoff = Utc::now() - chrono::Duration::days(RETENTION_DAYS);
        let mut state = self.state.lock().await;
        let to_delete: Vec<String> = state
            .chunks
            .values()
            .filter(|c| !c.pinned && c.kind != ChunkKind::Correction && c.created_at < cutoff)
            .map(|c| c.id.clone())
            .collect();
        let deleted = to_delete.len();
        let now = Utc::now();
        for id in to_delete {
            state.chunks.remove(&id);
            state.evictions.push(EvictionEntry {
                ts: now,
                chunk_id: id,
                reason: EvictionReason::Aging,
                metadata: serde_json::Value::Null,
            });
        }
        Ok(AgingReport {
            tier_promoted: 0, // v1 doesn't transition tiers
            deleted,
            bytes_reclaimed: 0,
            deletions_by_reason: vec![(EvictionReason::Aging, deleted)],
        })
    }

    async fn re_embed_all(&self, _target_model: &str) -> Result<ReEmbedReport> {
        Err(MemoryError::NotImplemented("re_embed_all"))
    }

    async fn export(&self, filter: ExportFilter) -> Result<ExportBundle> {
        let state = self.state.lock().await;
        let chunks: Vec<MemoryChunk> = state
            .chunks
            .values()
            .filter(|c| match &filter.predicate {
                Some(p) if !p.is_empty() => chunk_matches(c, p),
                _ => true,
            })
            .cloned()
            .collect();

        let session_ids: std::collections::HashSet<String> =
            chunks.iter().filter_map(|c| c.session_id.clone()).collect();
        let sessions = if filter.include_sessions {
            state
                .sessions
                .values()
                .filter(|s| session_ids.contains(&s.id))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let evictions = if filter.include_eviction_log {
            state.evictions.clone()
        } else {
            Vec::new()
        };
        let accesses = if filter.include_access_log {
            state.accesses.clone()
        } else {
            Vec::new()
        };
        Ok(ExportBundle {
            chunks,
            sessions,
            evictions,
            accesses,
        })
    }

    async fn stats(&self) -> Result<MemoryStats> {
        let state = self.state.lock().await;
        let total = state.chunks.len();
        Ok(MemoryStats {
            total_chunks: total,
            session_chunks: state
                .chunks
                .values()
                .filter(|c| c.tier == MemoryTier::Session)
                .count(),
            long_term_chunks: state
                .chunks
                .values()
                .filter(|c| c.tier == MemoryTier::LongTerm)
                .count(),
            total_sessions: state.sessions.len(),
            open_sessions: state
                .sessions
                .values()
                .filter(|s| s.outcome == SessionOutcome::Open)
                .count(),
            db_bytes: 0, // in-memory: not meaningful in v1
            embedding_model: None,
        })
    }
}

fn chunk_matches(c: &MemoryChunk, p: &MemoryPredicate) -> bool {
    if let Some(kinds) = &p.kinds {
        if !kinds.contains(&c.kind) {
            return false;
        }
    }
    if let Some(callers) = &p.callers {
        if !callers.iter().any(|x| x == &c.caller_id) {
            return false;
        }
    }
    if let Some(sids) = &p.session_ids {
        match &c.session_id {
            Some(id) if sids.iter().any(|x| x == id) => {}
            _ => return false,
        }
    }
    if let Some(prefix) = &p.project_root_prefix {
        match &c.project_root {
            Some(root) if root.starts_with(prefix.as_str()) => {}
            _ => return false,
        }
    }
    if let Some(before) = p.before {
        if c.created_at >= before {
            return false;
        }
    }
    if let Some(after) = p.after {
        if c.created_at <= after {
            return false;
        }
    }
    if let Some(pinned) = p.pinned {
        if c.pinned != pinned {
            return false;
        }
    }
    if let Some(below) = p.importance_below {
        if c.importance >= below {
            return false;
        }
    }
    if let Some(needle) = &p.content_contains {
        if !c.content.to_lowercase().contains(&needle.to_lowercase()) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::ChunkSource;
    use crate::query::{CallerScope, RetrievalProfile};
    use serde_json::json;

    fn nc(caller: &str, content: &str) -> NewMemoryChunk {
        NewMemoryChunk {
            kind: ChunkKind::Chat,
            source: ChunkSource::Embedded,
            session_id: None,
            project_root: None,
            caller_id: caller.into(),
            content: content.into(),
            metadata: json!(null),
            importance: None,
            shareable: false,
            pinned: false,
        }
    }

    fn q(caller: &str, text: &str) -> MemoryQuery {
        MemoryQuery {
            text: text.into(),
            kinds: None,
            since: None,
            until: None,
            session_id: None,
            caller_scope: CallerScope::Own,
            project_root_prefix: None,
            k: 8,
            include_rollups: true,
            min_importance: None,
            profile: RetrievalProfile::AgentChatTurn,
            caller_id: caller.into(),
        }
    }

    #[tokio::test]
    async fn write_and_retrieve_round_trips() {
        let m = BasicMemoryProvider::new();
        let c = m
            .write(nc("embedded", "Q4 report is filed under Workspace"))
            .await
            .unwrap();
        let hits = m.retrieve(q("embedded", "q4 report")).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, c.id);
    }

    #[tokio::test]
    async fn retrieve_respects_caller_scope_own() {
        let m = BasicMemoryProvider::new();
        m.write(nc("embedded", "my secret")).await.unwrap();
        m.write(nc("mcp:cursor", "my secret")).await.unwrap();
        let hits = m.retrieve(q("embedded", "secret")).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].caller_id, "embedded");
    }

    #[tokio::test]
    async fn retrieve_global_scope_sees_all() {
        let m = BasicMemoryProvider::new();
        m.write(nc("embedded", "my secret")).await.unwrap();
        m.write(nc("mcp:cursor", "my secret")).await.unwrap();
        let mut query = q("audit", "secret");
        query.caller_scope = CallerScope::Global;
        let hits = m.retrieve(query).await.unwrap();
        assert_eq!(hits.len(), 2);
    }

    #[tokio::test]
    async fn retrieve_own_plus_shared_surfaces_shareable_across_callers() {
        let m = BasicMemoryProvider::new();
        // Two callers, two chunks: one shareable from "mcp:cursor",
        // one private from "mcp:cursor". A third caller asks with
        // OwnPlusShared scope; only the shareable one surfaces.
        let mut shared = nc("mcp:cursor", "user prefers dry-run mode");
        shared.shareable = true;
        m.write(shared).await.unwrap();
        m.write(nc("mcp:cursor", "user said hi to cursor"))
            .await
            .unwrap();
        m.write(nc("mcp:codex", "user said hi to codex"))
            .await
            .unwrap();
        let mut query = q("embedded", "user");
        query.caller_scope = CallerScope::OwnPlusShared;
        let hits = m.retrieve(query).await.unwrap();
        // Only the shareable chunk is visible to the embedded caller.
        assert_eq!(hits.len(), 1);
        assert!(hits[0].content.contains("dry-run"));
        assert!(hits[0].shareable);
    }

    #[tokio::test]
    async fn retrieve_own_plus_shared_includes_own_unshared() {
        let m = BasicMemoryProvider::new();
        // The caller's own chunks are always visible under OwnPlusShared
        // regardless of the shareable flag.
        m.write(nc("embedded", "embedded private note"))
            .await
            .unwrap();
        m.write(nc("mcp:cursor", "cursor private note"))
            .await
            .unwrap();
        let mut query = q("embedded", "note");
        query.caller_scope = CallerScope::OwnPlusShared;
        let hits = m.retrieve(query).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].caller_id, "embedded");
    }

    #[tokio::test]
    async fn retrieve_orders_recent_first() {
        let m = BasicMemoryProvider::new();
        m.write(nc("embedded", "first about cats")).await.unwrap();
        // Small delay to ensure created_at differs.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let second = m.write(nc("embedded", "second about cats")).await.unwrap();
        let hits = m.retrieve(q("embedded", "cats")).await.unwrap();
        assert_eq!(hits[0].id, second.id);
    }

    #[tokio::test]
    async fn retrieve_kind_filter() {
        let m = BasicMemoryProvider::new();
        let chat = m.write(nc("embedded", "alpha")).await.unwrap();
        let mut action = nc("embedded", "alpha");
        action.kind = ChunkKind::Action;
        let _ = m.write(action).await.unwrap();
        let mut query = q("embedded", "alpha");
        query.kinds = Some(vec![ChunkKind::Chat]);
        let hits = m.retrieve(query).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, chat.id);
    }

    #[tokio::test]
    async fn empty_query_text_rejected() {
        let m = BasicMemoryProvider::new();
        let err = m.retrieve(q("embedded", "   ")).await.unwrap_err();
        assert!(matches!(err, MemoryError::InvalidArgument(_)));
    }

    #[tokio::test]
    async fn empty_content_rejected() {
        let m = BasicMemoryProvider::new();
        let err = m.write(nc("embedded", "")).await.unwrap_err();
        assert!(matches!(err, MemoryError::InvalidArgument(_)));
    }

    #[tokio::test]
    async fn session_open_close() {
        let m = BasicMemoryProvider::new();
        let s = m
            .open_session(NewMemorySession {
                caller_id: "embedded".into(),
                title: Some("test".into()),
                metadata: json!(null),
            })
            .await
            .unwrap();
        assert_eq!(s.outcome, SessionOutcome::Open);
        m.close_session(&s.id, SessionOutcome::Success)
            .await
            .unwrap();
        let s2 = m.get_session(&s.id).await.unwrap().unwrap();
        assert_eq!(s2.outcome, SessionOutcome::Success);
        assert!(s2.ended_at.is_some());
    }

    #[tokio::test]
    async fn close_session_unknown_id_errors() {
        let m = BasicMemoryProvider::new();
        let err = m
            .close_session("nope", SessionOutcome::Success)
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::NotFound(_)));
    }

    #[tokio::test]
    async fn delete_matching_empty_predicate_is_noop() {
        let m = BasicMemoryProvider::new();
        m.write(nc("embedded", "x")).await.unwrap();
        let n = m
            .delete_matching(MemoryPredicate::default(), EvictionReason::UserDelete)
            .await
            .unwrap();
        assert_eq!(n, 0);
        let stats = m.stats().await.unwrap();
        assert_eq!(stats.total_chunks, 1);
    }

    #[tokio::test]
    async fn delete_matching_by_caller() {
        let m = BasicMemoryProvider::new();
        m.write(nc("embedded", "x")).await.unwrap();
        m.write(nc("mcp:cursor", "y")).await.unwrap();
        let n = m
            .delete_matching(
                MemoryPredicate {
                    callers: Some(vec!["embedded".into()]),
                    ..Default::default()
                },
                EvictionReason::UserDelete,
            )
            .await
            .unwrap();
        assert_eq!(n, 1);
        let stats = m.stats().await.unwrap();
        assert_eq!(stats.total_chunks, 1);
    }

    #[tokio::test]
    async fn purge_all_clears_state() {
        let m = BasicMemoryProvider::new();
        m.write(nc("embedded", "x")).await.unwrap();
        let _ = m
            .open_session(NewMemorySession {
                caller_id: "embedded".into(),
                title: None,
                metadata: json!(null),
            })
            .await
            .unwrap();
        let r = m.purge_all().await.unwrap();
        assert_eq!(r.chunks_deleted, 1);
        assert_eq!(r.sessions_deleted, 1);
        let stats = m.stats().await.unwrap();
        assert_eq!(stats.total_chunks, 0);
        assert_eq!(stats.total_sessions, 0);
    }

    #[tokio::test]
    async fn summarize_returns_not_implemented() {
        let m = BasicMemoryProvider::new();
        let err = m.summarize_session("anything").await.unwrap_err();
        assert!(matches!(
            err,
            MemoryError::NotImplemented("summarize_session")
        ));
    }

    #[tokio::test]
    async fn re_embed_returns_not_implemented() {
        let m = BasicMemoryProvider::new();
        let err = m.re_embed_all("bge-small-en-v1.5").await.unwrap_err();
        assert!(matches!(err, MemoryError::NotImplemented("re_embed_all")));
    }

    #[tokio::test]
    async fn update_importance_and_supersede_noop() {
        let m = BasicMemoryProvider::new();
        let c = m.write(nc("embedded", "x")).await.unwrap();
        m.update_importance(&c.id, 0.9).await.unwrap();
        m.supersede(&c.id, "other").await.unwrap();
        // Importance unchanged: v1 stub no-ops.
        let got = m.get(&c.id).await.unwrap().unwrap();
        assert_eq!(got.importance, 0.5);
        assert!(got.superseded_by.is_none());
    }

    #[tokio::test]
    async fn export_filters_by_predicate() {
        let m = BasicMemoryProvider::new();
        m.write(nc("embedded", "alpha")).await.unwrap();
        m.write(nc("embedded", "beta")).await.unwrap();
        let bundle = m
            .export(ExportFilter {
                predicate: Some(MemoryPredicate {
                    content_contains: Some("alpha".into()),
                    ..Default::default()
                }),
                include_eviction_log: false,
                include_access_log: false,
                include_sessions: true,
            })
            .await
            .unwrap();
        assert_eq!(bundle.chunks.len(), 1);
        assert!(bundle.chunks[0].content.contains("alpha"));
    }

    #[tokio::test]
    async fn aging_sweep_deletes_old_unpinned() {
        let m = BasicMemoryProvider::new();
        let fresh = m.write(nc("embedded", "fresh")).await.unwrap();
        // Forcibly age a chunk by editing the state directly.
        {
            let mut state = m.state.lock().await;
            let c = state.chunks.get_mut(&fresh.id).unwrap();
            c.created_at = Utc::now() - chrono::Duration::days(45);
        }
        let r = m.run_aging_sweep().await.unwrap();
        assert_eq!(r.deleted, 1);
        let stats = m.stats().await.unwrap();
        assert_eq!(stats.total_chunks, 0);
    }

    #[tokio::test]
    async fn aging_sweep_preserves_pinned_and_corrections() {
        let m = BasicMemoryProvider::new();
        let pinned = m.write(nc("embedded", "pinned")).await.unwrap();
        let mut correction_nc = nc("embedded", "correction");
        correction_nc.kind = ChunkKind::Correction;
        let correction = m.write(correction_nc).await.unwrap();
        {
            let mut state = m.state.lock().await;
            for id in [&pinned.id, &correction.id] {
                let c = state.chunks.get_mut(id).unwrap();
                c.created_at = Utc::now() - chrono::Duration::days(45);
            }
            // Pin the pinned one.
            let c = state.chunks.get_mut(&pinned.id).unwrap();
            c.pinned = true;
        }
        let r = m.run_aging_sweep().await.unwrap();
        assert_eq!(r.deleted, 0);
    }
}
