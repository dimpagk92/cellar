//! Planning-view builder — projects rich Cortex state into a budgeted view.
//!
//! Implements the **store broadly, select narrowly** principle: the
//! `MentalModel` (and underlying `ScreenContext`) can be 60K+ tokens of raw
//! perception. The planner doesn't need that. This builder produces a
//! `PlanningView` with the goal-relevant slice — current screen + selected
//! elements + capabilities + run progress + (PR3) memory-aware hydration —
//! and tracks what was dropped.
//!
//! Element selection is deterministic (goal-keyword + quoted-phrase +
//! actionable-type heuristics).
//!
//! Memory selection (PR3) is also deterministic: the builder pulls
//! workflow-scoped memories from `cortex_memories`, scores each by keyword
//! overlap with the goal × exponential decay against `last_accessed_at`,
//! and takes the top-N within `budget.max_memories`. No LLM call. PR4 may
//! add an LLM-based selector on top, but the deterministic path is the
//! fallback (and the default).

use std::collections::HashSet;

use cel_context::{ContextElement, ScreenContext};
use cel_contracts::{
    AnomalyRef, Blocker, CapabilityRef, EventRef, KnowledgeRef, MemoryRef, OmittedCounts,
    PlanningBudget, PlanningElement, PlanningElementState, PlanningScreen, PlanningView,
    RunProgress, RuntimeCaps,
};

use crate::model::{Anomaly, AnomalyType, FreshnessAssessment, FreshnessState};

/// Inputs the builder needs from the cortex / runner side.
///
/// Kept as a borrowed-data struct so the runner can build it once per
/// turn from its own state without copying.
pub struct PlanningViewInputs<'a> {
    pub goal: &'a str,
    pub budget: &'a PlanningBudget,
    pub perception: &'a ScreenContext,
    pub caps: &'a RuntimeCaps,
    /// WK4 (was: `memory_db_path: Option<&'a str>`): the cortex-memory
    /// backing store, opened **once per run** by the caller and shared
    /// across every turn. Replaces the path-based API that re-opened
    /// SQLite on every planner turn. `None` (default) preserves PR1a
    /// "no memory" behaviour for callers that haven't opted in.
    ///
    /// Production callers pass a `&std::sync::Mutex<cel_store::CelStore>`
    /// (cheap to construct once, satisfies the trait's `Send + Sync`
    /// bound which is required for use across async-fn awaits). Tests
    /// pass the same.
    pub memory_store: Option<&'a dyn cel_store::CortexMemoryStore>,
    /// PR3: required alongside `memory_store`. Memory selection is
    /// strictly workflow-scoped — the same workflow_id used for writes
    /// (via `RunLimits.workflow_id_for_memory` or `cel_perceive start
    /// { enable_memory, workflow_id }`).
    pub workflow_id: Option<&'a str>,
    /// Tier A1 (post-WK reframe): when set, hydrate `PlanningView.knowledge`
    /// via FTS5-ranked search over `knowledge_fts`. Workflow scope comes
    /// from `workflow_id` — `Some(wf)` filters facts to (NULL OR matches
    /// wf); `None` returns global facts only. The same `Mutex<CelStore>`
    /// the runner opens for memory satisfies this trait too — callers
    /// typically pass the same handle for both `memory_store` and
    /// `knowledge_store`.
    pub knowledge_store: Option<&'a dyn cel_store::KnowledgeStore>,
    /// WK2 (un-deferred): pre-computed embedding of the goal text as
    /// raw little-endian f32 bytes (decode via
    /// `cel_llm::EmbeddingVector::from_bytes`). When present AND a
    /// candidate memory's stored `embedding` decodes to the same
    /// dimension, the selector adds a cosine-similarity boost on top
    /// of the FTS5+decay base score.
    ///
    /// Pre-computed by the runner once per run (the goal doesn't
    /// change within a run) so `build_planning_view` stays sync —
    /// embedding generation is async and cel-cortex can't await.
    /// `None` (the default) skips the cosine step entirely; selector
    /// behaviour falls back to pure WK1 FTS5+decay.
    pub goal_embedding: Option<&'a [u8]>,
    /// Tier A2 (post-WK reframe): when set, hydrate
    /// `PlanningView.recent_events` from cortex `observations`. Same
    /// `Mutex<CelStore>` handle the runner opens for memory satisfies
    /// this trait too. Workflow scope from `workflow_id`; `None`
    /// silently skips the field (preserves PR1a perception-only
    /// behaviour for callers that haven't opted in).
    pub recent_events_store: Option<&'a dyn cel_store::RecentEventStore>,
    /// Tier A3: cortex anomaly queue snapshot. Each anomaly produces
    /// an `AnomalyRef` in `view.anomalies`; the **blocking subset**
    /// (`Dialog`, `AuthPrompt`) ALSO produces a `Blocker` in
    /// `view.blockers` so the planner can short-circuit goal pursuit
    /// until the blocker is resolved. `None` preserves PR1 perception-
    /// only behaviour (both fields stay empty).
    pub cortex_anomalies: Option<&'a [Anomaly]>,
    /// Tier A3: cortex freshness assessment. `HardStale` produces a
    /// `Blocker` (perception isn't trustworthy); `SoftStale` produces
    /// an `AnomalyRef` (visible to planner but not blocking); `Fresh`
    /// contributes nothing. `None` preserves PR1 behaviour.
    pub cortex_freshness: Option<&'a FreshnessAssessment>,
}

/// Build a budgeted `PlanningView` from current cortex perception + caps,
/// optionally hydrating workflow-scoped memories.
///
/// Selection is fully deterministic. No LLM calls. Memory hydration only
/// fires when both `memory_store` and `workflow_id` are set — privacy-
/// preserving default. Store-side failures (read errors, etc.) are logged
/// at WARN and the builder returns an empty memory list rather than
/// failing the view.
pub fn build_planning_view(inputs: &PlanningViewInputs<'_>) -> PlanningView {
    let total_elements = inputs.perception.elements.len() as u32;
    let elements = select_elements(inputs.perception, inputs.goal, inputs.budget.max_elements);
    let kept_elements = elements.len() as u32;
    let omitted_elements = total_elements.saturating_sub(kept_elements);

    let memory_selection = match (inputs.memory_store, inputs.workflow_id) {
        (Some(store), Some(wf)) => select_memories(
            store,
            wf,
            inputs.goal,
            inputs.budget.max_memories,
            inputs.goal_embedding,
        ),
        _ => MemorySelection::default(),
    };

    // Tier A1: knowledge hydration. Independent of memory selection —
    // a caller can opt into one, the other, both, or neither. Knowledge
    // is workflow-scoped via the underlying `knowledge_scoped.workflow_scope`
    // column (NULL = global facts visible to every workflow). When
    // `workflow_id` is None we still query for global facts only.
    let knowledge_selection = match inputs.knowledge_store {
        Some(store) => select_knowledge(
            store,
            inputs.workflow_id,
            inputs.goal,
            inputs.budget.max_knowledge,
        ),
        None => KnowledgeSelection::default(),
    };

    // Tier A2: recent events hydration. Workflow-scoped — `None`
    // workflow_id means we can't pick observations (the underlying
    // table is workflow_name-keyed). Silent in that case.
    let recent_events_selection = match (inputs.recent_events_store, inputs.workflow_id) {
        (Some(store), Some(wf)) => select_recent_events(store, wf, inputs.budget.max_recent_events),
        _ => RecentEventsSelection::default(),
    };

    // Tier A3: surface cortex anomalies + freshness as AnomalyRefs and
    // (subset) Blockers. NOT budgeted — anomalies and blockers are
    // first-class signals the planner must see all of, per
    // OmittedCounts's docstring ("first-class blocker that should not
    // be lost to compression").
    let (anomalies, blockers) =
        select_anomalies_and_blockers(inputs.cortex_anomalies, inputs.cortex_freshness);

    let selection_rationale = build_rationale(
        elements.len(),
        &memory_selection,
        &knowledge_selection,
        &recent_events_selection,
        anomalies.len(),
        blockers.len(),
        omitted_elements,
    );

    PlanningView {
        goal: inputs.goal.to_string(),
        budget: inputs.budget.clone(),
        screen: PlanningScreen {
            active_app: inputs.perception.app.clone(),
            window: inputs.perception.window.clone(),
            summary: None,
            url: inputs.caps.cdp_url.clone(),
        },
        elements,
        adapter_facts: vec![],
        capabilities: caps_to_capabilities(inputs.caps),
        memories: memory_selection.kept,
        knowledge: knowledge_selection.kept,
        recent_events: recent_events_selection.kept,
        blockers,
        anomalies,
        evidence: vec![],
        selection_rationale,
        omitted_counts: OmittedCounts {
            elements: omitted_elements,
            memories: memory_selection.omitted,
            knowledge: knowledge_selection.omitted,
            recent_events: recent_events_selection.omitted,
            ..Default::default()
        },
        run_progress: RunProgress {
            steps_used: inputs.caps.steps_used,
            max_steps: inputs.caps.max_steps,
        },
    }
}

