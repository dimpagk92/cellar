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

// Split into focused submodules, re-exported so `build_planning_view` and
// `PlanningViewInputs` stay at the crate path:
//   selection — deterministic memory/knowledge/event/anomaly/adapter selectors
//   rationale — rationale + evidence synthesis
//   elements  — goal-aware element selection + scoring
//   util      — date/content-preview formatting helpers
// This file retains `PlanningViewInputs` and the `build_planning_view` entry point.
mod elements;
mod rationale;
mod selection;
#[cfg(test)]
mod tests;
mod util;

use elements::select_elements;
use rationale::{build_rationale, caps_to_capabilities, synthesize_evidence};
use selection::{
    select_adapter_facts, select_anomalies_and_blockers, select_knowledge, select_memories,
    select_recent_events, KnowledgeSelection, MemorySelection, RecentEventsSelection,
};

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
    /// Closing-gap fill: adapter facts collected by the runner from
    /// active adapters via `StepExecutor::adapter_facts`. When set,
    /// they populate `view.adapter_facts` directly + contribute one
    /// `EvidenceRef` each to `view.evidence`. `None` (default) keeps
    /// `view.adapter_facts` empty — pre-closure behaviour.
    ///
    /// Adapter selection is the adapter's own concern: each adapter's
    /// `facts_for_planning_view` impl decides what's relevant for the
    /// current goal + perception. The runner aggregates without
    /// reranking.
    pub adapter_facts: Option<&'a [cel_contracts::AdapterFactRef]>,
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
    let adapter_fact_selection = select_adapter_facts(
        inputs.adapter_facts.unwrap_or(&[]),
        inputs.budget.max_adapter_facts,
    );

    let selection_rationale = build_rationale(
        elements.len(),
        &memory_selection,
        &knowledge_selection,
        &recent_events_selection,
        &adapter_fact_selection,
        anomalies.len(),
        blockers.len(),
        omitted_elements,
    );

    // Closing-gap fill: synthesize EvidenceRefs from everything the
    // selectors picked, so the planner can trace each surfaced item
    // back to its source. One EvidenceRef per kept memory / knowledge
    // fact / recent event. Adapter facts get their own evidence entries
    // when they're populated (Tier-A adapter integration below).
    let evidence = synthesize_evidence(
        &memory_selection.kept,
        &knowledge_selection.kept,
        &recent_events_selection.kept,
        &adapter_fact_selection.kept,
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
        // Closing-gap fill: adapter_facts now plumbed through from the
        // runner via the new `adapter_facts` field on PlanningViewInputs.
        // Pre-existing callers that don't set the field still get the
        // empty Vec — backward compat preserved.
        adapter_facts: adapter_fact_selection.kept,
        adapter_actions: vec![],
        capabilities: caps_to_capabilities(inputs.caps),
        memories: memory_selection.kept,
        knowledge: knowledge_selection.kept,
        recent_events: recent_events_selection.kept,
        blockers,
        anomalies,
        evidence,
        selection_rationale,
        omitted_counts: OmittedCounts {
            elements: omitted_elements,
            memories: memory_selection.omitted,
            knowledge: knowledge_selection.omitted,
            recent_events: recent_events_selection.omitted,
            adapter_facts: adapter_fact_selection.omitted,
        },
        run_progress: RunProgress {
            steps_used: inputs.caps.steps_used,
            max_steps: inputs.caps.max_steps,
        },
        // Canonical runner stamps adapter actions post-build from
        // `StepExecutor::{adapter_actions, adapter_actions_prompt}` —
        // keeping them out of `PlanningViewInputs` avoids forcing every
        // existing call site (13+ tests, plus production paths) to add
        // fields for values only runner-backed views can snapshot.
        adapter_actions_prompt: None,
    }
}

// ─── Memory selection (PR3, deterministic) ──────────────────────────────────
