/**
 * Graduated failure recovery — Browser-Use pattern.
 * Injects escalating nudges into the goal when consecutive failures occur.
 *
 * Extended with structured escalation levels for orchestrator integration:
 * - nudge: hint to the planner to try something different
 * - replan: signal to the orchestrator to decompose a new strategy
 * - abort: give up entirely
 */

// ── Structured escalation (for orchestrator) ─────────────────────────────────

export interface FailureEscalation {
  level: "nudge" | "replan" | "abort";
  message: string;
}

/**
 * Determine the escalation level based on failure and loop state.
 *
 * Thresholds are designed so that `replan` triggers BEFORE the loop-detector's
 * auto-fail at loopCount >= 5, giving the orchestrator a chance to try
 * a different approach.
 */
export function getFailureEscalation(
  consecutiveFailures: number,
  loopDetected: boolean,
  loopCount: number,
): FailureEscalation {
  // Abort: loop detector would auto-fail at this point anyway
  if (loopCount >= 5) {
    return {
      level: "abort",
      message: `Detected ${loopCount} loops — aborting to prevent infinite cycling.`,
    };
  }

  // Replan: trigger before loop auto-fail so orchestrator can try new strategy
  if (consecutiveFailures >= 3 || (loopDetected && loopCount >= 2)) {
    return {
      level: "replan",
      message: consecutiveFailures >= 3
        ? `${consecutiveFailures} consecutive failures — current approach isn't working.`
        : `Loop detected (${loopCount} cycles) — need a fundamentally different approach.`,
    };
  }

  // Nudge: gentle suggestion to try something different
  if (consecutiveFailures >= 2) {
    return {
      level: "nudge",
      message: "Previous action failed. Consider a different approach.",
    };
  }

  return { level: "nudge", message: "" };
}

// ── Queue-aware recovery ──────────────────────────────────────────────────────

import type { AlternativeQueue } from "./alternative-queue.js";
import type { PlannedAction } from "../types.js";
import type { CognitiveTrail } from "./cognitive-trail.js";
import type { Notebook } from "./notebook.js";
import type { CheckpointManager } from "./checkpoint-manager.js";
import type { HistoryAdvisor as HistoryAdvisorType } from "./history-advisor.js";

/**
 * Result of a replan attempt. `ok=false` means the strategy tracker is
 * globally exhausted — caller should fail the goal.
 */
export interface ReplanOutcome {
  ok: boolean;
  tier: ReplanTier;
  /** Prompt fragment to inject as loopWarning on the next plan step. */
  loopWarning: string | null;
  /** If true, notebook was restored from a checkpoint (Tier 3). */
  backtracked: boolean;
  /** If true, caller should re-run milestone decomposition (Tier 4). */
  needsRedecompose: boolean;
}

export interface TriggerReplanArgs {
  reason: "wrong_approach" | "reactive_failure";
  stepIndex: number;
  consecutiveFailures: number;
  currentMilestone: string;
  strategyTracker: import("./strategy-tracker.js").StrategyTracker;
  loopDetector: import("./loop-detector.js").LoopDetector;
  checkpointManager: CheckpointManager;
  notebook: Notebook | null;
  cognitiveTrail: CognitiveTrail;
  historyAdvisor: typeof HistoryAdvisorType;
  cel: unknown; // Cel binding — passed through to HistoryAdvisor
  goal: string;
  workflowName?: string;
  failureDetail?: string;
  /** Mutable metrics slice — this function increments the tier counters. */
  metrics: {
    tier2Replans?: number;
    tier3Backtracks?: number;
    tier4Reassessments?: number;
    strategyExhaustedEvents?: number;
  };
}

/**
 * Shared replan orchestration for both proactive (wrong_approach) and reactive
 * (consecutive failures) paths. Handles strategy registration, ephemeral-state
 * reset, checkpoint restore (Tier 3), history advice query, and tier-4 signal.
 *
 * Side effects (by design):
 *   - Mutates strategyTracker (records outcome + registers new strategy)
 *   - Mutates loopDetector (resetForNewStrategy)
 *   - Mutates notebook on Tier 3 (clear + restore from checkpoint)
 *   - Appends entries to cognitiveTrail
 *   - Increments caller-provided metrics counters
 */