// ─── Memory selection (PR3, deterministic) ──────────────────────────────────

/// Result of memory selection — kept memories plus omitted count for the
/// `omitted_counts` field on the view.
#[derive(Debug, Default)]
struct MemorySelection {
    kept: Vec<MemoryRef>,
    omitted: u32,
    /// Set when the store opened cleanly but the workflow is empty —
    /// distinct from a store-open failure (which is logged at WARN).
    workflow_empty: bool,
}

// ─── Knowledge selection (Tier A1, deterministic) ──────────────────────────

/// Result of knowledge hydration — kept facts plus omitted count for the
/// `omitted_counts.knowledge` field on the view.
#[derive(Debug, Default)]
struct KnowledgeSelection {
    kept: Vec<KnowledgeRef>,
    omitted: u32,
}

// ─── Recent events selection (Tier A2, deterministic) ─────────────────────

/// Result of recent-events hydration — kept events plus omitted count
/// for the `omitted_counts.recent_events` field on the view.
#[derive(Debug, Default)]
struct RecentEventsSelection {
    kept: Vec<EventRef>,
    omitted: u32,
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
fn select_recent_events(
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
fn select_anomalies_and_blockers(
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
    let at = if obs.observed_at.is_empty() {
        Some(obs.created_at.clone())
    } else {
        Some(obs.observed_at.clone())
    };
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
fn select_knowledge(
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
fn select_memories(
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

fn short_content_preview(content: &serde_json::Value) -> String {
    let s = serde_json::to_string(content).unwrap_or_default();
    if s.len() <= 80 {
        s
    } else {
        format!("{}…", &s[..80])
    }
}

fn unix_to_iso(secs: i64) -> String {
    // Best-effort ISO-8601 without pulling chrono. Same approach as the
    // canonical-runner outcome auto-write.
    let days = secs / 86_400;
    let remaining = secs % 86_400;
    let hours = remaining / 3600;
    let minutes = (remaining % 3600) / 60;
    let seconds = remaining % 60;
    let (year, month, day) = unix_days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

fn unix_days_to_ymd(days_from_epoch: i64) -> (i64, u32, u32) {
    let mut days = days_from_epoch;
    let mut year: i64 = 1970;
    loop {
        let dy = if is_leap(year) { 366 } else { 365 };
        if days >= dy {
            days -= dy;
            year += 1;
        } else {
            break;
        }
    }
    let months = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month: u32 = 1;
    for &dm in &months {
        let dm_actual = if month == 2 && is_leap(year) { 29 } else { dm };
        if days >= dm_actual {
            days -= dm_actual;
            month += 1;
        } else {
            break;
        }
    }
    (year, month, (days + 1) as u32)
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

fn build_rationale(
    kept_elements: usize,
    memory_selection: &MemorySelection,
    knowledge_selection: &KnowledgeSelection,
    recent_events_selection: &RecentEventsSelection,
    anomaly_count: usize,
    blocker_count: usize,
    omitted_elements: u32,
) -> Option<String> {
    let memory_silent = memory_selection.kept.is_empty()
        && memory_selection.omitted == 0
        && !memory_selection.workflow_empty;
    let knowledge_silent = knowledge_selection.kept.is_empty() && knowledge_selection.omitted == 0;
    let recent_events_silent =
        recent_events_selection.kept.is_empty() && recent_events_selection.omitted == 0;
    let a3_silent = anomaly_count == 0 && blocker_count == 0;
    if memory_silent && knowledge_silent && recent_events_silent && a3_silent {
        // Pure perception-only build (PR1 behaviour). Don't synthesise a
        // misleading rationale — leave the field absent so callers don't
        // think memory / knowledge / events / anomaly selection happened
        // when it didn't.
        return None;
    }
    let mut parts = Vec::with_capacity(5);
    parts.push(format!(
        "Selected {} element(s) from {} candidate(s).",
        kept_elements,
        kept_elements as u32 + omitted_elements
    ));
    // Memory line — preserve PR3 wording exactly so the existing tests
    // that pin on "No prior memories" / "Hydrated N workflow memories"
    // keep matching.
    if memory_selection.workflow_empty {
        parts.push("No prior memories for this workflow.".into());
    } else if !memory_silent {
        parts.push(format!(
            "Hydrated {} workflow memor{} (dropped {} below the goal-relevance + decay threshold).",
            memory_selection.kept.len(),
            if memory_selection.kept.len() == 1 {
                "y"
            } else {
                "ies"
            },
            memory_selection.omitted,
        ));
    }
    // Tier A1 knowledge line — only when knowledge selection actually
    // ran (knowledge_store was provided).
    if !knowledge_silent {
        parts.push(format!(
            "Hydrated {} knowledge fact{} (dropped {} below bm25 cut).",
            knowledge_selection.kept.len(),
            if knowledge_selection.kept.len() == 1 {
                ""
            } else {
                "s"
            },
            knowledge_selection.omitted,
        ));
    }
    // Tier A2 recent_events line.
    if !recent_events_silent {
        parts.push(format!(
            "Hydrated {} recent event{} (dropped {} below budget).",
            recent_events_selection.kept.len(),
            if recent_events_selection.kept.len() == 1 {
                ""
            } else {
                "s"
            },
            recent_events_selection.omitted,
        ));
    }
    // Tier A3 anomaly + blocker lines. Surfaced separately so the
    // planner can see at a glance how many of each are present
    // (blockers gate goal pursuit; anomalies are advisory).
    if !a3_silent {
        parts.push(format!(
            "Surfaced {} cortex anomal{} and {} blocker{}.",
            anomaly_count,
            if anomaly_count == 1 { "y" } else { "ies" },
            blocker_count,
            if blocker_count == 1 { "" } else { "s" },
        ));
    }
    Some(parts.join(" "))
}

// ─── Capabilities folding ────────────────────────────────────────────────────

fn caps_to_capabilities(caps: &RuntimeCaps) -> Vec<CapabilityRef> {
    let mut out = Vec::with_capacity(2);
    if caps.cdp_bound {
        out.push(CapabilityRef {
            id: "cdp_bound".into(),
            detail: caps.cdp_browser.clone(),
        });
    }
    if caps.native_input {
        out.push(CapabilityRef {
            id: "native_input".into(),
            detail: None,
        });
    }
    out
}

// ─── Element selection (deterministic) ───────────────────────────────────────

fn select_elements(context: &ScreenContext, goal: &str, max_elements: u32) -> Vec<PlanningElement> {
    let max = max_elements as usize;
    if max == 0 {
        return Vec::new();
    }

    let keywords = extract_keywords(goal);
    let quoted = extract_quoted_phrases(goal);
    let is_extract = is_extraction_goal(goal);

    let mut scored: Vec<(f64, &ContextElement)> = context
        .elements
        .iter()
        .filter(|el| el.state.visible)
        .map(|el| (score_element(el, &keywords, &quoted, is_extract), el))
        .collect();

    // Stable sort by score descending so deterministic ordering is preserved
    // when scores tie.
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut out: Vec<PlanningElement> = scored
        .iter()
        .filter(|(s, _)| *s > 0.0)
        .take(max)
        .map(|(_, el)| compress(el))
        .collect();

    // Always include `page-text` if present and we're not at budget.
    if out.len() < max && out.iter().all(|e| e.id != "page-text") {
        if let Some(pt) = context.elements.iter().find(|el| el.id == "page-text") {
            out.push(compress(pt));
        }
    }

    out
}

fn compress(el: &ContextElement) -> PlanningElement {
    PlanningElement {
        id: el.id.clone(),
        element_type: el.element_type.clone(),
        label: el.label.clone(),
        value: el.value.clone(),
        state: PlanningElementState {
            focused: el.state.focused,
            selected: el.state.selected,
            enabled: el.state.enabled,
            checked: el.state.checked.unwrap_or(false),
            expanded: el.state.expanded.unwrap_or(false),
        },
        clickable: !el.actions.is_empty()
            || matches!(
                el.element_type.as_str(),
                "button" | "link" | "a" | "checkbox" | "radio_button" | "menu_item"
            ),
        settable: matches!(
            el.element_type.as_str(),
            "input" | "textarea" | "combobox" | "select"
        ),
    }
}

// ─── Scoring (ported from cel-planner/distiller.rs) ──────────────────────────

const ACTIONABLE_TYPES: &[&str] = &[
    "button",
    "input",
    "select",
    "textarea",
    "a",
    "link",
    "checkbox",
    "radio_button",
    "combobox",
    "slider",
    "tab",
    "menu_item",
];

const GENERIC_ACTION_LABELS: &[&str] = &[
    "open",
    "close",
    "cancel",
    "ok",
    "more",
    "menu",
    "next",
    "back",
    "learn more",
    "details",
    "view",
    "edit",
    "delete",
    "remove",
    "select",
    "continue",
    "submit",
    "save",
    "apply",
    "retry",
    "dismiss",
];

const CHROME_HINT_KEYWORDS: &[&str] = &[
    "header",
    "nav",
    "navbar",
    "toolbar",
    "menu",
    "sidebar",
    "breadcrumb",
    "footer",
    "legal",
    "cookie",
    "consent",
    "account",
    "profile",
    "help",
    "support",
    "social",
    "share",
    "newsletter",
    "chat",
    "intercom",
];

const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "were", "be", "been", "and", "or", "but", "in", "on",
    "at", "to", "for", "of", "with", "from", "by", "as", "it", "do", "not", "this", "that", "my",
    "me", "i", "you", "we", "can", "will", "just", "any", "all", "each", "find", "search", "open",
    "read", "get", "show", "tell", "what", "how", "please",
];

fn is_actionable(element_type: &str) -> bool {
    ACTIONABLE_TYPES.contains(&element_type)
}

fn is_generic_label(label: &str) -> bool {
    GENERIC_ACTION_LABELS.contains(&label.to_lowercase().as_str())
}

fn extract_keywords(goal: &str) -> Vec<String> {
    let stop: HashSet<&str> = STOP_WORDS.iter().copied().collect();
    goal.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2 && !stop.contains(w))
        .map(|w| w.to_string())
        .collect()
}

fn extract_quoted_phrases(goal: &str) -> Vec<String> {
    let mut phrases = Vec::new();
    let mut in_quote = false;
    let mut current = String::new();
    let mut quote_char = '"';

    for ch in goal.chars() {
        if !in_quote && (ch == '"' || ch == '\'') {
            in_quote = true;
            quote_char = ch;
            current.clear();
        } else if in_quote && ch == quote_char {
            if current.len() > 2 {
                phrases.push(current.to_lowercase());
            }
            in_quote = false;
            current.clear();
        } else if in_quote {
            current.push(ch);
        }
    }
    phrases
}

fn is_extraction_goal(goal: &str) -> bool {
    let lower = goal.to_lowercase();
    lower.contains("extract")
        || lower.contains("read")
        || lower.contains("what is")
        || lower.contains("how many")
        || lower.contains("find")
        || lower.contains("list")
        || lower.contains("get the")
}

fn score_element(
    el: &ContextElement,
    keywords: &[String],
    quoted_phrases: &[String],
    is_extract: bool,
) -> f64 {
    let mut score: f64 = 0.0;
    let label = el.label.as_deref().unwrap_or("").to_lowercase();
    let value = el.value.as_deref().unwrap_or("").to_lowercase();
    let text = format!("{} {} {}", label, value, el.element_type);

    for kw in keywords {
        if text.contains(kw.as_str()) {
            score += 2.0;
        }
    }
    for phrase in quoted_phrases {
        if label == *phrase {
            score += 50.0;
        }
    }
    if label.len() > 4 {
        let goal_lower: String = keywords.join(" ");
        if goal_lower.contains(&label) && label.contains(' ') {
            score += 20.0;
        }
    }
    if is_extract {
        if matches!(el.element_type.as_str(), "text" | "heading" | "static_text") {
            score += 3.0;
        }
        if el.id == "page-text" {
            score += 50.0;
        }
    } else if el.id == "page-text" {
        score += 5.0;
    }
    if is_actionable(&el.element_type) {
        score += 1.0;
    }
    if el.state.visible && el.state.enabled {
        score += 0.5;
    }
    if !el.actions.is_empty() {
        score += 0.5;
    }
    let label_len = el.label.as_ref().map(|l| l.len()).unwrap_or(0);
    if label_len > 30 {
        score += 2.0;
    } else if label_len > 15 {
        score += 1.0;
    } else if label_len <= 5 && is_actionable(&el.element_type) {
        score -= 1.0;
    }
    if is_generic_label(&label) && is_actionable(&el.element_type) {
        score -= 1.5;
    }
    let props_hint = el
        .properties
        .get("css_selector")
        .map(|s| s.as_str())
        .unwrap_or("");
    for kw in CHROME_HINT_KEYWORDS {
        if props_hint.contains(kw) {
            score -= 4.0;
            break;
        }
    }

    score
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use cel_context::ElementState;

    fn make_el(id: &str, ty: &str, label: Option<&str>) -> ContextElement {
        ContextElement {
            id: id.into(),
            label: label.map(String::from),
            description: None,
            element_type: ty.into(),
            value: None,
            bounds: None,
            state: ElementState {
                visible: true,
                enabled: true,
                ..Default::default()
            },
            parent_id: None,
            actions: vec![],
            confidence: 1.0,
            source: cel_context::ContextSource::AccessibilityTree,
            content_role: cel_context::ContentRole::Interactive,
            properties: Default::default(),
        }
    }

    fn make_context(elements: Vec<ContextElement>) -> ScreenContext {
        ScreenContext {
            app: "Browser".into(),
            window: "Test".into(),
            elements,
            network_events: vec![],
            http_events: vec![],
            timestamp_ms: 0,
            screen_width: None,
            screen_height: None,
            clipboard: None,
            window_list: vec![],
            audio: None,
            power: None,
            running_apps: vec![],
            recent_files: vec![],
            transcripts: vec![],
        }
    }

    #[test]
    fn budget_caps_element_count_and_records_omitted() {
        let elements: Vec<ContextElement> = (0..200)
            .map(|i| make_el(&format!("e{i}"), "button", Some("Submit form")))
            .collect();
        let perception = make_context(elements);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget {
            max_elements: 30,
            ..Default::default()
        };
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit the form",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        assert_eq!(view.elements.len(), 30);
        assert_eq!(view.omitted_counts.elements, 170);
    }

    #[test]
    fn goal_relevant_elements_outrank_irrelevant_ones() {
        let mut elements = Vec::new();
        for i in 0..20 {
            elements.push(make_el(&format!("noise{i}"), "button", Some("Open menu")));
        }
        elements.push(make_el("submit-1", "button", Some("Submit Invoice")));
        elements.push(make_el("submit-2", "button", Some("Save Draft")));

        let perception = make_context(elements);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget {
            max_elements: 5,
            ..Default::default()
        };
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit the invoice",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });

        let ids: Vec<&str> = view.elements.iter().map(|e| e.id.as_str()).collect();
        assert!(
            ids.contains(&"submit-1"),
            "submit-related element should rank in top-5; got {ids:?}"
        );
    }

    #[test]
    fn caps_fold_into_capabilities_and_run_progress() {
        let perception = make_context(vec![]);
        let caps = RuntimeCaps {
            cdp_bound: true,
            cdp_browser: Some("Google Chrome".into()),
            cdp_url: Some("https://example.com".into()),
            native_input: true,
            steps_used: 13,
            max_steps: 80,
        };
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "anything",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });

        assert_eq!(view.run_progress.steps_used, 13);
        assert_eq!(view.run_progress.max_steps, 80);
        assert_eq!(view.run_progress.steps_remaining(), 67);

        assert!(view.capabilities.iter().any(|c| c.id == "cdp_bound"));
        assert!(view.capabilities.iter().any(|c| c.id == "native_input"));
        assert_eq!(view.screen.url.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn empty_perception_yields_empty_view() {
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "do nothing",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        assert_eq!(view.elements.len(), 0);
        assert_eq!(view.omitted_counts.elements, 0);
        assert!(view.capabilities.is_empty());
    }

    #[test]
    fn invisible_elements_are_filtered_before_scoring() {
        let mut visible = make_el("visible", "button", Some("Submit"));
        let mut hidden = make_el("hidden", "button", Some("Submit"));
        hidden.state.visible = false;
        visible.state.visible = true;

        let perception = make_context(vec![hidden, visible]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget {
            max_elements: 10,
            ..Default::default()
        };
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        assert!(view.elements.iter().all(|e| e.id != "hidden"));
    }

    // ─── PR3 + WK4: memory-aware hydration via CortexMemoryStore trait ──────

    /// Build an in-memory `CelStore` wrapped in `Mutex` so it satisfies the
    /// `CortexMemoryStore` trait's `Send + Sync` bound. Replaces the
    /// PR3-era temp-file pattern: faster (no disk IO), no cleanup needed,
    /// and exercises the same code path the canonical runner uses in
    /// production (open once, share `&Mutex<CelStore>`).
    fn fresh_store() -> std::sync::Mutex<cel_store::CelStore> {
        std::sync::Mutex::new(cel_store::CelStore::open_memory().expect("open in-memory CelStore"))
    }

    fn seed_memory(
        store: &std::sync::Mutex<cel_store::CelStore>,
        workflow: &str,
        kind: cel_store::cortex_memory::MemoryKind,
        summary: &str,
        content: serde_json::Value,
    ) -> i64 {
        store
            .lock()
            .expect("seed: store mutex poisoned")
            .insert_cortex_memory(&cel_store::cortex_memory::NewCortexMemory {
                workflow_id: workflow.into(),
                kind,
                content,
                summary: Some(summary.into()),
                tags: vec![],
                source_ref: None,
                embedding: None,
            })
            .expect("insert")
    }

    #[test]
    fn pr3_view_stays_empty_when_memory_inputs_missing() {
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "any",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        assert!(view.memories.is_empty());
        assert_eq!(view.omitted_counts.memories, 0);
        assert!(view.selection_rationale.is_none());
    }

    #[test]
    fn pr3_relevant_memory_outranks_irrelevant_one() {
        let store = fresh_store();
        seed_memory(
            &store,
            "test-pr3",
            cel_store::cortex_memory::MemoryKind::Outcome,
            "Submitted invoice via Concur successfully",
            serde_json::json!({"goal": "submit invoice"}),
        );
        seed_memory(
            &store,
            "test-pr3",
            cel_store::cortex_memory::MemoryKind::Outcome,
            "Read morning headlines from Hacker News",
            serde_json::json!({"goal": "read news"}),
        );

        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit invoice in Concur",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: Some(&store),
            knowledge_store: None,
            workflow_id: Some("test-pr3"),
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });

        assert!(
            !view.memories.is_empty(),
            "expected at least one hydrated memory; got 0"
        );
        // The submit-invoice memory must rank first.
        assert!(
            view.memories[0]
                .summary
                .to_lowercase()
                .contains("submitted invoice"),
            "expected submit-invoice memory first; got {:?}",
            view.memories[0].summary
        );
    }

    #[test]
    fn pr3_budget_caps_memory_count_and_records_omitted() {
        let store = fresh_store();
        // Seed 5 memories all referencing the goal keyword "form" so each
        // scores > 0.
        for i in 0..5 {
            seed_memory(
                &store,
                "wf",
                cel_store::cortex_memory::MemoryKind::Outcome,
                &format!("Submitted form attempt {i}"),
                serde_json::json!({"i": i}),
            );
        }

        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget {
            max_memories: 2,
            ..PlanningBudget::default()
        };
        let view = build_planning_view(&PlanningViewInputs {
            goal: "fill out form",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: Some(&store),
            knowledge_store: None,
            workflow_id: Some("wf"),
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });

        assert_eq!(view.memories.len(), 2);
        assert_eq!(view.omitted_counts.memories, 3);
    }

    #[test]
    fn pr3_workflow_with_no_memories_returns_empty_with_rationale() {
        let store = fresh_store();
        // Seed only OTHER workflow's memories — should not surface here.
        seed_memory(
            &store,
            "other-wf",
            cel_store::cortex_memory::MemoryKind::Outcome,
            "did something",
            serde_json::json!({}),
        );

        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "any",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: Some(&store),
            knowledge_store: None,
            workflow_id: Some("the-empty-workflow"),
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });

        assert!(view.memories.is_empty());
        assert_eq!(view.omitted_counts.memories, 0);
        let rationale = view.selection_rationale.expect("expected rationale");
        assert!(
            rationale.contains("No prior memories"),
            "expected empty-workflow rationale; got {rationale}"
        );
    }

    #[test]
    fn pr3_irrelevant_memories_score_zero_and_are_dropped() {
        let store = fresh_store();
        // Fully off-topic memories with no goal-keyword overlap.
        seed_memory(
            &store,
            "wf",
            cel_store::cortex_memory::MemoryKind::Outcome,
            "Watered the plants in the kitchen",
            serde_json::json!({"plants": "many"}),
        );
        seed_memory(
            &store,
            "wf",
            cel_store::cortex_memory::MemoryKind::Outcome,
            "Rebooted the router after midnight",
            serde_json::json!({"router": "fixed"}),
        );

        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit invoice in Concur",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: Some(&store),
            knowledge_store: None,
            workflow_id: Some("wf"),
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });

        // Both candidates score 0 → both omitted, none kept.
        assert_eq!(view.memories.len(), 0);
        assert_eq!(view.omitted_counts.memories, 2);
    }

