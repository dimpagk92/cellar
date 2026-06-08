//! `cellar eval memory` — recall benchmark for the memory subsystem.
//!
//! Runs a JSONL corpus of `(query, expected_chunk_ids, corpus_chunks)`
//! triples against a [`SqliteMemoryProvider`] seeded with the corpus
//! and emits Recall@5, Recall@1, and Mean Reciprocal Rank (MRR)
//! aggregated per chunk-kind.
//!
//! The benchmark stays self-contained — it does NOT touch the user's
//! production memory DB and it does NOT speak to the running daemon.
//! Each pair is evaluated against a fresh, in-memory provider seeded
//! with that pair's `corpus_chunks`. This keeps the harness reproducible
//! and lets the user grow the corpus from 20 placeholders to the
//! 200-pair target (`cellar-memory-manager.md` §14.1) without worrying
//! about cross-query state leakage.
//!
//! See `cellar-memory-manager.md` §14.1 for the targets:
//! Recall@5 ≥ 0.85, Recall@1 ≥ 0.55, MRR ≥ 0.65 against a hand-labelled
//! benchmark of ~200 pairs.
//!
//! ### CLI surface
//!
//! ```text
//! cellar eval memory --corpus eval/memory/queries.jsonl
//! cellar eval memory --corpus eval/memory/queries.jsonl --profile agent_chat_turn
//! cellar eval memory --corpus eval/memory/queries.jsonl --json
//! ```
//!
//! ### Exit codes
//!
//! - `0` — corpus parsed and every query produced a result row (no
//!   inference about whether the targets were *met*; the CLI is
//!   informational, not gating).
//! - `1` — corpus could not be parsed, the seed provider failed to
//!   open, or a query against the seeded provider returned an error.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use cel_memory::{
    CallerScope, ChunkKind, ChunkSource, MemoryProvider, MemoryQuery, NewMemoryChunk,
    RetrievalProfile,
};
use cel_memory_sqlite::{MockEmbedder, SqliteMemoryProvider};
use clap::Subcommand;
use serde::{Deserialize, Serialize};

/// The `cellar eval` family. Today only `memory` is implemented; future
/// work (`cellar eval rules`, `cellar eval planner`) hangs off the same
/// subcommand spine.
#[derive(Debug, Subcommand)]
pub enum EvalCmd {
    /// Run the memory recall benchmark.
    ///
    /// Reads a JSONL corpus of `(query, expected_chunk_ids, corpus_chunks)`
    /// triples, seeds a fresh in-memory `SqliteMemoryProvider` per pair,
    /// retrieves with the configured profile, and emits Recall@5,
    /// Recall@1, and MRR aggregated per chunk-kind.
    Memory {
        /// Path to the JSONL corpus. Each line is a [`QueryPair`].
        ///
        /// Defaults to `eval/memory/queries.jsonl` relative to the
        /// current working directory (the worktree layout).
        #[arg(long, default_value = "eval/memory/queries.jsonl")]
        corpus: PathBuf,

        /// Override the per-pair retrieval profile. When unset, each
        /// pair's `profile` field is honored. Useful for sweeping a
        /// single profile across a heterogenous corpus.
        ///
        /// Accepted values (snake_case): `agent_chat_turn`,
        /// `agent_delegated_job`, `nl_compiler_similar_rules`,
        /// `nl_compiler_similar_fires`, `audit_timeline`, `user_search`.
        #[arg(long)]
        profile: Option<String>,
    },
}