export async function triggerReplan(args: TriggerReplanArgs): Promise<ReplanOutcome> {
  const {
    reason, stepIndex, consecutiveFailures, currentMilestone,
    strategyTracker, loopDetector, checkpointManager, notebook, cognitiveTrail,
    historyAdvisor, cel, goal, workflowName, failureDetail, metrics,
  } = args;

  // `wrong_approach` from the LLM is itself a failure signal — it means the
  // current strategy is hopeless even if individual actions are still
  // "succeeding". Tier computation would otherwise only escalate on
  // consecutiveFailures >= 3, which misses this case entirely. Treat the
  // proactive signal as the minimum escalation needed to enter tier 2.
  const escalatedFailures = reason === "wrong_approach"
    ? Math.max(consecutiveFailures, 3)
    : consecutiveFailures;
  const tier = getReplanTier(escalatedFailures, strategyTracker, loopDetector, currentMilestone);

  // Global exhaustion — tier 4 in name, but we cannot even register a new
  // strategy. Caller should terminate the goal.
  if (tier === 4 || !strategyTracker.canReplanGlobal()) {
    metrics.strategyExhaustedEvents = (metrics.strategyExhaustedEvents ?? 0) + 1;
    cognitiveTrail.add(stepIndex, "REPLAN", `Tier 4: strategy exhausted globally — re-decomposition required`);
    metrics.tier4Reassessments = (metrics.tier4Reassessments ?? 0) + 1;
    return { ok: true, tier: 4, loopWarning: null, backtracked: false, needsRedecompose: true };
  }

  if (tier < 2) {
    // Nothing to escalate — caller handles Tier 1 via getFailureNudge.
    return { ok: true, tier, loopWarning: null, backtracked: false, needsRedecompose: false };
  }

  // Record failure of current strategy (if any)
  const currentId = strategyTracker.currentStrategy(currentMilestone);
  if (currentId) {
    const outcomeReason = reason === "wrong_approach"
      ? "LLM flagged wrong_approach"
      : failureDetail ?? `${consecutiveFailures} consecutive failures`;
    strategyTracker.recordOutcome(currentId, "failed", outcomeReason, stepIndex);
  }

  // Register next strategy and reset ephemeral state
  const prefix = reason === "wrong_approach" ? "approach" : "reactive";
  strategyTracker.register(currentMilestone, `${prefix}-${stepIndex}`);
  loopDetector.resetForNewStrategy();

  cognitiveTrail.add(
    stepIndex, "REPLAN",
    `Tier ${tier} ${reason === "wrong_approach" ? "proactive" : "reactive"} replan — ${strategyTracker.getFailedStrategies(currentMilestone).length} failed strategies on "${currentMilestone}"${reason === "reactive_failure" ? ` after ${consecutiveFailures} failures` : ""}`,
  );

  // Inject failed strategies into next prompt
  const failedStrats = strategyTracker.getFailedStrategies(currentMilestone);
  let loopWarning: string | null = failedStrats.length > 0
    ? `REPLAN: These approaches FAILED: ${failedStrats.join("; ")}. You MUST try a fundamentally different approach.`
    : null;

  // Past-experience advice (symmetric across both paths)
  try {
    const failureAdvice = await historyAdvisor.queryForReplan(
      cel as never, goal, failedStrats.join("; ") || (failureDetail ?? reason), workflowName,
    );
    if (failureAdvice) {
      loopWarning = (loopWarning ?? "") + `\n\nPAST EXPERIENCE:\n${failureAdvice}`;
    }
  } catch { /* cel-store not available — skip */ }

  // Tier 3: backtrack to checkpoint
  let backtracked = false;
  if (tier === 3) {
    const checkpoint = checkpointManager.getPrevious();
    if (checkpoint && notebook) {
      notebook.clear();
      notebook.restoreFromSnapshot(checkpoint.notebookSnapshot);
      backtracked = true;
      cognitiveTrail.add(
        stepIndex, "REPLAN",
        `Backtracked to checkpoint "${checkpoint.milestone}" at step ${checkpoint.stepIndex}`,
      );
    }
  }

  if (tier === 2) metrics.tier2Replans = (metrics.tier2Replans ?? 0) + 1;
  if (tier === 3) metrics.tier3Backtracks = (metrics.tier3Backtracks ?? 0) + 1;

  return { ok: true, tier, loopWarning, backtracked, needsRedecompose: false };
}