    /// WK4: the PR3 "store_open_failure" test no longer exists at this
    /// layer — the builder no longer opens the store, so open failure
    /// can't happen here. The equivalent failure surface is now in the
    /// canonical runner: when `RunLimits.memory_db_path` points at a bad
    /// file, the runner logs and proceeds with `memory_store: None` (see
    /// `pr2_outcome_*` tests + the new `wk4_open_failure_at_runner_*`
    /// tests in `cel-goal-runner::canonical_runner::tests`).
    ///
    /// We keep a thin trait-level analogue here: a store impl that
    /// always errors must produce an empty view, not a panic.
    #[test]
    fn wk4_store_read_failure_returns_empty_view_no_panic() {
        struct AlwaysErrStore;
        impl cel_store::CortexMemoryStore for AlwaysErrStore {
            fn list_for_workflow(
                &self,
                _: &str,
                _: Option<&[cel_store::cortex_memory::MemoryKind]>,
                _: usize,
            ) -> Result<Vec<cel_store::cortex_memory::CortexMemory>, cel_store::StoreError>
            {
                Err(cel_store::StoreError::NotFound("simulated".into()))
            }
            fn insert_memory(
                &self,
                _: &cel_store::cortex_memory::NewCortexMemory,
            ) -> Result<i64, cel_store::StoreError> {
                Err(cel_store::StoreError::NotFound("simulated".into()))
            }
            fn search_for_workflow_ranked(
                &self,
                _: &str,
                _: &str,
                _: usize,
            ) -> Result<Vec<cel_store::cortex_memory::CortexMemory>, cel_store::StoreError>
            {
                Err(cel_store::StoreError::NotFound("simulated".into()))
            }
        }
        let store = AlwaysErrStore;
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        // Goal with usable keywords so FTS5 path is exercised — and
        // the fallback `list_for_workflow` also returns Err. Both reads
        // fail; the view must still build with empty memories.
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit invoice via Concur",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: Some(&store),
            knowledge_store: None,
            workflow_id: Some("wf"),
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        assert!(view.memories.is_empty());
    }

