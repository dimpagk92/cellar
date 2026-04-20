/**
 * Goal Runner — modular architecture.
 * Re-exports everything for backward compatibility.
 */

export type { GoalRunnerConfig, GoalResult, GoalRunnerCallbacks, ActionResult } from "./config.js";
export { LoopDetector } from "./loop-detector.js";
export type { LoopSignal, LoopSeverity } from "./loop-detector.js";
export { distillContext, isActionableType } from "./context-distiller.js";
export { shouldUseVision } from "./vision-manager.js";
export type { VisionMode } from "./vision-manager.js";
export { compactHistoryIfNeeded } from "./message-compactor.js";
export { getFailureNudge, getFailureEscalation, tryQueueRecovery } from "./failure-recovery.js";
export type { FailureEscalation } from "./failure-recovery.js";
export { AlternativeQueue, type QueuedAlternative } from "./alternative-queue.js";
export { validateGrounding, validatePostAction } from "./validation.js";
export type { ValidationResult, ValidatorConfig, ValidateActionParams } from "./validator.js";
export { validateAction } from "./validator.js";
export type { VisionRoute, VisionTaskType } from "./vision-router.js";
export { routeVision } from "./vision-router.js";
export { planStep } from "./planner.js";
export {
  sleep,
  simpleHash,
  contextFingerprint,
  actionSignature,
  isTransitionAction,
  DEFAULT_SETTLE_MS,
  plannedToWorkflowAction,
  cachedStepMatchesContext,
  extractJsonObject,
} from "./helpers.js";

// ── Cognitive loop modules ────────────────────────────────────────────────────
export { CognitiveTrail, type TrailEntry, type TrailEntryType } from "./cognitive-trail.js";
export { Notebook, type NotebookEntry, type NotebookCategory } from "./notebook.js";
export { StrategyTracker, type StrategyAttempt, type StrategyOutcome } from "./strategy-tracker.js";
export { CheckpointManager, type Checkpoint } from "./checkpoint-manager.js";
export { HistoryAdvisor } from "./history-advisor.js";
export { CortexBridge, type CortexSignal, type CortexSignalType } from "./cortex-bridge.js";
export { getReplanTier, triggerReplan, replanRouter, type ReplanTier, type ReplanDecision, type ReplanOutcome, type TriggerReplanArgs } from "./failure-recovery.js";
export {
  type GoalState,
  type PersistentState,
  type EphemeralState,
  type CognitivePhase,
  type Reducer,
  initialPersistentState,
  initialEphemeralState,
  resetEphemeralForReplan,
  appendReducer,
  mergeDictReducer,
  overwriteReducer,
} from "./state.js";
export { type TrailEvent, type TrailListener } from "./cognitive-trail.js";
export {
  canaryCohort,
  resolveCanaryPercentage,
  applyCanaryOverride,
  type CanaryCohort,
} from "./canary.js";
export {
  ReplanEventEmitter,
  type ReplanEvent,
  type ReplanEventType,
  type ReplanListener,
  type TierEvent,
  type StallEvent,
  type Tier4CapEvent,
  type PreflightEvent,
} from "./replan-events.js";