/// One row in the corpus JSONL.
///
/// Each pair is independent: the harness opens a fresh in-memory
/// provider, writes `corpus_chunks`, runs `query` against it, and
/// scores the result against `expected_chunk_ids`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryPair {
    /// Stable identifier surfaced in failure reports. Convention:
    /// `q_<7-digit-padded-int>` to keep the JSONL sortable.
    pub id: String,
    /// The chunk kind the query is targeting (used to aggregate
    /// per-kind metrics). Must match one of [`ChunkKind`]; serialised
    /// snake_case (`chat`, `action`, `fire`, `observation`,
    /// `correction`, `job_summary`, `context`, `rollup`).
    pub kind: ChunkKind,
    /// The free-text query string passed to
    /// [`MemoryProvider::retrieve`].
    pub query: String,
    /// Retrieval profile. Defaults to `AgentChatTurn` if absent. The
    /// CLI `--profile` flag overrides this if supplied.
    #[serde(default)]
    pub profile: RetrievalProfile,
    /// Caller identifier the retrieval runs as. Defaults to
    /// `"embedded"` if absent.
    #[serde(default = "default_caller")]
    pub caller_id: String,
    /// Top-k retrieved by the query. Defaults to 5.
    #[serde(default = "default_k")]
    pub k: usize,
    /// IDs of the chunks that should appear in the top-k for the query
    /// to be "correct" under Recall@5. At minimum one expected ID is
    /// required for a meaningful score.
    pub expected_chunk_ids: Vec<String>,
    /// Chunks the harness writes into the provider before running the
    /// query. Identifiers here are matched against
    /// `expected_chunk_ids`. The harness does NOT pre-create sessions;
    /// every chunk lives at the top level.
    pub corpus_chunks: Vec<CorpusChunk>,
}

fn default_caller() -> String {
    "embedded".into()
}

fn default_k() -> usize {
    5
}

/// A single chunk to seed into the provider for a pair. Mirrors
/// [`NewMemoryChunk`] but assigns the ID explicitly so the harness can
/// compare results.
///
/// The harness writes this into the provider via a custom path (not
/// `MemoryProvider::write`) because the trait assigns IDs at write
/// time. To keep the corpus stable, the eval writes via the same
/// `write` call but then maps the harness-supplied id to the
/// provider-assigned id internally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusChunk {
    /// Stable identifier for matching against `expected_chunk_ids`.
    pub id: String,
    /// Chunk kind (mirrors [`ChunkKind`]).
    pub kind: ChunkKind,
    /// Body text — indexed by FTS, embedded.
    pub content: String,
    /// Optional caller — defaults to `"embedded"`. When the harness
    /// runs a query under a different `caller_id`, ensure this matches
    /// (or use the `Global` scope by leaving `caller_id` unset on the
    /// query — the harness defaults to `Own`).
    #[serde(default = "default_caller")]
    pub caller_id: String,
    /// Optional session grouping.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Optional project root.
    #[serde(default)]
    pub project_root: Option<String>,
    /// Optional explicit importance — clamped to [0, 1] by the
    /// provider's scorer.
    #[serde(default)]
    pub importance: Option<f32>,
}

/// Per-pair result row.
#[derive(Debug, Clone, Serialize)]
pub struct PairResult {
    /// Pair id.
    pub id: String,
    /// Pair kind.
    pub kind: ChunkKind,
    /// True if any expected chunk landed in top-k.
    pub recall_at_k: bool,
    /// True if the top-ranked chunk is an expected one.
    pub recall_at_1: bool,
    /// Reciprocal rank of the first expected chunk in the result list;
    /// `0.0` if no expected chunk was found.
    pub reciprocal_rank: f32,
    /// The k that was used (for transparency — overrides surface here).
    pub k: usize,
}

/// Aggregated metrics across a slice of [`PairResult`].
///
/// Stored per kind and across all kinds. Targets per §14.1:
///   Recall@5 ≥ 0.85, Recall@1 ≥ 0.55, MRR ≥ 0.65.
#[derive(Debug, Clone, Serialize)]
pub struct AggregatedMetrics {
    /// Number of pairs in the aggregate.
    pub n: usize,
    /// Mean Recall@k (k = each pair's configured k).
    pub recall_at_k: f32,
    /// Mean Recall@1.
    pub recall_at_1: f32,
    /// Mean Reciprocal Rank.
    pub mrr: f32,
}

impl AggregatedMetrics {
    /// Compute aggregates over a slice of pair results. Empty slice
    /// yields zeros across the board.
    pub fn from_results(results: &[PairResult]) -> Self {
        let n = results.len();
        if n == 0 {
            return Self {
                n: 0,
                recall_at_k: 0.0,
                recall_at_1: 0.0,
                mrr: 0.0,
            };
        }
        let n_f = n as f32;
        let recall_at_k = results.iter().filter(|r| r.recall_at_k).count() as f32 / n_f;
        let recall_at_1 = results.iter().filter(|r| r.recall_at_1).count() as f32 / n_f;
        let mrr: f32 = results.iter().map(|r| r.reciprocal_rank).sum::<f32>() / n_f;
        Self {
            n,
            recall_at_k,
            recall_at_1,
            mrr,
        }
    }
}

