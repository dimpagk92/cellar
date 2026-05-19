import type { ScreenContext } from "../types.js";

export interface RuntimeCaps {
  cdp_bound: boolean;
  cdp_browser?: string | null;
  cdp_url?: string | null;
  native_input: boolean;
  steps_used: number;
  max_steps: number;
}

export interface CellWrite {
  cell_ref: string;
  value: string;
}

export type CanonicalAction =
  | { type: "click"; target_id: string }
  | { type: "type"; target_id?: string | null; text: string }
  | { type: "key"; key: string }
  | { type: "key_combo"; keys: string[] }
  | { type: "set_value"; target_id: string; value: string }
  | { type: "scroll"; dx: number; dy: number }
  | { type: "drag"; from_target_id: string; to_target_id: string }
  | { type: "wait"; ms: number }
  | { type: "custom"; adapter: string; action: string; params?: unknown }
  | { type: "extract"; goal: string; data: string }
  | { type: "batch"; actions: CanonicalAction[] }
  | { type: "act"; instruction: string }
  | { type: "done"; summary: string; evidence_ids?: string[] }
  | { type: "fail"; reason: string }
  | { type: "ax_action"; target_id: string; action: string; label?: string | null; role_hint?: string | null }
  | { type: "activate_app"; app_name: string }
  | { type: "select"; from_x: number; from_y: number; to_x: number; to_y: number }
  | { type: "cdp_eval"; expression: string }
  | {
      type: "navigate";
      url: string;
      wait_until?: "none" | "domcontentloaded" | "load" | "networkidle";
      timeout_ms?: number;
      dismiss_overlays?: boolean;
    }
  | { type: "notebook_writes"; key?: string; value?: string; category?: string }
  | { type: "extract_with_fallback"; name: string; selectors: string[]; parse_as?: string }
  | { type: "write_cells"; app?: string; sheet?: string | null; table?: string | null; writes: CellWrite[]; verify?: boolean }
  | { type: "read_cells"; app?: string; sheet?: string | null; table?: string | null; cell_refs: string[] };

export type CanonicalStepKind = "deterministic" | "llm_assisted";

export interface CanonicalStep {
  purpose: string;
  kind: CanonicalStepKind;
  action: CanonicalAction;
}

export interface AttemptRecord {
  step_purpose: string;
  action: CanonicalAction;
  succeeded: boolean;
  error?: string | null;
  data: unknown;
}

export type NextMove =
  | {
      kind: "batch";
      purpose: string;
      steps: CanonicalStep[];
    }
  | {
      kind: "done";
      summary: string;
      extracted_data?: unknown;
    }
  | {
      kind: "fail";
      reason: string;
    };

export type CanonicalStepResult =
  | {
      status: "ok";
      data?: unknown;
      discovered_sub_goal?: unknown;
    }
  | {
      status: "err";
      message: string;
      recoverable?: boolean;
    };

export interface DoneVerdict {
  verified: boolean;
  reason: string;
}

export interface FailureReport {
  failing_sub_goal: string;
  failing_step: string;
  attempts: string[];
}

export type GoalOutcome =
  | {
      status: "succeeded";
      summary: string;
      extracted_data?: unknown;
    }
  | ({
      status: "failed";
    } & FailureReport);

export interface PerceptionFrame {
  perception: ScreenContext;
  screenshot_base64?: string | null;
  caps: RuntimeCaps;
  adapter_facts?: AdapterFactRef[];
  cortex_anomalies?: CortexAnomaly[];
  cortex_freshness?: CortexFreshnessAssessment | null;
}

export type CortexAnomalyType = "dialog" | "error" | "app_switch" | "auth_prompt";

export interface CortexAnomaly {
  type: CortexAnomalyType;
  title?: string | null;
  description: string;
  timestamp: number;
  element_ids?: string[];
}

export type FreshnessState = "fresh" | "soft_stale" | "hard_stale";
export type StalenessCause = "time" | "event" | "confidence" | "verification";

export interface CortexFreshnessAssessment {
  state: FreshnessState;
  causes: StalenessCause[];
  age_ms: number;
  confidence: number;
  last_update_ms: number;
  last_event_ms?: number | null;
  last_significant_event_ms?: number | null;
}

// ─── Cortex memory (PR2) ─────────────────────────────────────────────────────
//
// Mirrors `cel_store::cortex_memory`. Durable, workflow-scoped memory
// the cortex selector can hydrate into a PlanningView. Writes are opt-in;
// the `cel_think store_memory` MCP mode and the canonical-runner outcome
// auto-write path are the two callers in PR2.

