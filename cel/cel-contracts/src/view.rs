//! `PlanningView` — the budgeted, agent-facing projection of CEL state.
//!
//! `MentalModel` (in cel-cortex) can be rich. `PlanningView` must be small.
//! Planners receive this, not the full mental model, so prompts stay under
//! token budgets and the same context contract works across every planner
//! runtime (canonical Rust runner, LangGraph, Codex, in-house).
//!
//! See `COGNITION_LAYER_PLAN.md` for the principle: **store broadly, select
//! narrowly**.
//!
//! Memory / knowledge / event refs are typed here but populated only by
//! later PRs (memory store + memory-aware selection). PR1a only fills
//! `screen`, `elements`, `adapter_facts`, `adapter_actions`,
//! `capabilities`, `blockers`, `anomalies`, `evidence`,
//! `omitted_counts`, `selection_rationale`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ─── Top-level view ──────────────────────────────────────────────────────────

/// Compact, budgeted projection of CEL state for one planner call.
///
/// Built by `cel-cortex`'s planning-view builder from `MentalModel` plus
/// active adapter facts. Consumed by every planner — built-in or external —
/// instead of raw `ScreenContext`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanningView {
    /// The natural-language goal the planner is working on.
    pub goal: String,
    /// The budget the caller asked the builder to respect.
    pub budget: PlanningBudget,

    /// Current screen / app / window summary.
    pub screen: PlanningScreen,
    /// Selected elements relevant to the goal, after budget compression.
    pub elements: Vec<PlanningElement>,
    /// Active adapter-backed facts (Numbers cells, browser DOM facts, etc.).
    /// Empty until adapters expose typed facts; in PR1a builders may leave
    /// this empty if no adapter contributed.
    #[serde(default)]
    pub adapter_facts: Vec<AdapterFactRef>,
    /// Active adapter-backed actions available for this turn. This is the
    /// structured, agent-agnostic contract; LLM planners may render it into
    /// prompt text, while non-LLM agents can inspect it directly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adapter_actions: Vec<AdapterActionRef>,
    /// Capabilities currently wired up (CDP bound, native input enabled, …).
    /// Folds in the boolean flags from the legacy `RuntimeCaps`.
    #[serde(default)]
    pub capabilities: Vec<CapabilityRef>,
    /// Where in the run the planner is. Lets the planner pace itself —
    /// e.g. stop polishing extraction once most of the budget is spent.
    /// Folds in the legacy `RuntimeCaps::steps_used` / `max_steps`.
    #[serde(default)]
    pub run_progress: RunProgress,

    /// Memories selected by the cognition layer (PR3).
    #[serde(default)]
    pub memories: Vec<MemoryRef>,
    /// Knowledge records selected by the cognition layer (PR3).
    #[serde(default)]
    pub knowledge: Vec<KnowledgeRef>,
    /// Recent events / actions / checkpoints relevant to the goal.
    #[serde(default)]
    pub recent_events: Vec<EventRef>,

    /// Things actively blocking forward progress (consent walls, modal
    /// dialogs, missing capability, etc.). Promoted to first-class so the
    /// planner notices them even if elements are aggressively compressed.
    #[serde(default)]
    pub blockers: Vec<Blocker>,
    /// Anomalies the cortex flagged (stale state, unexpected window, etc.).
    #[serde(default)]
    pub anomalies: Vec<AnomalyRef>,
    /// References back to source records that explain why selection picked
    /// what it picked. Filled when a memory / failure / prior outcome
    /// influenced the view.
    #[serde(default)]
    pub evidence: Vec<EvidenceRef>,

    /// One-sentence rationale explaining the selection. Optional —
    /// deterministic selectors may leave this absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection_rationale: Option<String>,
    /// What the builder dropped to fit the budget. Lets the planner know
    /// the view is compressed and how aggressively.
    pub omitted_counts: OmittedCounts,
    /// Transitional pre-rendered "App-Specific Actions" prompt fragment listing the
    /// `{"type": "custom", "adapter": "...", "action": "...", "params": {...}}`
    /// shapes for every currently-active adapter. The cortex-side step
    /// executor builds this once per turn from `Cortex::active_adapter_manifests()`
    /// and the canonical runner stamps it onto the view post-build.
    /// `None` means no adapter actions are available to the planner this
    /// turn — empty equivalent. Prefer `adapter_actions` for new callers;
    /// this field is retained while prompt-only clients migrate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_actions_prompt: Option<String>,
}

