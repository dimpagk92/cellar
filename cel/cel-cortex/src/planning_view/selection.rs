//! Deterministic selectors that narrow rich Cortex state to a budget.
//!
//! Picks the memories, knowledge, recent events, anomalies / blockers, and
//! adapter facts most relevant to the goal (keyword overlap x recency decay,
//! top-N within each per-category budget — no LLM) and returns `*Selection`
//! structs carrying the kept refs plus how many were omitted.

use super::elements::{extract_keywords, extract_quoted_phrases};
use super::util::{short_content_preview, unix_to_iso};
use super::*;

/// Result of memory selection — kept memories plus omitted count for the
/// `omitted_counts` field on the view.
#[derive(Debug, Default)]
pub(crate) struct MemorySelection {
    pub(crate) kept: Vec<MemoryRef>,
    pub(crate) omitted: u32,
    /// Set when the store opened cleanly but the workflow is empty —
    /// distinct from a store-open failure (which is logged at WARN).
    pub(crate) workflow_empty: bool,
}

// ─── Knowledge selection (Tier A1, deterministic) ──────────────────────────

/// Result of knowledge hydration — kept facts plus omitted count for the
/// `omitted_counts.knowledge` field on the view.
#[derive(Debug, Default)]
pub(crate) struct KnowledgeSelection {
    pub(crate) kept: Vec<KnowledgeRef>,
    pub(crate) omitted: u32,
}

// ─── Recent events selection (Tier A2, deterministic) ─────────────────────

/// Result of recent-events hydration — kept events plus omitted count
/// for the `omitted_counts.recent_events` field on the view.
#[derive(Debug, Default)]
pub(crate) struct RecentEventsSelection {
    pub(crate) kept: Vec<EventRef>,
    pub(crate) omitted: u32,
}

// ─── Adapter fact selection (closing-gap fill) ────────────────────────────

/// Result of adapter-fact hydration — kept facts plus omitted count for
/// `omitted_counts.adapter_facts`.
#[derive(Debug, Default)]
pub(crate) struct AdapterFactSelection {
    pub(crate) kept: Vec<cel_contracts::AdapterFactRef>,
    pub(crate) omitted: u32,
}

/// Cap adapter facts to the caller-provided budget. Adapters already decide
/// relevance for the current goal + perception, so CEL preserves adapter
/// order and truncates only to protect the shared PlanningView budget.
pub(crate) fn select_adapter_facts(
    adapter_facts: &[cel_contracts::AdapterFactRef],
    max_adapter_facts: u32,
) -> AdapterFactSelection {
    let max = max_adapter_facts as usize;
    let total = adapter_facts.len() as u32;
    if max == 0 {
        return AdapterFactSelection {
            kept: Vec::new(),
            omitted: total,
        };
    }
    let kept: Vec<cel_contracts::AdapterFactRef> =
        adapter_facts.iter().take(max).cloned().collect();
    let omitted = total.saturating_sub(kept.len() as u32);
    AdapterFactSelection { kept, omitted }
}

/// Tier A2: hydrate `PlanningView.recent_events` from cortex
/// `observations`. Observations are pre-prioritised (high → medium →
/// low) and recency-ordered within priority by the underlying store.
/// We just take the top `max_recent_events`.
///
/// No goal-keyword scoring on this path — observations are already
/// curated summaries (the cortex's compressed run-history facts), and
/// the LLM is good at filtering relevant ones from a small list.
/// Adding a keyword score here would over-filter and miss general
/// patterns the planner benefits from seeing (e.g. "this app crashes
/// on Mondays" is relevant even when the goal isn't explicitly about
/// crashes).
///
/// Failure logs at WARN and returns empty (privacy- + production-safe;
/// never blocks the view).
pub(crate) fn select_recent_events(
    store: &dyn cel_store::RecentEventStore,
    workflow_id: &str,
    max_recent_events: u32,
) -> RecentEventsSelection {
    let max = max_recent_events as usize;
    if max == 0 {
        return RecentEventsSelection::default();
    }
    // Overscan slightly so the priority/recency ordering inside
    // `get_observations` has room to bubble the right ones to the top
    // when the active set is larger than `max`. The store does the
    // ORDER BY; we just take the top-N. Capping at 4× max is generous
    // for typical workflow observation counts.
    let overscan = (max * 4).min(80);
    let candidates = match store.recent_events_for_workflow(workflow_id, overscan) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                workflow_id,
                error = %e,
                "Tier A2 planning_view: recent_events_for_workflow failed; \
                 skipping recent_events hydration",
            );
            return RecentEventsSelection::default();
        }
    };
    let total = candidates.len() as u32;
    let kept: Vec<EventRef> = candidates
        .into_iter()
        .take(max)
        .map(observation_to_event_ref)
        .collect();
    let omitted = total.saturating_sub(kept.len() as u32);
    RecentEventsSelection { kept, omitted }
}

