/**
 * Replay Executor — Deterministic Workflow Replay
 *
 * Replays a cached multi-step workflow sequence, executing each step
 * against the live UI with optional self-healing.
 *
 * Returns GoalResult on full success, or null to signal that replay
 * should be abandoned and the caller should fall back to normal planning.
 */

import type { Cel } from "../cel-bindings.js";
import type { PlannedAction, GoalMetrics, WorkflowStep } from "../types.js";
import type { GoalRunnerCallbacks, GoalResult } from "../goal-runner.js";
import { executeAction, type AdapterRegistry } from "../action-executor.js";
import { plannedToWorkflowAction } from "../goal-runner.js";
import { selfHeal, type SelfHealOptions } from "../self-healer.js";
import type { AgentCache, AgentCacheEntry, CachedStep } from "../cache/agent-cache.js";

/** Options for workflow replay. */
export interface ReplayOptions {
  /** Enable self-healing for failed steps. Default: true. */
  selfHeal?: boolean;
  /** Self-heal options. */
  selfHealOptions?: SelfHealOptions;
  /** The agent cache instance (for updating cache on self-heal). */
  agentCache?: AgentCache;
  /** Adapter registry for custom actions. */
  adapters?: AdapterRegistry;
  /** Settle time defaults per action type. */
  settleMs?: Record<string, number>;
}

const DEFAULT_SETTLE_MS: Record<string, number> = {
  click: 800,
  custom: 500,
  type: 500,
  key: 200,
  key_combo: 200,
  scroll: 200,
  wait: 0,
};

/**
 * Replay a cached workflow against the live UI.
 *
 * For each step:
 * 1. Get current state fingerprint (if available)
 * 2. Execute the cached action
 * 3. If fails → self-heal → update cache → continue
 * 4. If self-heal fails → return null (abandon replay)
 * 5. After all steps → verify goal if callback exists
 *
 * @returns GoalResult on success, null if replay should be abandoned
 */
export async function replayGoal(
  entry: AgentCacheEntry,
  callbacks: GoalRunnerCallbacks,
  cel: Cel,
  variables: Record<string, string> = {},
  options?: ReplayOptions,
): Promise<GoalResult | null> {
  const startTime = Date.now();
  const doSelfHeal = options?.selfHeal ?? true;
  const adapters = options?.adapters;
  const settleDefaults = options?.settleMs ?? DEFAULT_SETTLE_MS;

  const metrics: GoalMetrics = {
    totalMs: 0,
    contextExtractionMs: 0,
    llmCalls: 0,
    visionCalls: 0,
    errorCount: 0,
    stateChanges: 0,
    loopWarnings: 0,
    cacheHits: entry.steps.length,
  };

  for (let i = 0; i < entry.steps.length; i++) {
    const step = entry.steps[i];
    const action = step.action;

    // Skip done/fail in cached sequence
    if (action.type === "done" || action.type === "fail") continue;

    // Get context for action execution
    const ctxStart = Date.now();
    const context = await callbacks.getContext();
    metrics.contextExtractionMs += Date.now() - ctxStart;

    // Execute cached action
    let success = false;
    try {
      if (callbacks.executeAction) {
        success = await callbacks.executeAction(action, context);
      } else {
        const wfAction = plannedToWorkflowAction(action);
        if (!wfAction) continue;
        const wfStep: WorkflowStep = {
          id: `replay-${i}`,
          description: `Replay step ${i + 1}/${entry.steps.length}`,
          action: wfAction,
        };
        success = await executeAction(cel, wfStep, context, adapters);
      }
    } catch (e) {
      // Step failed — attempt self-heal
      if (doSelfHeal) {
        const healResult = await selfHeal(
          action,
          String(e),
          callbacks,
          cel,
          entry.goal,
          [], // Minimal history for replay self-heal
          options?.selfHealOptions,
        );

        if (healResult) {
          metrics.llmCalls++;
          try {
            if (callbacks.executeAction) {
              success = await callbacks.executeAction(healResult.repairedAction, healResult.newContext);
            } else {
              const wfAction = plannedToWorkflowAction(healResult.repairedAction);
              if (wfAction) {
                const wfStep: WorkflowStep = {
                  id: `replay-healed-${i}`,
                  description: `Self-healed replay step ${i + 1}`,
                  action: wfAction,
                };
                success = await executeAction(cel, wfStep, healResult.newContext, adapters);
              }
            }

            // Update cache with healed step
            if (success && options?.agentCache) {
              const postFP = callbacks.stateFingerprint?.() ?? "";
              const healedStep: CachedStep = {
                action: healResult.repairedAction,
                preFingerprint: step.preFingerprint,
                postFingerprint: postFP,
                variables,
              };
              await options.agentCache.repairStep(entry.key, i, healedStep).catch(() => {});
              metrics.selfHealSuccesses = (metrics.selfHealSuccesses ?? 0) + 1;
            }
          } catch {
            success = false;
          }
        }
      }

      if (!success) {
        // Replay failed — abandon and fall back to normal planning
        return null;
      }
    }

    if (!success) return null;

    // Wait for UI to settle
    if (callbacks.waitForSettle) {
      await callbacks.waitForSettle(action.type);
    } else {
      const ms = settleDefaults[action.type] ?? 500;
      if (ms > 0) await new Promise((r) => setTimeout(r, ms));
    }
  }

  // Verify goal if callback exists
  if (callbacks.verifyGoal) {
    const verified = await callbacks.verifyGoal().catch(() => false);
    if (!verified) {
      return null; // Verification failed — fall back to planning
    }
  }

  metrics.totalMs = Date.now() - startTime;

  return {
    status: "achieved",
    summary: "Goal achieved (replayed from workflow cache)",
    totalSteps: entry.steps.length,
    history: entry.steps.map((s, i) => ({
      step_index: i,
      action: s.action,
      success: true,
    })),
    metrics,
  };
}
