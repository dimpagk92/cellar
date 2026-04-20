/**
 * Goal Runner configuration types and interfaces.
 */

import type {
  ScreenContext,
  PlannedStep,
  PlannedAction,
  PlannerStepRecord,
  GoalMetrics,
  FreshnessAssessment,
  DiffSummary,
  ActionOutcome,
} from "../types.js";
import type { ActCache } from "../cache/act-cache.js";
import type { Cortex } from "../cortex.js";
import type { ValidationResult, ValidatorConfig } from "./validator.js";

/**
 * Duck-typed Cortex interface for run_goal integration.
 * Allows NAPI Cortex proxy (from MCP server) to be used without importing the full TS Cortex class.
 */
export interface CortexProxy {
  isRunning(): boolean;
  model: {
    currentContext: ScreenContext;
    temporal: {
      loading?: { detected: boolean; durationMs?: number; duration_ms?: number };
      errorPersisting?: { detected: boolean; durationMs?: number; duration_ms?: number; message?: string };
      idleSince: number | null;
      focusTrail: string[];
      stagnantCycles: number;
    };
    stability: {
      stable: Set<string>;
      volatile: Set<string>;
    };
    freshness?: FreshnessAssessment;
    lastDiffSummary?: DiffSummary | null;
  } | null;
  consumeAnomalies(): Array<{ type?: string; anomaly_type?: string; description: string; elementIds?: string[]; element_ids?: string[] }>;
  readFreshness?(): FreshnessAssessment;
  readDiffSummary?(): DiffSummary | null;
  ingestActionOutcome?(outcome: ActionOutcome): void;
}

/** Configuration for a goal execution. */
export interface GoalRunnerConfig {
  goal: string;
  maxSteps?: number;
  stepDelay?: number;
  taskTimeout?: number;
  maxConsecutiveFailures?: number;
  enableVision?: boolean;
  selfHeal?: boolean;
  selfHealMaxAttempts?: number;
  actCache?: ActCache;
  variables?: Record<string, string>;
  enableContextLazy?: boolean;
  visionMode?: "always" | "auto" | "never" | "local_first";
  distillContext?: boolean;
  /**
   * Skip the goal router's fast paths (which use native macOS input).
   * Set to true when running through a browser adapter where all input
   * must go through Playwright/CDP, not the real desktop keyboard.
   */
  skipRouter?: boolean;
  /**
   * Optional Cortex instance for always-on perception.
   * When provided, the goal-runner reads from the cortex's mental model
   * instead of calling getContext() on each step.
   */
  cortex?: Cortex | CortexProxy;
  /**
   * Dedicated validator configuration.
   * When provided, replaces inline post-action validation with the three-tier validator.
   */
  validator?: ValidatorConfig;
  /**
   * Enable speculative branching (WebTactix-inspired).
   * When true, the planner requests ranked alternatives alongside the primary
   * action. On failure, the next alternative is tried immediately (no LLM
   * roundtrip). On success, remaining alternatives are queued for backtracking.
   */
  speculative?: boolean;
  /**
   * Enable persistent LLM conversation thread across steps.
   * When true (default), the planner maintains a messages array and sends
   * context diffs instead of rebuilding full context each step.
   * Reduces token usage by 50-70% and gives the LLM memory across steps.
   */
  persistentThread?: boolean;
  /**
   * Per-step timeout in ms.
   * If a single step (action execution) exceeds this, it counts as a failure
   * and the loop moves to the next step. Default: 30000 (30s).
   */
  stepTimeout?: number;
  /**
   * Max steps without context fingerprint change before bailing out.
   * Catches cases where the agent keeps trying different actions but none
   * produce visible progress. Default: 10.
   */
  maxStepsWithoutProgress?: number;

  // ── Cognitive loop extensions ──────────────────────────────────────────