/**
 * Attempt to recover from a failure using the alternative queue.
 * Returns an alternative action if one is available and fresh, or null
 * to fall through to LLM replanning.
 */
export function tryQueueRecovery(
  queue: AlternativeQueue,
  currentContextHash: string,
): PlannedAction | null {
  // Prune stale alternatives from a different context
  queue.pruneStale(currentContextHash);

  const alt = queue.pop();
  if (alt) {
    return alt.action;
  }
  return null;
}

// ── Tiered replanning (cognitive loop) ────────────────────────────────────────

import type { StrategyTracker } from "./strategy-tracker.js";
import type { LoopDetector } from "./loop-detector.js";

/**
 * Replan tier for the cognitive loop:
 * 1 = nudge (1-2 failures, same strategy)
 * 2 = new strategy (3+ failures or proactive reassessment)
 * 3 = backtrack to checkpoint (strategy exhausted at current milestone)
 * 4 = full goal re-assessment (multiple milestones failed)
 */
export type ReplanTier = 1 | 2 | 3 | 4;

/**
 * Determine the replan tier based on failure state and strategy tracker.
 * Used by the cognitive loop to decide how aggressively to replan.
 */
export function getReplanTier(
  consecutiveFailures: number,
  strategyTracker: StrategyTracker,
  loopDetector: LoopDetector,
  currentMilestone?: string,
): ReplanTier {
  // Tier 4: strategy tracker exhausted globally
  if (!strategyTracker.canReplanGlobal()) {
    return 4;
  }

  // Tier 3: current milestone's strategies exhausted
  if (currentMilestone && !strategyTracker.canReplan(currentMilestone)) {
    return 3;
  }

  // Tier 2: enough failures to warrant a new strategy
  if (consecutiveFailures >= 3 || (loopDetector.shouldAutoFail())) {
    return 2;
  }

  // Tier 1: nudge
  return 1;
}

// ── Conditional-edge router (LangGraph-style) ────────────────────────────────

/**
 * Semantic replan decision returned by {@link replanRouter}. One-to-one map with
 * {@link ReplanTier} but named for declarative routing: a graph executor can
 * branch on the returned key without knowing the numeric tier.
 */
export type ReplanDecision = "nudge" | "new_strategy" | "backtrack" | "reassess";

/**
 * Pure routing function: given the current failure signals, return which
 * replan branch to take. This is the conditional edge of the cognitive loop —
 * the same inputs always produce the same decision, so it's trivial to test.
 *
 * Keep this in lockstep with {@link getReplanTier}; they're the same logic
 * viewed from two angles (semantic label vs tier number).
 */
export function replanRouter(
  consecutiveFailures: number,
  strategyTracker: StrategyTracker,
  loopDetector: LoopDetector,
  currentMilestone?: string,
): ReplanDecision {
  const tier = getReplanTier(consecutiveFailures, strategyTracker, loopDetector, currentMilestone);
  switch (tier) {
    case 1: return "nudge";
    case 2: return "new_strategy";
    case 3: return "backtrack";
    case 4: return "reassess";
  }
}

// ── Original nudge function (backward compat) ────────────────────────────────

/**
 * Get a nudge message to inject into the goal based on failure count.
 * Returns null if no nudge is needed.
 */
export function getFailureNudge(consecutiveFailures: number): string | null {
  if (consecutiveFailures >= 5) {
    return "\n\nCRITICAL: You have failed 5+ times in a row. You MUST either:\n" +
      "1. Output a completely different action type than what you've been trying\n" +
      "2. Output a 'done' action if the goal is actually achieved\n" +
      "3. Output a 'fail' action if the goal is truly impossible";
  }
  if (consecutiveFailures >= 3) {
    return "\n\nWARNING: Multiple consecutive failures. Revise your plan — " +
      "list 3 alternative approaches and pick the most promising one.";
  }
  if (consecutiveFailures >= 2) {
    return "\n\nNote: The previous action failed. Consider a different approach.";
  }
  return null;
}