// ─── Budget ──────────────────────────────────────────────────────────────────

/// Caller-provided ceilings for one planning view.
///
/// Builders enforce these greedily — most-relevant items first, drop the
/// rest, count what was dropped in `OmittedCounts`. Budgets are advisory,
/// not contractual: a planner with a 128K context window can request a
/// larger budget; a benchmark harness can request a smaller one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanningBudget {
    /// Soft token ceiling for the serialized view as it would appear in an
    /// LLM prompt. Builders make best-effort decisions; the absolute cap is
    /// the per-category limits below.
    pub max_tokens: u32,
    /// Maximum number of `PlanningElement` entries.
    pub max_elements: u32,
    /// Maximum number of `MemoryRef` entries (PR3 selector).
    pub max_memories: u32,
    /// Maximum number of `AdapterFactRef` entries.
    pub max_adapter_facts: u32,
    /// Maximum number of `KnowledgeRef` entries (Tier A1 selector — pulled
    /// from `knowledge_fts` via FTS5 + bm25 ranking, scoped by workflow).
    /// `serde(default)` keeps backward compat with v1 callers that don't
    /// know about this field.
    #[serde(default = "default_max_knowledge")]
    pub max_knowledge: u32,
    /// Maximum number of `EventRef` entries (Tier A2 selector — pulled
    /// from cortex `observations` table, ordered by priority then
    /// recency, scoped by workflow). `serde(default)` keeps backward
    /// compat with pre-A2 callers.
    #[serde(default = "default_max_recent_events")]
    pub max_recent_events: u32,
}

fn default_max_knowledge() -> u32 {
    8
}

fn default_max_recent_events() -> u32 {
    10
}

impl Default for PlanningBudget {
    /// Defaults sized to keep prompts under common LLM context windows.
    /// Planners override per-call when they know better.
    fn default() -> Self {
        Self {
            max_tokens: 8000,
            max_elements: 80,
            max_memories: 8,
            max_adapter_facts: 12,
            max_knowledge: 8,
            max_recent_events: 10,
        }
    }
}

// ─── Run progress ────────────────────────────────────────────────────────────

/// How much of the run budget the planner has consumed so far.
///
/// Replaces the `steps_used` / `max_steps` fields of the legacy `RuntimeCaps`.
/// Helps the planner pace itself toward the terminal phase of the goal.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunProgress {
    /// Steps already consumed on this run (zero on the first call).
    pub steps_used: u32,
    /// Hard cap on total steps for this run.
    pub max_steps: u32,
}

impl RunProgress {
    pub fn steps_remaining(&self) -> u32 {
        self.max_steps.saturating_sub(self.steps_used)
    }
}

// ─── Screen summary ──────────────────────────────────────────────────────────

/// Compact screen-level summary written by perception.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanningScreen {
    /// Frontmost application name (e.g. "Google Chrome", "Numbers").
    pub active_app: String,
    /// Active window title.
    #[serde(default)]
    pub window: String,
    /// One-paragraph natural-language summary of what is on screen.
    /// Optional — builders may leave empty for the deterministic path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Current URL when the active app is a browser with CDP bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

// ─── Compact element representation ──────────────────────────────────────────

/// Compressed element representation for the planner.
///
/// Strict subset of `cel_context::ContextElement` — drops bounds, parent_id,
/// full action vectors, source provenance, properties HashMap. Keeps what
/// the planner needs to identify and act on the element. Builders embed
/// only goal-relevant elements (and nearby anchors) to fit the budget.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanningElement {
    /// Stable element identifier the planner uses in `PlannedAction`.
    pub id: String,
    /// Element kind: "button", "input", "link", "checkbox", "row", etc.
    pub element_type: String,
    /// Human-readable label or visible text. Optional — labelless elements
    /// may still be selected when nearby labels clarify them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Current value for inputs / selects / cells.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// State anchors the planner needs to reason about (focused, selected,
    /// enabled, checked, expanded). Compact subset of `ElementState`.
    pub state: PlanningElementState,
    /// Whether this element supports a click-equivalent action.
    #[serde(default)]
    pub clickable: bool,
    /// Whether this element supports `set_value` (form fields).
    #[serde(default)]
    pub settable: bool,
    /// For `<select>` / combobox elements: enumerated option values
    /// so the planner can `set_value` with a real `value=` attribute
    /// instead of guessing slugs. Populated from
    /// `ContextElement.properties["select_options"]` when the source
    /// adapter provides it (browser CDP / DOM extractor). Empty / None
    /// for everything else.
    ///
    /// Format encoded by the browser adapter as
    /// `"value|Label, value2|Label 2, ..."` — strings separated by
    /// `", "`, each entry split on `"|"`. The planner-side rendering
    /// re-parses this; older clients that don't know the format show
    /// the raw string, which is still strictly more useful than the
    /// pre-fix behaviour of showing nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select_options: Option<String>,
}