// ─── Anomalies + Blockers selection (Tier A3) ─────────────────────────────

/// Tier A3: surface cortex anomaly queue + freshness assessment into
/// the planner-facing `anomalies` and `blockers` fields.
///
/// Mapping rules:
/// - **Every** anomaly produces an `AnomalyRef` (so the planner sees
///   the full signal).
/// - The **blocking subset** (`Dialog`, `AuthPrompt`) ALSO produces a
///   `Blocker` — these prevent goal pursuit until resolved. `Error` and
///   `AppSwitch` are surfaced as anomalies only (informational; the
///   planner can adapt without explicit blocker treatment).
/// - `FreshnessState::HardStale` produces a `Blocker` (perception
///   isn't trustworthy → don't act on it). `SoftStale` produces an
///   `AnomalyRef` (visible to planner but not blocking). `Fresh`
///   contributes nothing.
///
/// Not budgeted — anomalies and blockers are first-class signals per
/// the `OmittedCounts` docstring; nothing is dropped.
pub(crate) fn select_anomalies_and_blockers(
    cortex_anomalies: Option<&[Anomaly]>,
    cortex_freshness: Option<&FreshnessAssessment>,
) -> (Vec<AnomalyRef>, Vec<Blocker>) {
    let mut anomalies = Vec::new();
    let mut blockers = Vec::new();

    if let Some(items) = cortex_anomalies {
        for a in items {
            let kind = anomaly_type_str(&a.anomaly_type).to_string();
            let description = a.title.clone().unwrap_or_else(|| a.description.clone());
            anomalies.push(AnomalyRef {
                kind: kind.clone(),
                description: description.clone(),
            });
            // Blocking subset: Dialog (modal that traps focus) and
            // AuthPrompt (credential gate). Error / AppSwitch are
            // informational anomalies, not blockers per se.
            let blocker_kind = match a.anomaly_type {
                AnomalyType::Dialog => Some("modal_dialog"),
                AnomalyType::AuthPrompt => Some("auth_required"),
                AnomalyType::Error | AnomalyType::AppSwitch => None,
            };
            if let Some(bk) = blocker_kind {
                blockers.push(Blocker {
                    kind: bk.into(),
                    description,
                    element_id: a.element_ids.first().cloned(),
                });
            }
        }
    }

    if let Some(f) = cortex_freshness {
        match f.state {
            FreshnessState::HardStale => blockers.push(Blocker {
                kind: "stale_perception".into(),
                description: format!(
                    "Cortex perception is hard-stale (age {}ms); \
                     re-perceive before acting on the current view",
                    f.age_ms
                ),
                element_id: None,
            }),
            FreshnessState::SoftStale => anomalies.push(AnomalyRef {
                kind: "perception_soft_stale".into(),
                description: format!(
                    "Cortex perception is soft-stale (age {}ms); \
                     ranking signal still trustworthy",
                    f.age_ms
                ),
            }),
            FreshnessState::Fresh => {}
        }
    }

    (anomalies, blockers)
}

