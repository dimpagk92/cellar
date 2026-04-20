/**
 * Runtime Kernel types — canonical interfaces for the unified execution model.
 *
 * These types define the contract between the runtime kernel and adapters.
 * The kernel owns the route→execute→verify loop; adapters provide capabilities.
 *
 * License: MIT
 */

import type {
  ScreenContext,
  PlannedAction,
  FreshnessAssessment,
  ActionOutcome,
} from "../types.js";
import type {
  StrategyAttempt,
  StrategyRoute,
  AmbiguityAssessment,
} from "../strategy-router.js";

// ── Adapter Capability Contract ─────────────────────────────────────────────

/**
 * What an adapter must provide to the runtime kernel.
 *
 * Adapters own execution primitives (how to click, type, navigate).
 * The kernel owns policy (when to escalate, refresh, or stop).
 */
export interface AdapterCapabilities {
  /** Read current context from the adapter (full extraction). */
  readContext(): Promise<ScreenContext>;

  /** Execute action via structured route (CSS selector / backend node / a11y ID). */
  executeStructured(action: PlannedAction, context: ScreenContext): Promise<boolean>;

  /**
   * Resolve an action semantically (a11y tree / LLM-guided disambiguation).
   * Returns a resolved action with a concrete target, or null if resolution fails.
   */
  resolveSemantic(action: PlannedAction, context: ScreenContext): Promise<PlannedAction | null>;

  /** Capture a screenshot for vision-assist fallback. */
  captureScreenshot(): Promise<Buffer>;

  /** Optional: post-navigate cleanup (e.g., dismiss cookie banners). */
  postNavigateCleanup?(): Promise<void>;
}

// ── Kernel Action Outcome ───────────────────────────────────────────────────

/**
 * Fully-populated action outcome produced by the kernel.
 *
 * Extends the base ActionOutcome with fields that only the kernel can populate:
 * timing, terminal status, refresh tracking, escalation history.
 *
 * All runtime consumers (goal-runner, cortex, benchmarks, live-view) use this shape.
 */
export interface KernelActionOutcome {
  /** The action type that was executed (click, type, set_value, etc.). */
  action: string;
  /** The strategy route used for execution. Always populated by the kernel. */
  route: StrategyRoute;
  /** Whether the adapter reported successful execution. */
  success: boolean;
  /** Whether post-action verification confirmed the action landed. */
  verified: boolean;
  /** Whether verification contradicted the expected outcome. */
  contradiction: boolean;
  /** Human-readable summary of side effects (cross-app shift, no diff, etc.). */
  sideEffectSummary?: string;
  /** Timestamp when the action completed. */
  timestamp: number;
  /** Wall-clock duration of the entire route→execute→verify cycle. */
  durationMs: number;
  /** Whether the escalation ceiling was reached (terminal failure). */
  terminal: boolean;
  /** Whether a context refresh was triggered during this action's execution. */
  refreshTriggered: boolean;
  /** Route confidence from the strategy router. */
  confidence: number;
  /** Full escalation history: which routes were attempted and their results. */
  routeAttempts: StrategyAttempt[];
}

// ── Kernel Input ────────────────────────────────────────────────────────────

/**
 * Everything the kernel needs to execute a single planned action.
 */
export interface KernelExecutionInput {
  /** The action to execute. */
  action: PlannedAction;
  /** Current screen context (before execution). */
  context: ScreenContext;
  /** Adapter capabilities for this execution. */
  capabilities: AdapterCapabilities;
  /** Read current freshness assessment from the cortex (or return null). */
  readFreshness: () => FreshnessAssessment | null;
  /** Feed the action outcome back to the cortex. */
  ingestOutcome: (outcome: ActionOutcome) => void;
  /** Pre-computed ambiguity assessment for this action (optional, adapter-specific). */
  ambiguity?: AmbiguityAssessment | null;
  /** Prior route attempts for this action (mutable — kernel appends to this). */
  attempts?: StrategyAttempt[];
  /** The high-level goal (used for ambiguity assessment on retry). */
  goal?: string;
  /** Structured logging callback for route decisions (legacy — prefer onEvent). */
  logRoute?: (msg: string, meta?: Record<string, unknown>) => void;
  /** Kernel event callback — receives structured events for live-view, benchmarks, analytics. */
  onEvent?: (event: KernelEvent) => void;
  /** Re-assess ambiguity for this action given updated context (optional). */
  assessAmbiguity?: (action: PlannedAction, context: ScreenContext) => AmbiguityAssessment | null;
}

// ── Kernel Events ──────────────────────────────────────────────────────────

/** Event types emitted by the kernel during the route→execute→verify pipeline. */
export type KernelEventType =
  | "route_selected"
  | "refresh_triggered"
  | "execution_result"
  | "verification_result"
  | "terminal_failure"
  | "side_effect"
  | "trusted_execution";

/**
 * A structured event emitted by the kernel at key decision points.
 *
 * Consumers (live-view, benchmarks, debug surfaces) subscribe via the
 * `onEvent` callback in KernelExecutionInput. This replaces the previous
 * pattern of monkey-patching console.info and parsing log lines.
 */
export interface KernelEvent {
  type: KernelEventType;
  /** The action being executed. */
  action: string;
  /** The route involved in this event. */
  route: StrategyRoute;
  /** Timestamp when the event occurred. */
  timestamp: number;
  /** Route confidence from the strategy router. */
  confidence?: number;
  /** Whether the action/verification succeeded. */
  success?: boolean;
  /** Whether verification detected a meaningful change. */
  verified?: boolean;
  /** Freshness state at the time of the event. */
  freshnessState?: string | null;
  /** Staleness causes (time, event, confidence, verification). */
  causes?: string[];
  /** Human-readable reason from the strategy router. */
  reason?: string;
  /** Side-effect summary (if any). */
  sideEffectSummary?: string;
  /** Whether this is a terminal failure. */
  terminal?: boolean;
  /** Additional details for debugging. */
  details?: Record<string, unknown>;
}

// ── Verification ────────────────────────────────────────────────────────────

/** Result of post-action verification. */
export interface VerificationResult {
  /** Whether verification detected a meaningful change. */
  changed: boolean;
  /** Whether a set_value action's value was confirmed in the target element. */
  valueConfirmed: boolean;
  /** Whether the action caused a cross-app or cross-window shift. */
  crossAppShift: boolean;
  /** Human-readable side-effect summary (if any). */
  sideEffectSummary?: string;
}

/**
 * Convert a KernelActionOutcome to the base ActionOutcome for cortex ingestion.
 */
export function toActionOutcome(kernel: KernelActionOutcome): ActionOutcome {
  return {
    action: kernel.action,
    route: kernel.route,
    success: kernel.success,
    verified: kernel.verified,
    contradiction: kernel.contradiction,
    sideEffectSummary: kernel.sideEffectSummary,
    timestamp: kernel.timestamp,
  };
}