  /**
   * Workflow name for history scoping.
   * When provided, enables: history advisor queries, observation storage,
   * working memory persistence, and knowledge scoping.
   */
  workflowName?: string;
  /**
   * Enable milestone decomposition for complex goals.
   * When true (and maxSteps > 15), the system decomposes the goal into
   * advisory milestones before starting execution.
   */
  enableDecomposition?: boolean;
  /**
   * Enable notebook for cross-replan data persistence.
   * Default: true. The notebook records data discovered during execution
   * (prices, URLs, confirmation numbers) that persists across replans.
   */
  enableNotebook?: boolean;
  /**
   * Enable feasibility check before starting execution.
   * Default: true when enableDecomposition is true. One cheap LLM call
   * to assess whether the goal is achievable from the current state.
   */
  enableFeasibilityCheck?: boolean;

  // ── Replan-hardening flags (rollback-safe, default off) ───────────────────
  // The tier-replan system (triggerReplan, resetGlobalCounter, Tier 4
  // re-decomposition) landed in commits 96f5db0 + 68bf99d. These flags let
  // operators enable it progressively: A/B by hash, canary cohort, full on.

  /**
   * Master flag for the tier-replan cognitive loop (triggerReplan helper,
   * loop-detector reset on strategy change, per-tier metrics).
   * Default: false — preserves pre-commit-96f5db0 behavior.
   */
  enableTierReplan?: boolean;

  /**
   * Escalate `consecutiveFailures` when action succeeds but state hasn't
   * changed for 3+ steps AND verifyGoal still fails. Catches semantic loops
   * (navigate ↔ key_combo targeting the same URL) the hash-based loop
   * detector can't see. Requires `enableTierReplan`. Default: false.
   */
  enableSemanticStallEscalation?: boolean;

  /**
   * Enable Tier 4 full goal re-assessment (LLM re-decomposition with global
   * strategy-budget reset). Separate flag from tier 2/3 because it triggers
   * an additional LLM call and has a different risk profile. Requires
   * `enableTierReplan`. Default: false.
   */
  enableTier4Reassessment?: boolean;

  /**
   * Auto-execute pre_steps suggested by the feasibility check.
   * When true and the LLM emits `"open Chrome"` / `"launch Safari"` and
   * friends, the runner will actually type the app name via Spotlight.
   * Default: false — pre_steps are logged only. Turning this on types into
   * the user's real desktop; review the openAppActions regex before enabling.
   */
  enableFeasibilityPreSteps?: boolean;
}

/** Normalized result of a single action execution (Browser-Use pattern). */
export interface ActionResult {
  extractedContent?: string;
  error?: string;
  success: boolean;
  isDone?: boolean;
}

/** Result of a goal execution. */
export interface GoalResult {
  status: "achieved" | "failed" | "max_steps" | "timeout";
  summary: string;
  totalSteps: number;
  history: PlannerStepRecord[];
  metrics?: GoalMetrics;
  /** Signal to orchestrator: sub-agent recommends replanning or aborting. */
  escalation?: "replan" | "abort";
  /** Persistent conversation thread — full LLM message history for this goal run. */
  conversationThread?: Array<{ role: string; content: string }>;
}

/** Callbacks for goal execution — the adapter contract. */
export interface GoalRunnerCallbacks {
  getContext: () => Promise<ScreenContext>;
  /** Tier 1 context: URL, title, scroll position, text preview. ~5ms. */
  getContextTier1?: () => Promise<ScreenContext>;
  /** Tier 2 context: Interactive elements only (lightweight DOM extraction). ~50ms. */
  getContextTier2?: () => Promise<ScreenContext>;
  screenshot?: () => Promise<Buffer>;
  stateFingerprint?: () => string;
  waitForSettle?: (actionType: string) => Promise<void>;
  verifyGoal?: () => Promise<boolean>;
  executeAction?: (action: PlannedAction, context: ScreenContext) => Promise<boolean>;
  onStepPlanned?: (step: PlannedStep, index: number) => void;
  onStepExecuted?: (step: PlannedStep, index: number, success: boolean, error?: string) => void;
  onComplete?: (result: GoalResult) => void;
  /** Called after each action's validation (when validator config is set). */
  onValidation?: (result: ValidationResult, stepIndex: number) => void;
}