fn anomaly_type_str(t: &AnomalyType) -> &'static str {
    match t {
        AnomalyType::Dialog => "dialog",
        AnomalyType::Error => "error",
        AnomalyType::AppSwitch => "app_switch",
        AnomalyType::AuthPrompt => "auth_prompt",
    }
}

/// Map a stored `Observation` into the view-side `EventRef` shape.
/// Synthesizes a stable composite id (`obs:<row_id>`) so the planner
/// can reference the same event across turns. Priority becomes part
/// of the kind so the planner can give "high"-priority events more
/// weight even though the EventRef shape is otherwise opaque.
fn observation_to_event_ref(obs: cel_store::Observation) -> EventRef {
    let priority_str = match obs.priority {
        cel_store::ObservationPriority::High => "high",
        cel_store::ObservationPriority::Medium => "medium",
        cel_store::ObservationPriority::Low => "low",
    };
    let at = obs
        .observed_at
        .clone()
        .or_else(|| Some(obs.created_at.clone()));
    EventRef {
        id: format!("obs:{}", obs.id),
        kind: format!("observation:{priority_str}"),
        summary: obs.content,
        at,
    }
}

/// Tier A1: select goal-relevant facts from `knowledge_fts` via FTS5 +
/// bm25 ranking, scoped by workflow.
///
/// Algorithm (mirrors WK1 `select_memories` but without the in-Rust
/// scorer — bm25 is the ranking signal, no decay/quoted-phrase boost):
///
/// 1. Build an FTS5 MATCH expression from `extract_keywords(goal)` via
///    `cel_store::safe_fts5_query_from_keywords` (same helper WK1 uses).
/// 2. Call `KnowledgeStore::search_knowledge_for_workflow` — bm25-ranked,
///    capped at `max_knowledge × 4` to give a reasonable window before
///    truncating to budget. (Knowledge facts are denser than memories;
///    a tighter overscan suffices.)
/// 3. Take top `max_knowledge`, hydrate to `KnowledgeRef`. Tags column
///    on `knowledge_scoped` isn't surfaced by `ScoredKnowledge` today —
///    we leave `KnowledgeRef.tags` empty until enrichment lands (Tier
///    A4); `serde(skip_serializing_if = "Vec::is_empty")` keeps the
///    over-the-wire shape clean.
///
/// Failure modes: empty goal keywords → empty result (no FTS5 query to
/// run); store-side error → log + empty result. Same privacy-safe
/// pattern as WK1.
pub(crate) fn select_knowledge(
    store: &dyn cel_store::KnowledgeStore,
    workflow_scope: Option<&str>,
    goal: &str,
    max_knowledge: u32,
) -> KnowledgeSelection {
    let max = max_knowledge as usize;
    if max == 0 {
        return KnowledgeSelection::default();
    }
    let keywords = extract_keywords(goal);
    let fts_query = match cel_store::safe_fts5_query_from_keywords(&keywords) {
        Some(q) => q,
        None => return KnowledgeSelection::default(),
    };

    // Overscan to give bm25 room to rank — 4× budget feels right; capped
    // to a sensible max so giant budgets don't pull arbitrarily-large
    // result sets (knowledge tables can grow into the thousands).
    let overscan = (max * 4).min(80);
    let candidates = match store.search_knowledge_for_workflow(&fts_query, workflow_scope, overscan)
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Tier A1 planning_view: search_knowledge_for_workflow failed; \
                 skipping knowledge hydration",
            );
            return KnowledgeSelection::default();
        }
    };

    let total = candidates.len() as u32;
    let kept: Vec<KnowledgeRef> = candidates
        .into_iter()
        .take(max)
        .map(|sk| KnowledgeRef {
            id: sk.id,
            source: sk.source,
            content: sk.content,
            tags: vec![], // populated by enrichment in Tier A4
        })
        .collect();
    let omitted = total.saturating_sub(kept.len() as u32);
    KnowledgeSelection { kept, omitted }
}