    #[test]
    fn pr3_quoted_phrase_in_goal_boosts_matching_memory() {
        let store = fresh_store();
        // Two memories — one matches the quoted phrase exactly.
        seed_memory(
            &store,
            "wf",
            cel_store::cortex_memory::MemoryKind::Prior,
            "Concur uses two-step submit",
            serde_json::json!({"app": "Concur"}),
        );
        seed_memory(
            &store,
            "wf",
            cel_store::cortex_memory::MemoryKind::Prior,
            "Some unrelated submit notes",
            serde_json::json!({}),
        );

        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit \"two-step submit\" form",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: Some(&store),
            knowledge_store: None,
            workflow_id: Some("wf"),
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });

        assert!(view.memories[0].summary.contains("two-step"));
    }

    // ─── Tier A1: knowledge hydration ────────────────────────────────────────

    fn seed_knowledge(
        store: &std::sync::Mutex<cel_store::CelStore>,
        content: &str,
        source: &str,
    ) -> i64 {
        store
            .lock()
            .expect("seed: store mutex poisoned")
            .add_knowledge(content, source)
            .expect("add_knowledge")
    }

    #[test]
    fn a1_knowledge_silent_when_store_not_provided() {
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit invoice in Concur",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        // PR1 perception-only behaviour preserved.
        assert!(view.knowledge.is_empty());
        assert_eq!(view.omitted_counts.knowledge, 0);
    }

    #[test]
    fn a1_relevant_knowledge_hydrated_via_fts5_bm25() {
        let store = fresh_store();
        seed_knowledge(
            &store,
            "Concur uses a two-step submit: first 'Save', then 'Submit'.",
            "manual",
        );
        seed_knowledge(
            &store,
            "Espresso machine descaling guide step by step.",
            "wiki",
        );
        seed_knowledge(
            &store,
            "Submit button on payroll forms requires a separate confirmation.",
            "ops_doc",
        );
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit invoice in Concur",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: Some(&store),
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        // Two facts mention "submit" / "Concur" tokens; the espresso
        // one doesn't. FTS5 returns only the matching pair.
        assert_eq!(view.knowledge.len(), 2);
        assert!(view
            .knowledge
            .iter()
            .all(|k| !k.content.contains("Espresso")));
    }

    #[test]
    fn a1_knowledge_capped_by_budget_and_records_omitted() {
        let store = fresh_store();
        for i in 0..6 {
            seed_knowledge(
                &store,
                &format!("Submit step number {i} in the workflow."),
                "manual",
            );
        }
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget {
            max_knowledge: 3,
            ..PlanningBudget::default()
        };
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit step",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: Some(&store),
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        assert_eq!(view.knowledge.len(), 3);
        assert_eq!(view.omitted_counts.knowledge, 3);
    }

    #[test]
    fn a1_no_match_yields_empty_knowledge() {
        let store = fresh_store();
        seed_knowledge(
            &store,
            "Espresso machine descaling guide step by step.",
            "wiki",
        );
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit invoice in Concur",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: Some(&store),
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        assert_eq!(view.knowledge.len(), 0);
        // 0 candidates returned by FTS5 → 0 omitted (we never had them).
        assert_eq!(view.omitted_counts.knowledge, 0);
    }

    #[test]
    fn a1_no_keywords_in_goal_yields_empty_knowledge() {
        // Goal has only stop words → safe_fts5_query_from_keywords
        // returns None → selector exits early, never queries the store.
        let store = fresh_store();
        seed_knowledge(&store, "important fact about anything", "doc");
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "do it",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: Some(&store),
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        assert!(view.knowledge.is_empty());
        assert_eq!(view.omitted_counts.knowledge, 0);
    }

    #[test]
    fn a1_rationale_mentions_knowledge_when_hydrated() {
        let store = fresh_store();
        seed_knowledge(
            &store,
            "Concur submit button is on the upper right",
            "manual",
        );
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit invoice in Concur",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: Some(&store),
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        let rationale = view.selection_rationale.expect("expected rationale");
        assert!(
            rationale.contains("knowledge"),
            "expected rationale to mention knowledge; got {rationale}"
        );
    }

    #[test]
    fn a1_store_read_failure_returns_empty_knowledge_no_panic() {
        struct AlwaysErrKnowledge;
        impl cel_store::KnowledgeStore for AlwaysErrKnowledge {
            fn search_knowledge_for_workflow(
                &self,
                _: &str,
                _: Option<&str>,
                _: usize,
            ) -> Result<Vec<cel_store::ScoredKnowledge>, cel_store::StoreError> {
                Err(cel_store::StoreError::NotFound("simulated".into()))
            }
        }
        let store = AlwaysErrKnowledge;
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit invoice in Concur",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: Some(&store),
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        assert!(view.knowledge.is_empty());
    }

    #[test]
    fn a1_memory_and_knowledge_can_coexist() {
        // Same Mutex<CelStore> handle satisfies BOTH traits — this is
        // the production canonical-runner shape. Verify both selectors
        // run, both surface, both rationale lines appear.
        let store = fresh_store();
        seed_memory(
            &store,
            "wf",
            cel_store::cortex_memory::MemoryKind::Outcome,
            "Submitted invoice via Concur successfully",
            serde_json::json!({}),
        );
        seed_knowledge(&store, "Concur submit requires manager approval", "ops_doc");

        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit invoice in Concur",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: Some(&store),
            knowledge_store: Some(&store),
            workflow_id: Some("wf"),
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        assert_eq!(view.memories.len(), 1);
        assert_eq!(view.knowledge.len(), 1);
        let rationale = view.selection_rationale.expect("expected rationale");
        assert!(rationale.contains("workflow memor"));
        assert!(rationale.contains("knowledge fact"));
    }

    // ─── Tier A2: recent_events from observations ────────────────────────────

    fn seed_observation(
        store: &std::sync::Mutex<cel_store::CelStore>,
        workflow_name: &str,
        content: &str,
        priority: cel_store::ObservationPriority,
    ) -> i64 {
        store
            .lock()
            .expect("seed: store mutex poisoned")
            .add_observation(workflow_name, content, &priority, &[], None, None)
            .expect("add_observation")
    }

    #[test]
    fn a2_recent_events_silent_when_store_not_provided() {
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "any",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        // PR1 perception-only behaviour preserved.
        assert!(view.recent_events.is_empty());
        assert_eq!(view.omitted_counts.recent_events, 0);
    }

    #[test]
    fn a2_recent_events_silent_when_workflow_id_missing() {
        // Observations are workflow-scoped via workflow_name; without
        // a workflow_id we can't pick them. Silent rather than wrong.
        let store = fresh_store();
        seed_observation(
            &store,
            "wf",
            "noise",
            cel_store::ObservationPriority::Medium,
        );
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "any",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: None, // <-- key
            goal_embedding: None,
            recent_events_store: Some(&store),
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        assert!(view.recent_events.is_empty());
    }

    #[test]
    fn a2_recent_events_hydrate_in_priority_then_recency_order() {
        let store = fresh_store();
        // Insert in mixed order; underlying ORDER BY in get_observations
        // surfaces high → medium → low, then created_at DESC within
        // priority. Observations are independent of goal keywords —
        // they're curated summaries, not keyword-search candidates.
        seed_observation(
            &store,
            "wf",
            "low priority older",
            cel_store::ObservationPriority::Low,
        );
        seed_observation(
            &store,
            "wf",
            "medium priority middle",
            cel_store::ObservationPriority::Medium,
        );
        seed_observation(
            &store,
            "wf",
            "high priority newest",
            cel_store::ObservationPriority::High,
        );
        seed_observation(
            &store,
            "wf",
            "high priority second-newest",
            cel_store::ObservationPriority::High,
        );

        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "any goal",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: Some("wf"),
            goal_embedding: None,
            recent_events_store: Some(&store),
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        assert_eq!(view.recent_events.len(), 4);
        // High-priority pair surface first (most-recent within priority
        // first), then medium, then low.
        assert!(view.recent_events[0].kind.contains("high"));
        assert!(view.recent_events[1].kind.contains("high"));
        assert!(view.recent_events[2].kind.contains("medium"));
        assert!(view.recent_events[3].kind.contains("low"));
    }

    #[test]
    fn a2_recent_events_capped_by_budget_and_records_omitted() {
        let store = fresh_store();
        for i in 0..7 {
            seed_observation(
                &store,
                "wf",
                &format!("note {i}"),
                cel_store::ObservationPriority::Medium,
            );
        }
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget {
            max_recent_events: 3,
            ..PlanningBudget::default()
        };
        let view = build_planning_view(&PlanningViewInputs {
            goal: "any",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: Some("wf"),
            goal_embedding: None,
            recent_events_store: Some(&store),
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        assert_eq!(view.recent_events.len(), 3);
        assert_eq!(view.omitted_counts.recent_events, 4);
    }

    #[test]
    fn a2_event_ref_id_and_kind_format_is_stable() {
        let store = fresh_store();
        let id = seed_observation(
            &store,
            "wf",
            "important",
            cel_store::ObservationPriority::High,
        );
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "any",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: Some("wf"),
            goal_embedding: None,
            recent_events_store: Some(&store),
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        assert_eq!(view.recent_events.len(), 1);
        let ev = &view.recent_events[0];
        // ID format pinned so future tooling can parse it back.
        assert_eq!(ev.id, format!("obs:{id}"));
        // Kind includes priority for planner weighting.
        assert_eq!(ev.kind, "observation:high");
        assert_eq!(ev.summary, "important");
        // `at` populated from observed_at or created_at — both nullable
        // at insert; defaults to created_at fallback.
        assert!(ev.at.is_some(), "expected non-empty at timestamp");
    }

    #[test]
    fn a2_rationale_mentions_recent_events_when_hydrated() {
        let store = fresh_store();
        seed_observation(
            &store,
            "wf",
            "anything",
            cel_store::ObservationPriority::Medium,
        );
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "any",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: Some("wf"),
            goal_embedding: None,
            recent_events_store: Some(&store),
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        let rationale = view.selection_rationale.expect("expected rationale");
        assert!(
            rationale.contains("recent event"),
            "expected rationale to mention recent_events; got {rationale}"
        );
    }

    #[test]
    fn a2_store_read_failure_returns_empty_no_panic() {
        struct AlwaysErrEvents;
        impl cel_store::RecentEventStore for AlwaysErrEvents {
            fn recent_events_for_workflow(
                &self,
                _: &str,
                _: usize,
            ) -> Result<Vec<cel_store::Observation>, cel_store::StoreError> {
                Err(cel_store::StoreError::NotFound("simulated".into()))
            }
        }
        let store = AlwaysErrEvents;
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "any",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: Some("wf"),
            goal_embedding: None,
            recent_events_store: Some(&store),
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        assert!(view.recent_events.is_empty());
    }

    // ─── Tier A3: anomalies + blockers ──────────────────────────────────────

    fn make_anomaly(kind: AnomalyType, description: &str, element_id: Option<&str>) -> Anomaly {
        Anomaly {
            anomaly_type: kind,
            title: None,
            description: description.into(),
            timestamp: 0,
            element_ids: element_id.into_iter().map(String::from).collect(),
        }
    }

    fn make_freshness(state: FreshnessState, age_ms: u64) -> FreshnessAssessment {
        FreshnessAssessment {
            state,
            causes: vec![],
            age_ms,
            confidence: 1.0,
            last_update_ms: 0,
            last_event_ms: None,
            last_significant_event_ms: None,
        }
    }

    #[test]
    fn a3_silent_when_no_anomalies_and_no_freshness() {
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "any",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        assert!(view.anomalies.is_empty());
        assert!(view.blockers.is_empty());
    }

    #[test]
    fn a3_dialog_anomaly_surfaces_as_anomaly_and_blocker() {
        // Dialog is in the blocking subset → produces BOTH an
        // AnomalyRef and a Blocker. Element id from the anomaly's
        // first element_id should attach to the blocker.
        let anomalies = vec![make_anomaly(
            AnomalyType::Dialog,
            "Save changes before quitting?",
            Some("ax:save-dialog"),
        )];
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "any",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: Some(&anomalies),
            cortex_freshness: None,
        });
        assert_eq!(view.anomalies.len(), 1);
        assert_eq!(view.anomalies[0].kind, "dialog");
        assert_eq!(view.blockers.len(), 1);
        assert_eq!(view.blockers[0].kind, "modal_dialog");
        assert_eq!(
            view.blockers[0].element_id.as_deref(),
            Some("ax:save-dialog")
        );
    }

    #[test]
    fn a3_auth_prompt_surfaces_as_anomaly_and_blocker() {
        let anomalies = vec![make_anomaly(
            AnomalyType::AuthPrompt,
            "Sign in to continue",
            Some("ax:login-button"),
        )];
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "any",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: Some(&anomalies),
            cortex_freshness: None,
        });
        assert_eq!(view.anomalies[0].kind, "auth_prompt");
        assert_eq!(view.blockers.len(), 1);
        assert_eq!(view.blockers[0].kind, "auth_required");
    }

    #[test]
    fn a3_error_anomaly_surfaces_as_anomaly_only_no_blocker() {
        // Errors are informational, not blocking. The planner can
        // adapt without the heavier blocker treatment.
        let anomalies = vec![
            make_anomaly(AnomalyType::Error, "Network timeout", None),
            make_anomaly(AnomalyType::AppSwitch, "Frontmost changed to Mail", None),
        ];
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "any",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: Some(&anomalies),
            cortex_freshness: None,
        });
        assert_eq!(view.anomalies.len(), 2);
        assert!(view.blockers.is_empty(), "Error/AppSwitch must not block");
    }

    #[test]
    fn a3_hard_stale_freshness_produces_blocker() {
        let freshness = make_freshness(FreshnessState::HardStale, 10_000);
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "any",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: Some(&freshness),
        });
        assert!(view.anomalies.is_empty());
        assert_eq!(view.blockers.len(), 1);
        assert_eq!(view.blockers[0].kind, "stale_perception");
        assert!(view.blockers[0].description.contains("hard-stale"));
    }

    #[test]
    fn a3_soft_stale_freshness_produces_anomaly_only() {
        // Soft-stale: visible to the planner but not blocking.
        // Ranking signal still trustworthy.
        let freshness = make_freshness(FreshnessState::SoftStale, 2_000);
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "any",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: Some(&freshness),
        });
        assert!(view.blockers.is_empty(), "soft-stale must NOT block");
        assert_eq!(view.anomalies.len(), 1);
        assert_eq!(view.anomalies[0].kind, "perception_soft_stale");
    }

    #[test]
    fn a3_fresh_freshness_contributes_nothing() {
        let freshness = make_freshness(FreshnessState::Fresh, 50);
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "any",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: Some(&freshness),
        });
        assert!(view.anomalies.is_empty());
        assert!(view.blockers.is_empty());
    }

    #[test]
    fn a3_anomalies_and_freshness_combine() {
        // Both signals present: dialog → anomaly + blocker; hard-stale
        // → blocker. Total: 1 anomaly + 2 blockers.
        let anomalies = vec![make_anomaly(
            AnomalyType::Dialog,
            "Confirm delete?",
            Some("ax:confirm"),
        )];
        let freshness = make_freshness(FreshnessState::HardStale, 9_999);
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "any",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: Some(&anomalies),
            cortex_freshness: Some(&freshness),
        });
        assert_eq!(view.anomalies.len(), 1, "1 anomaly from dialog");
        assert_eq!(
            view.blockers.len(),
            2,
            "1 blocker from dialog + 1 from hard-stale"
        );
        let blocker_kinds: Vec<&str> = view.blockers.iter().map(|b| b.kind.as_str()).collect();
        assert!(blocker_kinds.contains(&"modal_dialog"));
        assert!(blocker_kinds.contains(&"stale_perception"));
    }

    #[test]
    fn a3_rationale_mentions_anomalies_and_blockers_when_present() {
        let anomalies = vec![make_anomaly(AnomalyType::Dialog, "Confirm?", None)];
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "any",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: None,
            knowledge_store: None,
            workflow_id: None,
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: Some(&anomalies),
            cortex_freshness: None,
        });
        let rationale = view.selection_rationale.expect("expected rationale");
        assert!(
            rationale.contains("anomaly") || rationale.contains("anomalies"),
            "expected rationale to mention anomalies; got {rationale}"
        );
        assert!(
            rationale.contains("blocker"),
            "expected rationale to mention blockers; got {rationale}"
        );
    }

    // ─── WK2: vector embedding cosine boost ──────────────────────────────────

    /// Deterministic "embedder" for tests — same byte-level output as
    /// `cel_llm::Embedder` but synchronous and inline. Mirrors the
    /// stub in `cel-llm::embedder::tests` (same hash bucketing) so a
    /// memory embedded via the runner's stub embedder and a goal
    /// embedded inline produce comparable vectors.
    fn embed_inline(text: &str, dim: usize) -> Vec<u8> {
        let mut out = vec![0f32; dim];
        for (i, b) in text.bytes().enumerate() {
            out[i % dim] += (b as f32) / 255.0;
        }
        let mag: f32 = out.iter().map(|x| x * x).sum::<f32>().sqrt();
        if mag > 0.0 {
            for v in &mut out {
                *v /= mag;
            }
        }
        cel_llm::EmbeddingVector::new(out).to_bytes()
    }

    fn seed_memory_with_embedding(
        store: &std::sync::Mutex<cel_store::CelStore>,
        workflow: &str,
        kind: cel_store::cortex_memory::MemoryKind,
        summary: &str,
        embedded_text: &str,
        dim: usize,
    ) -> i64 {
        let bytes = embed_inline(embedded_text, dim);
        store
            .lock()
            .expect("seed: store mutex poisoned")
            .insert_cortex_memory(&cel_store::cortex_memory::NewCortexMemory {
                workflow_id: workflow.into(),
                kind,
                content: serde_json::json!({}),
                summary: Some(summary.into()),
                tags: vec![],
                source_ref: None,
                embedding: Some(bytes),
            })
            .expect("insert")
    }

    #[test]
    fn wk2_cosine_boost_reranks_when_goal_embedding_provided() {
        // **Test design contract**: this assertion must FAIL if the
        // cosine boost is removed from `score_memory`. Both memories
        // have IDENTICAL keyword overlap with the goal (same three
        // keywords in their summaries), so without WK2 their base ×
        // decay scores tie and ordering is FTS5-bm25-dependent (not
        // deterministically aligned). With WK2, the embedding alignment
        // decisively breaks the tie via the 0.5x cosine boost.
        let dim = 16;
        let store = fresh_store();

        // Memory UNRELATED: 3 goal keywords in summary (base = 9),
        // embedded with text that doesn't share any goal keywords
        // → cos ≈ low.
        let id_unrelated = seed_memory_with_embedding(
            &store,
            "wf",
            cel_store::cortex_memory::MemoryKind::Outcome,
            "Submitted invoice via Concur — alpha",
            "watered the kitchen plants this morning",
            dim,
        );
        // Memory ALIGNED: 3 goal keywords in summary (base = 9 —
        // identical to UNRELATED), embedded with the exact goal
        // text → cos ≈ 1, max boost (0.5x amplification).
        let id_aligned = seed_memory_with_embedding(
            &store,
            "wf",
            cel_store::cortex_memory::MemoryKind::Outcome,
            "Submitted invoice via Concur — beta",
            "submit invoice in Concur",
            dim,
        );

        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let goal_bytes = embed_inline("submit invoice in Concur", dim);

        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit invoice in Concur",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: Some(&store),
            knowledge_store: None,
            workflow_id: Some("wf"),
            goal_embedding: Some(&goal_bytes),
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });

        assert_eq!(
            view.memories.len(),
            2,
            "expected both keyword-matched memories"
        );
        // Aligned memory MUST win — the only differentiator vs
        // UNRELATED is the cosine boost (bases tie at 9). Removing the
        // cosine path from `score_memory` would let UNRELATED tie or
        // win on FTS5 bm25 ordering.
        assert_eq!(
            view.memories[0].id, id_aligned,
            "expected cosine-aligned memory ranked first; \
             got id={} (unrelated id={}, aligned id={})",
            view.memories[0].id, id_unrelated, id_aligned
        );
        assert_eq!(view.memories[1].id, id_unrelated);
    }

    #[test]
    fn wk2_no_goal_embedding_falls_back_to_pure_wk1() {
        // Same setup as the boost test, but with `goal_embedding: None`.
        // Selector must NOT consult stored embeddings — pure WK1
        // behaviour. We just check it doesn't panic and returns both.
        let dim = 16;
        let store = fresh_store();
        seed_memory_with_embedding(
            &store,
            "wf",
            cel_store::cortex_memory::MemoryKind::Outcome,
            "Submitted invoice via Concur",
            "submit invoice",
            dim,
        );
        seed_memory_with_embedding(
            &store,
            "wf",
            cel_store::cortex_memory::MemoryKind::Outcome,
            "Submitted invoice yet again",
            "submit invoice",
            dim,
        );
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit invoice",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: Some(&store),
            knowledge_store: None,
            workflow_id: Some("wf"),
            goal_embedding: None, // <-- key
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        assert_eq!(view.memories.len(), 2);
    }

    #[test]
    fn wk2_dimension_mismatch_safely_skips_cosine_boost() {
        // Memory was embedded with dim=16, goal embedding is dim=32 —
        // dimension mismatch. Cosine path must NOT panic; selector
        // falls back to pure WK1 base * decay. Ranking is preserved
        // (memory still hydrated via FTS5 keyword match).
        let store = fresh_store();
        seed_memory_with_embedding(
            &store,
            "wf",
            cel_store::cortex_memory::MemoryKind::Outcome,
            "Submitted invoice via Concur successfully",
            "submit invoice in Concur",
            16, // memory dim
        );
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let goal_bytes = embed_inline("submit invoice in Concur", 32); // DIFFERENT dim

        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit invoice in Concur",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: Some(&store),
            knowledge_store: None,
            workflow_id: Some("wf"),
            goal_embedding: Some(&goal_bytes),
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        // Memory still surfaces via FTS5+decay; no panic, no NaN scoring.
        assert_eq!(view.memories.len(), 1);
    }

    #[test]
    fn wk2_corrupted_embedding_bytes_safely_skip_cosine_boost() {
        // Misaligned bytes (not multiple of 4) → from_bytes returns
        // None → cosine path skipped → memory still scored via base ×
        // decay alone. Defensive against schema-corruption scenarios.
        let store = fresh_store();
        store
            .lock()
            .unwrap()
            .insert_cortex_memory(&cel_store::cortex_memory::NewCortexMemory {
                workflow_id: "wf".into(),
                kind: cel_store::cortex_memory::MemoryKind::Outcome,
                content: serde_json::json!({}),
                summary: Some("submit invoice with bad embedding".into()),
                tags: vec![],
                source_ref: None,
                embedding: Some(vec![1, 2, 3]), // 3 bytes — not f32-aligned
            })
            .expect("insert");

        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let goal_bytes = embed_inline("submit invoice", 16);
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit invoice",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: Some(&store),
            knowledge_store: None,
            workflow_id: Some("wf"),
            goal_embedding: Some(&goal_bytes),
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        // No panic. Memory hydrates via FTS5+decay despite invalid
        // stored embedding bytes.
        assert_eq!(view.memories.len(), 1);
    }

    #[test]
    fn wk2_embedding_never_outranks_keyword_zero_score() {
        // Critical contract: cosine boost is multiplicative on `base`;
        // a memory with no keyword overlap (base = 0) MUST stay at 0
        // even if its embedding is a perfect cosine match. Embeddings
        // enrich keyword-matched ranking; they don't expand the matched
        // set.
        let dim = 16;
        let store = fresh_store();
        seed_memory_with_embedding(
            &store,
            "wf",
            cel_store::cortex_memory::MemoryKind::Outcome,
            // Summary has NO goal keyword overlap.
            "Watered the kitchen plants this morning",
            "submit invoice in Concur", // perfect cosine match to goal
            dim,
        );
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let goal_bytes = embed_inline("submit invoice in Concur", dim);
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit invoice in Concur",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: Some(&store),
            knowledge_store: None,
            workflow_id: Some("wf"),
            goal_embedding: Some(&goal_bytes),
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });
        // Cosine alone cannot lift a base=0 memory. Empty hydration.
        assert_eq!(view.memories.len(), 0);
    }

    /// WK1: end-to-end check that the FTS5 pre-filter narrows the
    /// candidate window before the Rust scorer runs. We seed a workflow
    /// with one keyword-matching memory + many unrelated ones; the
    /// selector must return exactly the matching one without the
    /// unrelated rows polluting its candidate window.
    #[test]
    fn wk1_fts5_prefilter_narrows_candidates_to_keyword_matches() {
        let store = fresh_store();
        // 50 unrelated memories — pre-WK1, the selector would pull these
        // as part of "200 most recent" and the Rust scorer would reject
        // them. Post-WK1, FTS5 never returns them; the scorer never sees
        // them; `omitted_counts.memories` reflects only the FTS5-matched
        // candidates that scored too low.
        for i in 0..50 {
            seed_memory(
                &store,
                "wf",
                cel_store::cortex_memory::MemoryKind::Outcome,
                &format!("Watered the plants on day {i}"),
                serde_json::json!({"day": i}),
            );
        }
        // 1 keyword-matching memory.
        seed_memory(
            &store,
            "wf",
            cel_store::cortex_memory::MemoryKind::Outcome,
            "Submitted invoice via Concur successfully",
            serde_json::json!({"goal": "submit invoice"}),
        );

        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit invoice in Concur",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_store: Some(&store),
            knowledge_store: None,
            workflow_id: Some("wf"),
            goal_embedding: None,
            recent_events_store: None,
            cortex_anomalies: None,
            cortex_freshness: None,
        });

        // Exactly one memory hydrated; the unrelated 50 never made it
        // into the candidate window.
        assert_eq!(view.memories.len(), 1);
        assert!(view.memories[0]
            .summary
            .to_lowercase()
            .contains("submitted invoice"));
        // omitted_counts.memories reflects FTS5-returned candidates
        // (1 here) minus kept (1) = 0. Pre-WK1 this would have been
        // 50 (the recency-sourced unrelated rows scored to 0 and
        // were dropped).
        assert_eq!(view.omitted_counts.memories, 0);
    }
}