/// Final report assembled by [`run_memory_eval`].
#[derive(Debug, Clone, Serialize)]
pub struct MemoryEvalReport {
    /// Per-pair scores in input order.
    pub pairs: Vec<PairResult>,
    /// Aggregates across the whole corpus.
    pub overall: AggregatedMetrics,
    /// Aggregates grouped by chunk kind.
    pub by_kind: BTreeMap<String, AggregatedMetrics>,
    /// Path the corpus was read from (echoed in JSON output so log
    /// readers can correlate runs to inputs).
    pub corpus: String,
    /// Profile applied across the run, or `null` if each pair
    /// used its own profile.
    pub profile_override: Option<String>,
}

// ─────────────────────────── corpus parsing ───────────────────────────

/// Parse a JSONL corpus file. Blank lines and lines starting with `#`
/// are skipped (the latter so corpus authors can leave provenance notes
/// inline).
pub fn parse_corpus(path: &Path) -> Result<Vec<QueryPair>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read corpus at {}", path.display()))?;
    let mut pairs = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let pair: QueryPair = serde_json::from_str(trimmed).with_context(|| {
            format!(
                "parse line {} of {} as QueryPair JSON",
                i + 1,
                path.display()
            )
        })?;
        pairs.push(pair);
    }
    Ok(pairs)
}

/// Translate a CLI `--profile` flag to a [`RetrievalProfile`]. Returns
/// `None` for unknown spellings so the caller can surface a friendly
/// error rather than silently falling back to the default.
pub fn profile_from_str(s: &str) -> Option<RetrievalProfile> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "agent_chat_turn" | "agentchatturn" => RetrievalProfile::AgentChatTurn,
        "agent_delegated_job" | "agentdelegatedjob" => RetrievalProfile::AgentDelegatedJob,
        "nl_compiler_similar_rules" | "nlcompilersimilarrules" => {
            RetrievalProfile::NLCompilerSimilarRules
        }
        "nl_compiler_similar_fires" | "nlcompilersimilarfires" => {
            RetrievalProfile::NLCompilerSimilarFires
        }
        "audit_timeline" | "audittimeline" => RetrievalProfile::AuditTimeline,
        "user_search" | "usersearch" => RetrievalProfile::UserSearch,
        _ => return None,
    })
}

/// Snake-case label for a [`ChunkKind`], used as the key in the
/// per-kind report so the JSON output matches the JSONL `kind` field.
fn kind_label(k: ChunkKind) -> &'static str {
    match k {
        ChunkKind::Chat => "chat",
        ChunkKind::Action => "action",
        ChunkKind::Fire => "fire",
        ChunkKind::Observation => "observation",
        ChunkKind::Correction => "correction",
        ChunkKind::JobSummary => "job_summary",
        ChunkKind::Context => "context",
        ChunkKind::Rollup => "rollup",
    }
}

// ─────────────────────────── eval loop ───────────────────────────

/// Run the full eval. Each pair gets its own fresh in-memory
/// `SqliteMemoryProvider` seeded with the pair's `corpus_chunks`.
///
/// `profile_override` lets the CLI force every pair to use the same
/// profile (useful for sweeps); `None` honors each pair's own profile.
pub async fn run_memory_eval(
    pairs: Vec<QueryPair>,
    profile_override: Option<RetrievalProfile>,
    corpus_path: String,
) -> Result<MemoryEvalReport> {
    let mut results = Vec::with_capacity(pairs.len());
    for pair in &pairs {
        let row = evaluate_pair(pair, profile_override).await?;
        results.push(row);
    }

    let overall = AggregatedMetrics::from_results(&results);
    let mut by_kind: BTreeMap<String, AggregatedMetrics> = BTreeMap::new();
    let mut buckets: BTreeMap<String, Vec<PairResult>> = BTreeMap::new();
    for r in &results {
        buckets
            .entry(kind_label(r.kind).to_string())
            .or_default()
            .push(r.clone());
    }
    for (label, rs) in buckets {
        by_kind.insert(label, AggregatedMetrics::from_results(&rs));
    }

    let profile_override_label = profile_override.map(|p| profile_label(p).to_string());
    Ok(MemoryEvalReport {
        pairs: results,
        overall,
        by_kind,
        corpus: corpus_path,
        profile_override: profile_override_label,
    })
}