/// Select goal-relevant memories from the cortex memory store.
///
/// Algorithm (post-WK1):
/// 1. Build candidate window:
///    - If goal has extractable keywords, run an FTS5-ranked search via
///      `CortexMemoryStore::search_for_workflow_ranked` — bm25-ordered,
///      capped at 200. This is the relevance pre-filter.
///    - Otherwise (or if FTS5 returns 0 / errors), fall back to the
///      most-recent 200 via `list_for_workflow`. This preserves the PR3
///      "give me something" behaviour for goals without parseable
///      keywords (e.g. "do it") and also lets the Rust scorer prove the
///      workflow is empty when both paths return 0.
/// 2. Score each candidate in Rust: `score = base × decay`, where base
///    reflects keyword + quoted-phrase + kind-bias. Decay uses
///    `last_accessed_at` so touched memories ride longer.
/// 3. Sort by score descending; take up to `max_memories` whose score > 0.
/// 4. Hydrate to `MemoryRef`s, record the omitted count.
///
/// The FTS5 pre-filter (WK1) doesn't replace the Rust scorer — it
/// narrows the candidate pool from "200 most recent" to "200 most
/// keyword-relevant", then the scorer applies nuanced quoted-phrase
/// boosting, kind-bias, and decay weighting on top. FTS5 cuts noise;
/// the Rust scorer ranks the survivors.
///
/// WK4: takes `&dyn CortexMemoryStore` instead of a path. Caller (the
/// canonical runner) opens the store once per run and shares it across
/// every turn — replaces N+1 SQLite opens per N-turn run with 1.
pub(crate) fn select_memories(
    store: &dyn cel_store::CortexMemoryStore,
    workflow_id: &str,
    goal: &str,
    max_memories: u32,
    goal_embedding: Option<&[u8]>,
) -> MemorySelection {
    let max = max_memories as usize;
    if max == 0 {
        return MemorySelection::default();
    }

    let keywords = extract_keywords(goal);
    let quoted = extract_quoted_phrases(goal);

    // WK2: decode the pre-computed goal embedding once. Selector falls
    // back to pure WK1 FTS5+decay when:
    //   - no goal_embedding was passed (no embedder wired), OR
    //   - the bytes don't decode (corruption / empty / misaligned).
    let goal_vec = goal_embedding.and_then(cel_llm::EmbeddingVector::from_bytes);

    // WK1: FTS5 pre-filter when the goal yields usable keywords.
    let fts_candidates = cel_store::safe_fts5_query_from_keywords(&keywords).and_then(|q| {
        match store.search_for_workflow_ranked(workflow_id, &q, 200) {
            Ok(v) if !v.is_empty() => Some(v),
            Ok(_) => None, // no FTS5 match — fall through to recency
            Err(e) => {
                tracing::warn!(
                    workflow_id,
                    error = %e,
                    "WK1 planning_view: FTS5 search failed; falling back to most-recent",
                );
                None
            }
        }
    });

    let candidates = match fts_candidates {
        Some(v) => v,
        None => match store.list_for_workflow(workflow_id, None, 200) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    workflow_id,
                    error = %e,
                    "WK4 planning_view: list_for_workflow failed; skipping memory hydration",
                );
                return MemorySelection::default();
            }
        },
    };

    if candidates.is_empty() {
        return MemorySelection {
            workflow_empty: true,
            ..MemorySelection::default()
        };
    }

    let total = candidates.len() as u32;
    let now = cel_store::cortex_memory::now_unix_secs();

    let mut scored: Vec<(f64, cel_store::cortex_memory::CortexMemory)> = candidates
        .into_iter()
        .map(|m| {
            (
                score_memory(&m, &keywords, &quoted, now, goal_vec.as_ref()),
                m,
            )
        })
        .collect();

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let kept: Vec<MemoryRef> = scored
        .iter()
        .filter(|(s, _)| *s > 0.0)
        .take(max)
        .map(|(_, m)| compress_memory(m))
        .collect();

    let omitted = total.saturating_sub(kept.len() as u32);
    MemorySelection {
        kept,
        omitted,
        workflow_empty: false,
    }
}

