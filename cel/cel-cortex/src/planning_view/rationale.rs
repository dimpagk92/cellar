//! Rationale and evidence synthesis for the planning view.
//!
//! Turns the raw selections into the human-readable rationale string and the
//! capability list, and synthesizes the evidence summary the planner cites.

use super::selection::{
    AdapterFactSelection, KnowledgeSelection, MemorySelection, RecentEventsSelection,
};
use super::*;

// 8 args is one above clippy's default threshold (7) but each is a
// meaningful, distinct piece of rationale state. A wrapper struct would
// just trade visible wiring for indirection without aiding readability.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_rationale(
    kept_elements: usize,
    memory_selection: &MemorySelection,
    knowledge_selection: &KnowledgeSelection,
    recent_events_selection: &RecentEventsSelection,
    adapter_fact_selection: &AdapterFactSelection,
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
    let adapter_facts_silent =
        adapter_fact_selection.kept.is_empty() && adapter_fact_selection.omitted == 0;
    let a3_silent = anomaly_count == 0 && blocker_count == 0;
    if memory_silent
        && knowledge_silent
        && recent_events_silent
        && adapter_facts_silent
        && a3_silent
    {
        // Pure perception-only build (PR1 behaviour). Don't synthesise a
        // misleading rationale — leave the field absent so callers don't
        // think memory / knowledge / events / adapter fact / anomaly
        // selection happened when it didn't.
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
    // Closing-gap adapter-fact line.
    if !adapter_facts_silent {
        parts.push(format!(
            "Hydrated {} adapter fact{} (dropped {} above budget).",
            adapter_fact_selection.kept.len(),
            if adapter_fact_selection.kept.len() == 1 {
                ""
            } else {
                "s"
            },
            adapter_fact_selection.omitted,
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

// ─── Evidence synthesis (closing-gap fill) ──────────────────────────────────

/// Build `view.evidence` from everything the selectors picked. One
/// `EvidenceRef` per kept memory / knowledge fact / recent event /
/// adapter fact, so the planner can cross-reference any item back to
/// its source without inflating the view.
///
/// Each `EvidenceRef`:
/// - `source` — `"memory"` / `"knowledge"` / `"observation"` /
///   `"adapter_fact"` (matches the docstring of EvidenceRef in
///   `cel_contracts`).
/// - `id` — the source row's stable id stringified (e.g. `"42"` for
///   memory id 42; `"obs:99"` for an observation; `"<adapter>:<kind>"`
///   for an adapter fact since AdapterFactRef doesn't carry a stable id).
/// - `summary` — short text the planner can include in prompts; uses
///   the source's summary or a fallback.
pub(crate) fn synthesize_evidence(
    memories: &[cel_contracts::MemoryRef],
    knowledge: &[cel_contracts::KnowledgeRef],
    recent_events: &[cel_contracts::EventRef],
    adapter_facts: &[cel_contracts::AdapterFactRef],
) -> Vec<cel_contracts::EvidenceRef> {
    let mut out = Vec::with_capacity(
        memories.len() + knowledge.len() + recent_events.len() + adapter_facts.len(),
    );
    for m in memories {
        out.push(cel_contracts::EvidenceRef {
            source: "memory".into(),
            id: m.id.to_string(),
            summary: m.summary.clone(),
        });
    }
    for k in knowledge {
        out.push(cel_contracts::EvidenceRef {
            source: "knowledge".into(),
            id: k.id.to_string(),
            summary: short_preview(&k.content, 80),
        });
    }
    for e in recent_events {
        out.push(cel_contracts::EvidenceRef {
            source: "observation".into(),
            id: e.id.clone(),
            summary: e.summary.clone(),
        });
    }
    for f in adapter_facts {
        out.push(cel_contracts::EvidenceRef {
            source: "adapter_fact".into(),
            id: adapter_fact_evidence_id(f),
            summary: short_preview(&serde_json::to_string(&f.payload).unwrap_or_default(), 80),
        });
    }
    out
}

fn adapter_fact_evidence_id(fact: &cel_contracts::AdapterFactRef) -> String {
    if let Some(id) = fact.id.as_deref().filter(|id| !id.trim().is_empty()) {
        return id.to_string();
    }
    let payload = serde_json::to_string(&fact.payload).unwrap_or_default();
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in fact
        .adapter
        .as_bytes()
        .iter()
        .chain([b':'].iter())
        .chain(fact.kind.as_bytes())
        .chain([b':'].iter())
        .chain(payload.as_bytes())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{}:{}:{hash:016x}", fact.adapter, fact.kind)
}

fn short_preview(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

// ─── Capabilities folding ────────────────────────────────────────────────────

pub(crate) fn caps_to_capabilities(caps: &RuntimeCaps) -> Vec<CapabilityRef> {
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