fn profile_label(p: RetrievalProfile) -> &'static str {
    match p {
        RetrievalProfile::AgentChatTurn => "agent_chat_turn",
        RetrievalProfile::AgentDelegatedJob => "agent_delegated_job",
        RetrievalProfile::NLCompilerSimilarRules => "nl_compiler_similar_rules",
        RetrievalProfile::NLCompilerSimilarFires => "nl_compiler_similar_fires",
        RetrievalProfile::AuditTimeline => "audit_timeline",
        RetrievalProfile::UserSearch => "user_search",
    }
}

/// Seed a fresh in-memory provider, write the corpus chunks, run the
/// query, score the result. Returns a [`PairResult`] suitable for
/// aggregation.
async fn evaluate_pair(
    pair: &QueryPair,
    profile_override: Option<RetrievalProfile>,
) -> Result<PairResult> {
    let embedder = Arc::new(MockEmbedder::new());
    let provider = SqliteMemoryProvider::open_in_memory(embedder)
        .await
        .map_err(|e| anyhow!("open in-memory provider for pair {}: {e}", pair.id))?;

    // Maps the harness-supplied chunk id (which lives in
    // `expected_chunk_ids`) to the provider-assigned id (which is what
    // we actually see in retrieval results). Establishing the mapping
    // is the only reason we don't just batch-write.
    let mut id_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for cc in &pair.corpus_chunks {
        let nc = NewMemoryChunk {
            kind: cc.kind,
            source: ChunkSource::Embedded,
            session_id: cc.session_id.clone(),
            project_root: cc.project_root.clone(),
            caller_id: cc.caller_id.clone(),
            content: cc.content.clone(),
            metadata: serde_json::Value::Null,
            importance: cc.importance,
            shareable: false,
            pinned: false,
        };
        let written = provider
            .write(nc)
            .await
            .map_err(|e| anyhow!("write corpus chunk {} for pair {}: {e}", cc.id, pair.id))?;
        id_map.insert(cc.id.clone(), written.id.clone());
    }

    let profile = profile_override.unwrap_or(pair.profile);
    let q = MemoryQuery {
        text: pair.query.clone(),
        kinds: None,
        since: None,
        until: None,
        session_id: None,
        // Default Own scope: per-pair caller_id matches the seeded
        // chunks' caller_id under our defaults. Authors can grow into
        // Global by setting `caller_id` mismatched in the corpus.
        caller_scope: CallerScope::Own,
        project_root_prefix: None,
        k: pair.k,
        include_rollups: true,
        min_importance: None,
        profile,
        caller_id: pair.caller_id.clone(),
    };
    let hits = provider
        .retrieve(q)
        .await
        .map_err(|e| anyhow!("retrieve for pair {}: {e}", pair.id))?;

    let expected_provider_ids: std::collections::HashSet<String> = pair
        .expected_chunk_ids
        .iter()
        .filter_map(|harness_id| id_map.get(harness_id).cloned())
        .collect();

    Ok(score_hits(pair, &hits, &expected_provider_ids))
}

/// Compute the per-pair metrics from the ranked hits. Pure — no I/O —
/// so it can be unit-tested without touching the provider.
fn score_hits(
    pair: &QueryPair,
    hits: &[cel_memory::MemoryChunk],
    expected_ids: &std::collections::HashSet<String>,
) -> PairResult {
    let mut recall_at_k = false;
    let mut recall_at_1 = false;
    let mut reciprocal_rank = 0.0f32;

    for (rank, hit) in hits.iter().enumerate() {
        if expected_ids.contains(&hit.id) {
            recall_at_k = true;
            if rank == 0 {
                recall_at_1 = true;
            }
            if reciprocal_rank == 0.0 {
                // 1-indexed reciprocal rank (the convention).
                reciprocal_rank = 1.0 / ((rank + 1) as f32);
            }
        }
    }

    PairResult {
        id: pair.id.clone(),
        kind: pair.kind,
        recall_at_k,
        recall_at_1,
        reciprocal_rank,
        k: pair.k,
    }
}

// ─────────────────────────── reporting ───────────────────────────