/// Score a memory's relevance to the goal at the given moment.
///
/// `score = base × decay × (1 + cosine_boost)`, where:
///   - `base` reflects keyword + quoted-phrase + kind-bias overlap
///     (WK1 + PR3 deterministic scorer)
///   - `decay = exp(-ln(2) × age_days / 90)` against `last_accessed_at`
///   - `cosine_boost` is `0.5 × max(0, cosine(goal, memory))` when WK2
///     is wired (`goal_vec` provided + memory has a same-dimension
///     stored embedding); `0` otherwise.
///
/// The cosine multiplier is capped (≤ 1.5x amplification) so embeddings
/// re-rank the FTS5+decay survivors rather than overwhelm them.
/// Negative cosine doesn't penalise — keyword-matched memories that
/// happen to embed-distantly stay in the running.
///
/// Memories with no keyword overlap still score 0 (no `base`, so the
/// multiplier doesn't lift them above the threshold). WK2 enriches
/// ranking *within* the keyword-matched set; it doesn't expand it.
fn score_memory(
    memory: &cel_store::cortex_memory::CortexMemory,
    keywords: &[String],
    quoted: &[String],
    now_secs: i64,
    goal_vec: Option<&cel_llm::EmbeddingVector>,
) -> f64 {
    let summary = memory.summary.as_deref().unwrap_or("").to_lowercase();
    let content_str = serde_json::to_string(&memory.content)
        .unwrap_or_default()
        .to_lowercase();
    let tags_blob = memory.tags.join(" ").to_lowercase();
    let haystack = format!("{summary} {content_str} {tags_blob}");

    let mut base: f64 = 0.0;

    for kw in keywords {
        if haystack.contains(kw.as_str()) {
            base += 2.0;
            // Bonus if keyword hits the curated summary specifically —
            // summaries are caller-written one-liners; matches there are
            // higher-signal than incidental hits inside JSON content.
            if summary.contains(kw.as_str()) {
                base += 1.0;
            }
        }
    }

    for phrase in quoted {
        if haystack.contains(phrase.as_str()) {
            base += 30.0;
        }
    }

    // Kind-based prior: failures are mildly more useful than outcomes
    // when the goal looks similar to past failed attempts. Small bias.
    if base > 0.0
        && matches!(
            memory.kind,
            cel_store::cortex_memory::MemoryKind::Failure
                | cel_store::cortex_memory::MemoryKind::Preference
        )
    {
        base *= 1.15;
    }

    if base == 0.0 {
        return 0.0;
    }

    let decay = cel_store::cortex_memory::decay_score(memory.last_accessed_at, now_secs);

    // WK2: cosine boost. Only fires when both the goal embedding and
    // the memory's stored embedding decode to the same dimension; any
    // mismatch (None, byte-decode failure, dimension drift after a
    // model swap) safely skips the boost without altering base * decay.
    let cosine_boost = match (goal_vec, memory.embedding.as_deref()) {
        (Some(goal), Some(mem_bytes)) => cel_llm::EmbeddingVector::from_bytes(mem_bytes)
            .filter(|m| m.dimensions() == goal.dimensions())
            .map(|m| {
                let raw = cel_llm::cosine_similarity(goal.as_slice(), m.as_slice());
                // Map cosine [-1, 1] → boost [0, 0.5] (clamp negatives
                // to 0 so embed-distant keyword matches aren't
                // penalised; cap positives to keep WK1 in the driver
                // seat). 1.5x max amplification on top of base*decay.
                (raw.max(0.0) as f64) * 0.5
            })
            .unwrap_or(0.0),
        _ => 0.0,
    };

    base * decay * (1.0 + cosine_boost)
}

fn compress_memory(memory: &cel_store::cortex_memory::CortexMemory) -> MemoryRef {
    let summary = memory
        .summary
        .clone()
        .unwrap_or_else(|| short_content_preview(&memory.content));
    MemoryRef {
        id: memory.id,
        kind: memory.kind.as_str().to_string(),
        summary,
        content: memory.content.clone(),
        created_at: Some(unix_to_iso(memory.created_at)),
    }
}