/// State flags that matter for planning. Strict subset of `ElementState`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanningElementState {
    pub focused: bool,
    pub selected: bool,
    pub enabled: bool,
    pub checked: bool,
    pub expanded: bool,
}

// ─── References to source records ────────────────────────────────────────────

/// Reference to a memory record selected for this view.
///
/// Hydrated by the memory-aware selector (PR3). PR1a builders leave this
/// list empty.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRef {
    /// Stable id of the memory record in `cortex_memories`.
    pub id: i64,
    /// Memory kind: "outcome", "prior", "failure", "preference".
    pub kind: String,
    /// One-line summary suitable for embedding in a prompt.
    pub summary: String,
    /// Raw content payload (free text or structured JSON).
    pub content: serde_json::Value,
    /// When the memory was created (ISO-8601 string).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

/// Reference to a knowledge record selected for this view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeRef {
    pub id: i64,
    pub source: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// Reference to an adapter-backed fact relevant to the goal.
///
/// Adapters expose typed facts (e.g. `{adapter: "numbers", kind: "table",
/// payload: { sheet: "Sheet 1", rows: 12, cols: 8 }}`). Selectors include
/// only facts about the active adapter for the focused app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterFactRef {
    /// Optional stable id supplied by the adapter. When absent, CEL
    /// synthesizes evidence ids from adapter/kind/payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub adapter: String,
    pub kind: String,
    pub payload: serde_json::Value,
}

/// Adapter-backed action currently available to a planner.
///
/// This is the structured form of "App-Specific Actions". It lets any
/// planner/runtime discover app-specific operations without scraping prompt
/// prose, while still leaving each planner free to render or reason over the
/// action catalogue in its own way.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterActionRef {
    pub adapter: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params_schema: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default)]
    pub mutates_state: bool,
    #[serde(default)]
    pub requires_verification: bool,
    #[serde(default)]
    pub returns_data: bool,
}

/// Reference to a capability the runtime currently has wired up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityRef {
    /// Capability identifier: "cdp_bound", "native_input", "vision",
    /// "numbers_write_cells", etc.
    pub id: String,
    /// Free-form details (browser name + URL for cdp_bound, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Reference to a recent event the planner should be aware of.
///
/// Examples: a checkpoint summary, a recent failed action, a window
/// activation. The full event lives in the run transcript / store; the
/// view carries only what's needed to prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRef {
    /// Stable identifier or hash for cross-referencing the source record.
    pub id: String,
    /// Event kind: "checkpoint", "action_failed", "focus_changed", …
    pub kind: String,
    /// One-line natural-language description.
    pub summary: String,
    /// Optional ISO-8601 timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

/// Reference to evidence that explains why selection picked something.
///
/// Lets a planner trace a memory or fact back to its source record without
/// inflating the view itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    /// Where this evidence came from: "memory", "knowledge", "transcript",
    /// "adapter_fact".
    pub source: String,
    /// Stable id within that source.
    pub id: String,
    /// One-line description suitable for inclusion in prompts.
    pub summary: String,
}

/// Reference to an anomaly the cortex flagged that the planner should see.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomalyRef {
    /// Anomaly kind: "stale_state", "unexpected_window", "missing_target",
    /// "low_confidence", …
    pub kind: String,
    /// Human-readable description.
    pub description: String,
}