/// Render a report as a human-readable table. Sorted: per-kind first
/// alphabetically, then overall last so users see the headline number
/// at the bottom.
pub fn render_report(report: &MemoryEvalReport) -> String {
    let mut out = String::new();
    out.push_str("[cellar eval memory]\n");
    out.push_str(&format!("corpus: {}\n", report.corpus));
    if let Some(p) = &report.profile_override {
        out.push_str(&format!("profile override: {p}\n"));
    } else {
        out.push_str("profile override: (per-pair default)\n");
    }
    out.push_str(&format!("pairs: {}\n\n", report.overall.n));

    out.push_str(&format!(
        "{:<14}  {:>5}  {:>8}  {:>8}  {:>6}\n",
        "kind", "n", "recall@k", "recall@1", "mrr"
    ));
    out.push_str(&"-".repeat(50));
    out.push('\n');
    for (label, m) in &report.by_kind {
        out.push_str(&format!(
            "{:<14}  {:>5}  {:>8.3}  {:>8.3}  {:>6.3}\n",
            label, m.n, m.recall_at_k, m.recall_at_1, m.mrr
        ));
    }
    out.push_str(&"-".repeat(50));
    out.push('\n');
    out.push_str(&format!(
        "{:<14}  {:>5}  {:>8.3}  {:>8.3}  {:>6.3}\n",
        "OVERALL",
        report.overall.n,
        report.overall.recall_at_k,
        report.overall.recall_at_1,
        report.overall.mrr
    ));

    // Targets from §14.1. Informational only — the harness never fails
    // on missed targets; the user is expected to track these over
    // iteration.
    out.push('\n');
    out.push_str("targets (cellar-memory-manager.md §14.1):  recall@5 ≥ 0.85,  recall@1 ≥ 0.55,  mrr ≥ 0.65\n");
    out
}

// ─────────────────────────── orchestration ───────────────────────────