/** Discriminator for the structured `content` payload. */
export type MemoryKind = "outcome" | "prior" | "failure" | "preference";

/** A single cortex memory record as stored. */
export interface CortexMemory {
  id: number;
  workflow_id: string;
  kind: MemoryKind;
  content: unknown;
  summary?: string | null;
  tags?: string[];
  source_ref?: string | null;
  /** Unix epoch seconds when the memory was first written. */
  created_at: number;
  /** Unix epoch seconds when the memory was last hydrated by the selector. */
  last_accessed_at: number;
}

/**
 * Insert payload — caller-supplied fields. Server fills `id`, `created_at`,
 * `last_accessed_at`. `embedding` is reserved for the PR3 vector pre-filter
 * and may be `null` until the embedder lands.
 */
export interface NewCortexMemory {
  workflow_id: string;
  kind: MemoryKind;
  content: unknown;
  summary?: string;
  tags?: string[];
  source_ref?: string;
  embedding?: number[] | null;
}

// ─── PlanningView (PR1a contract) ────────────────────────────────────────────
//
// Mirrors `cel_contracts::PlanningView`. The cortex builds it; planners
// consume it. Lets every planner runtime — canonical Rust, LangGraph,
// future external — speak the same compact context contract.

export interface PlanningBudget {
  max_tokens: number;
  max_elements: number;
  max_memories: number;
  max_adapter_facts: number;
  /**
   * Tier A1: max KnowledgeRef entries (FTS5-ranked workflow knowledge).
   * Optional for backward compat with v1 callers — Rust side defaults to 8.
   */
  max_knowledge?: number;
  /**
   * Tier A2: max EventRef entries (cortex observations, priority + recency).
   * Optional for backward compat — Rust side defaults to 10.
   */
  max_recent_events?: number;
}

export interface PlanningScreen {
  active_app: string;
  window?: string;
  summary?: string | null;
  url?: string | null;
}

export interface PlanningElementState {
  focused: boolean;
  selected: boolean;
  enabled: boolean;
  checked: boolean;
  expanded: boolean;
}

export interface PlanningElement {
  id: string;
  element_type: string;
  label?: string | null;
  value?: string | null;
  state: PlanningElementState;
  clickable?: boolean;
  settable?: boolean;
}

export interface RunProgress {
  steps_used: number;
  max_steps: number;
}

export interface CapabilityRef {
  id: string;
  detail?: string | null;
}

export interface MemoryRef {
  id: number;
  kind: string;
  summary: string;
  content: unknown;
  created_at?: string | null;
}

export interface KnowledgeRef {
  id: number;
  source: string;
  content: string;
  tags?: string[];
}

export interface AdapterFactRef {
  id?: string | null;
  adapter: string;
  kind: string;
  payload: unknown;
}

export interface AdapterActionRef {
  adapter: string;
  action: string;
  params_schema?: Record<string, string>;
  description?: string;
  mutates_state?: boolean;
  requires_verification?: boolean;
  returns_data?: boolean;
}

export interface EventRef {
  id: string;
  kind: string;
  summary: string;
  at?: string | null;
}

export interface EvidenceRef {
  source: string;
  id: string;
  summary: string;
}

export interface AnomalyRef {
  kind: string;
  description: string;
}

export interface Blocker {
  kind: string;
  description: string;
  element_id?: string | null;
}

export interface OmittedCounts {
  elements: number;
  memories: number;
  knowledge: number;
  adapter_facts: number;
  recent_events: number;
}

export interface PlanningView {
  goal: string;
  budget: PlanningBudget;
  screen: PlanningScreen;
  elements: PlanningElement[];
  adapter_facts: AdapterFactRef[];
  adapter_actions: AdapterActionRef[];
  capabilities: CapabilityRef[];
  run_progress: RunProgress;
  memories: MemoryRef[];
  knowledge: KnowledgeRef[];
  recent_events: EventRef[];
  blockers: Blocker[];
  anomalies: AnomalyRef[];
  evidence: EvidenceRef[];
  selection_rationale?: string | null;
  omitted_counts: OmittedCounts;
  adapter_actions_prompt?: string | null;
}

export interface ReviewDecision {
  approved: boolean;
  edited_step?: CanonicalStep;
  feedback?: string;
}