/// First-class blocker that should not be lost to compression.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blocker {
    /// Blocker kind: "consent_wall", "modal_dialog", "missing_capability",
    /// "auth_required", …
    pub kind: String,
    /// Human-readable description.
    pub description: String,
    /// Optional element id that represents the blocker (e.g. the consent
    /// banner's accept button).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_id: Option<String>,
}

// ─── What was dropped ────────────────────────────────────────────────────────

/// Counts of items the builder filtered out to fit the budget.
///
/// Lets the planner notice that the view is compressed and decide whether
/// to act on what it sees, request a tier-up, or pivot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OmittedCounts {
    pub elements: u32,
    pub memories: u32,
    pub knowledge: u32,
    pub adapter_facts: u32,
    pub recent_events: u32,
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_default_keeps_typical_prompts_under_common_windows() {
        // Defaults must not push a prompt over 8K tokens for typical use.
        // Planners with bigger windows override per-call.
        let b = PlanningBudget::default();
        assert!(b.max_tokens <= 8000);
        assert!(b.max_elements <= 100);
        assert!(b.max_memories <= 16);
    }

    #[test]
    fn empty_view_serializes_compactly() {
        let view = PlanningView {
            goal: "test".into(),
            budget: PlanningBudget::default(),
            screen: PlanningScreen {
                active_app: "TestApp".into(),
                window: "main".into(),
                summary: None,
                url: None,
            },
            elements: vec![],
            adapter_facts: vec![],
            adapter_actions: vec![],
            capabilities: vec![],
            memories: vec![],
            knowledge: vec![],
            recent_events: vec![],
            blockers: vec![],
            anomalies: vec![],
            evidence: vec![],
            selection_rationale: None,
            omitted_counts: OmittedCounts::default(),
            run_progress: RunProgress::default(),
            adapter_actions_prompt: None,
        };
        let json = serde_json::to_string(&view).unwrap();
        // Empty-list fields use `default` + `skip_serializing_if` where
        // appropriate; sanity-check the size is small for an empty view.
        assert!(
            json.len() < 700,
            "empty view should serialize compactly, got {} chars",
            json.len()
        );
    }

    #[test]
    fn view_roundtrips_through_json() {
        let view = PlanningView {
            goal: "submit form".into(),
            budget: PlanningBudget {
                max_tokens: 4000,
                max_elements: 40,
                max_memories: 4,
                max_adapter_facts: 6,
                max_knowledge: 4,
                max_recent_events: 5,
            },
            screen: PlanningScreen {
                active_app: "Browser".into(),
                window: "Concur — Expenses".into(),
                summary: Some("Expense form, partially filled".into()),
                url: Some("https://concur.example.com/expenses".into()),
            },
            elements: vec![PlanningElement {
                id: "dom:submit".into(),
                element_type: "button".into(),
                label: Some("Submit for Approval".into()),
                value: None,
                state: PlanningElementState {
                    focused: false,
                    selected: false,
                    enabled: true,
                    checked: false,
                    expanded: false,
                },
                clickable: true,
                settable: false,
                select_options: None,
            }],
            adapter_facts: vec![],
            adapter_actions: vec![],
            capabilities: vec![CapabilityRef {
                id: "cdp_bound".into(),
                detail: Some("Google Chrome — concur.example.com".into()),
            }],
            memories: vec![],
            knowledge: vec![],
            recent_events: vec![],
            blockers: vec![],
            anomalies: vec![],
            evidence: vec![],
            selection_rationale: Some("Submit-related element + cdp capability".into()),
            omitted_counts: OmittedCounts {
                elements: 431,
                ..Default::default()
            },
            run_progress: RunProgress {
                steps_used: 7,
                max_steps: 80,
            },
            adapter_actions_prompt: None,
        };
        let json = serde_json::to_string(&view).unwrap();
        let back: PlanningView = serde_json::from_str(&json).unwrap();
        assert_eq!(back.goal, view.goal);
        assert_eq!(back.elements.len(), 1);
        assert_eq!(back.capabilities[0].id, "cdp_bound");
        assert_eq!(back.omitted_counts.elements, 431);
        assert_eq!(back.run_progress.steps_remaining(), 73);
    }

    #[test]
    fn run_progress_remaining_saturates_at_zero() {
        let p = RunProgress {
            steps_used: 100,
            max_steps: 80,
        };
        assert_eq!(p.steps_remaining(), 0);
    }
}