/// Run a `cellar eval memory` subcommand to completion. Returns the
/// process exit code (`0` on success, `1` on any error). The CLI top
/// level calls this and propagates the code via `std::process::exit`.
pub async fn run(cmd: EvalCmd, json: bool) -> Result<i32> {
    match cmd {
        EvalCmd::Memory { corpus, profile } => {
            let pairs = parse_corpus(&corpus)?;
            let profile_override = match profile.as_deref() {
                None => None,
                Some(s) => Some(profile_from_str(s).ok_or_else(|| {
                    anyhow!(
                        "unknown profile `{s}` (expected one of: agent_chat_turn, \
                         agent_delegated_job, nl_compiler_similar_rules, \
                         nl_compiler_similar_fires, audit_timeline, user_search)"
                    )
                })?),
            };
            let report =
                run_memory_eval(pairs, profile_override, corpus.display().to_string()).await?;
            if json {
                let s = serde_json::to_string_pretty(&report)
                    .context("serialize eval report as JSON")?;
                println!("{s}");
            } else {
                print!("{}", render_report(&report));
            }
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel_memory::{MemoryChunk, MemoryTier};
    use chrono::Utc;

    // ───── profile_from_str ─────

    #[test]
    fn profile_from_str_accepts_known_variants() {
        assert_eq!(
            profile_from_str("agent_chat_turn"),
            Some(RetrievalProfile::AgentChatTurn)
        );
        assert_eq!(
            profile_from_str("AGENT_CHAT_TURN"),
            Some(RetrievalProfile::AgentChatTurn)
        );
        assert_eq!(
            profile_from_str("user_search"),
            Some(RetrievalProfile::UserSearch)
        );
        assert_eq!(
            profile_from_str("audit_timeline"),
            Some(RetrievalProfile::AuditTimeline)
        );
    }

    #[test]
    fn profile_from_str_rejects_unknown() {
        assert!(profile_from_str("nonsense").is_none());
        assert!(profile_from_str("").is_none());
    }

    // ───── score_hits ─────

    fn chunk(id: &str, content: &str) -> MemoryChunk {
        MemoryChunk {
            id: id.into(),
            created_at: Utc::now(),
            kind: ChunkKind::Chat,
            tier: MemoryTier::Session,
            source: ChunkSource::Embedded,
            session_id: None,
            project_root: None,
            caller_id: "embedded".into(),
            content: content.into(),
            metadata: serde_json::Value::Null,
            importance: 0.5,
            pinned: false,
            shareable: false,
            superseded_by: None,
            embedding_model: "mock".into(),
            embedding_dim: 384,
        }
    }

    fn pair(id: &str, k: usize, expected: &[&str]) -> QueryPair {
        QueryPair {
            id: id.into(),
            kind: ChunkKind::Chat,
            query: "q".into(),
            profile: RetrievalProfile::AgentChatTurn,
            caller_id: "embedded".into(),
            k,
            expected_chunk_ids: expected.iter().map(|s| s.to_string()).collect(),
            corpus_chunks: vec![],
        }
    }

    #[test]
    fn score_hits_recall_at_1_when_top_match() {
        let hits = vec![chunk("c_a", ""), chunk("c_b", ""), chunk("c_c", "")];
        let expected = ["c_a".to_string()].into_iter().collect();
        let pair = pair("q1", 5, &["c_a"]);
        let r = score_hits(&pair, &hits, &expected);
        assert!(r.recall_at_k);
        assert!(r.recall_at_1);
        assert!((r.reciprocal_rank - 1.0).abs() < 1e-6);
    }

    #[test]
    fn score_hits_recall_at_k_but_not_at_1() {
        let hits = vec![chunk("c_x", ""), chunk("c_a", ""), chunk("c_b", "")];
        let expected = ["c_a".to_string()].into_iter().collect();
        let pair = pair("q1", 5, &["c_a"]);
        let r = score_hits(&pair, &hits, &expected);
        assert!(r.recall_at_k);
        assert!(!r.recall_at_1);
        assert!((r.reciprocal_rank - 0.5).abs() < 1e-6);
    }

    #[test]
    fn score_hits_no_match_yields_zero() {
        let hits = vec![chunk("c_x", ""), chunk("c_y", "")];
        let expected = ["c_a".to_string()].into_iter().collect();
        let pair = pair("q1", 5, &["c_a"]);
        let r = score_hits(&pair, &hits, &expected);
        assert!(!r.recall_at_k);
        assert!(!r.recall_at_1);
        assert_eq!(r.reciprocal_rank, 0.0);
    }

    #[test]
    fn score_hits_first_expected_drives_reciprocal_rank() {
        // Two expected chunks at ranks 2 and 4 — RR is 1/2.
        let hits = vec![
            chunk("c_x", ""),
            chunk("c_a", ""),
            chunk("c_y", ""),
            chunk("c_b", ""),
            chunk("c_z", ""),
        ];
        let expected = ["c_a".to_string(), "c_b".to_string()].into_iter().collect();
        let pair = pair("q1", 5, &["c_a", "c_b"]);
        let r = score_hits(&pair, &hits, &expected);
        assert!(r.recall_at_k);
        assert!(!r.recall_at_1);
        assert!((r.reciprocal_rank - 0.5).abs() < 1e-6);
    }

    // ───── AggregatedMetrics ─────

    #[test]
    fn aggregated_metrics_empty_is_zero() {
        let m = AggregatedMetrics::from_results(&[]);
        assert_eq!(m.n, 0);
        assert_eq!(m.recall_at_k, 0.0);
        assert_eq!(m.recall_at_1, 0.0);
        assert_eq!(m.mrr, 0.0);
    }

    #[test]
    fn aggregated_metrics_averages_recall_and_mrr() {
        let results = vec![
            PairResult {
                id: "q1".into(),
                kind: ChunkKind::Chat,
                recall_at_k: true,
                recall_at_1: true,
                reciprocal_rank: 1.0,
                k: 5,
            },
            PairResult {
                id: "q2".into(),
                kind: ChunkKind::Chat,
                recall_at_k: true,
                recall_at_1: false,
                reciprocal_rank: 0.5,
                k: 5,
            },
            PairResult {
                id: "q3".into(),
                kind: ChunkKind::Chat,
                recall_at_k: false,
                recall_at_1: false,
                reciprocal_rank: 0.0,
                k: 5,
            },
        ];
        let m = AggregatedMetrics::from_results(&results);
        assert_eq!(m.n, 3);
        // 2/3 of pairs had a recall@k hit.
        assert!((m.recall_at_k - 2.0 / 3.0).abs() < 1e-6);
        // 1/3 had a recall@1 hit.
        assert!((m.recall_at_1 - 1.0 / 3.0).abs() < 1e-6);
        // MRR = mean of (1.0, 0.5, 0.0) = 0.5.
        assert!((m.mrr - 0.5).abs() < 1e-6);
    }

    // ───── corpus parsing ─────

    #[test]
    fn parse_corpus_round_trips_a_minimal_pair() {
        let pair = QueryPair {
            id: "q_0000001".into(),
            kind: ChunkKind::Chat,
            query: "what did we discuss yesterday?".into(),
            profile: RetrievalProfile::AgentChatTurn,
            caller_id: "embedded".into(),
            k: 5,
            expected_chunk_ids: vec!["c_chat_001".into()],
            corpus_chunks: vec![CorpusChunk {
                id: "c_chat_001".into(),
                kind: ChunkKind::Chat,
                content: "yesterday we discussed the API design".into(),
                caller_id: "embedded".into(),
                session_id: Some("s1".into()),
                project_root: None,
                importance: None,
            }],
        };
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("corpus.jsonl");
        let line = serde_json::to_string(&pair).unwrap();
        std::fs::write(&p, format!("# header comment\n\n{line}\n")).unwrap();
        let parsed = parse_corpus(&p).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "q_0000001");
        assert_eq!(parsed[0].expected_chunk_ids, vec!["c_chat_001".to_string()]);
    }

    #[test]
    fn parse_corpus_rejects_malformed_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.jsonl");
        std::fs::write(&p, "not json at all\n").unwrap();
        let err = parse_corpus(&p).unwrap_err();
        assert!(format!("{err:?}").contains("parse line 1"));
    }

    // ───── end-to-end eval against a seeded provider ─────

    /// Smoke test: a tiny pair runs end-to-end against a fresh
    /// in-memory provider and yields a parseable report. Uses
    /// lexical fallback (the corpus chunk's content contains the query
    /// substring) so it doesn't depend on real embeddings.
    #[tokio::test]
    async fn run_memory_eval_against_small_corpus_produces_report() {
        let pair_a = QueryPair {
            id: "q_a".into(),
            kind: ChunkKind::Chat,
            query: "weather".into(),
            profile: RetrievalProfile::AgentChatTurn,
            caller_id: "embedded".into(),
            k: 5,
            expected_chunk_ids: vec!["c_a".into()],
            corpus_chunks: vec![
                CorpusChunk {
                    id: "c_a".into(),
                    kind: ChunkKind::Chat,
                    content: "the weather is nice today".into(),
                    caller_id: "embedded".into(),
                    session_id: None,
                    project_root: None,
                    importance: None,
                },
                CorpusChunk {
                    id: "c_b".into(),
                    kind: ChunkKind::Chat,
                    content: "we discussed the api design".into(),
                    caller_id: "embedded".into(),
                    session_id: None,
                    project_root: None,
                    importance: None,
                },
            ],
        };
        let report = run_memory_eval(vec![pair_a], None, "synthetic".into())
            .await
            .unwrap();
        assert_eq!(report.pairs.len(), 1);
        assert_eq!(report.overall.n, 1);
        // The corpus is small + content matches the query verbatim, so we
        // expect at least a recall@k hit (lexical signal alone).
        assert!(
            report.overall.recall_at_k >= 0.5,
            "expected at least some recall@k signal, got {}",
            report.overall.recall_at_k
        );
        assert!(report.by_kind.contains_key("chat"));
    }

    // ───── render_report ─────

    #[test]
    fn render_report_includes_targets_and_kinds() {
        let r = MemoryEvalReport {
            pairs: vec![],
            overall: AggregatedMetrics {
                n: 1,
                recall_at_k: 0.5,
                recall_at_1: 0.0,
                mrr: 0.25,
            },
            by_kind: {
                let mut m = BTreeMap::new();
                m.insert(
                    "chat".into(),
                    AggregatedMetrics {
                        n: 1,
                        recall_at_k: 0.5,
                        recall_at_1: 0.0,
                        mrr: 0.25,
                    },
                );
                m
            },
            corpus: "x.jsonl".into(),
            profile_override: Some("agent_chat_turn".into()),
        };
        let out = render_report(&r);
        assert!(out.contains("OVERALL"));
        assert!(out.contains("chat"));
        assert!(out.contains("targets"));
        assert!(out.contains("agent_chat_turn"));
    }
}
