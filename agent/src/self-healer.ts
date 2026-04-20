/**
 * Self-Healing Repair Loop
 *
 * When an action fails at execution time (element not found, click intercepted,
 * DOM shifted, etc.), this module re-extracts context and re-plans the specific
 * step using the fresh context + failure information.
 *
 * Inspired by Stagehand v3's self-healing cache mechanism, adapted for CEL's
 * adapter-agnostic architecture.
 *
 * Enhanced with healing metadata for cross-run learning and plan-awareness:
 * - healingContext tracks what failed, why, and whether the screen shifted
 * - HistoryAdvisor integration queries past healing patterns from cel-store
 */

import type { Planner } from "./interfaces/planner.js";
import type { KnowledgeStore } from "./interfaces/knowledge-store.js";
import type {
  ScreenContext,
  PlannedStep,
  PlannedAction,
  PlannerStepRecord,
} from "./types.js";
import type { GoalRunnerCallbacks } from "./goal-runner.js";
import { HistoryAdvisor } from "./goal-runner/history-advisor.js";
import { contextFingerprint } from "./goal-runner/helpers.js";

/** Metadata about what changed during healing. */
export interface HealingContext {
  /** The original action that failed. */
  failedAction: PlannedAction;
  /** Why the original action failed. */
  failureReason: string;
  /** Whether the screen changed between the failure and the fresh context. */
  contextShifted: boolean;
  /** Human-readable description of the repair strategy chosen by the planner. */
  repairDescription: string;
}

/** Result of a successful self-heal attempt. */
export interface SelfHealResult {
  /** The repaired action that succeeded. */
  repairedAction: PlannedAction;
  /** The fresh context captured during healing. */
  newContext: ScreenContext;
  /** Which attempt succeeded (1-based). */
  attemptNumber: number;
  /** Metadata about the failure and repair for downstream awareness. */
  healingContext: HealingContext;
}

/** Options for the self-healing process. */
export interface SelfHealOptions {
  /** Maximum repair attempts before giving up. Default: 2. */
  maxAttempts?: number;
  /** Enable vision for repair planning when screenshot is available. Default: true. */
  enableVision?: boolean;
  /** Knowledge store for querying past healing patterns. */
  knowledgeStore?: KnowledgeStore;
  /** Workflow name for scoping knowledge store queries. */
  workflowName?: string;
  /** Fingerprint of the context at the time of failure (for shift detection). */
  originalContextFingerprint?: number;
}

/**
 * Attempt to self-heal a failed action by re-extracting context and re-planning.
 *
 * Flow:
 * 1. Capture fresh context via callbacks.getContext()
 * 2. Query past healing patterns from knowledge store (if available)
 * 3. Build a repair goal that includes the failure context + past advice
 * 4. Call the planner with the repair goal + fresh context + recent history
 * 5. Validate grounding of the new step
 * 6. Return the repaired action with healing metadata, or null if all attempts fail
 *
 * @returns SelfHealResult on success, null if repair is not possible
 */
export async function selfHeal(
  failedAction: PlannedAction,
  failureReason: string,
  callbacks: GoalRunnerCallbacks,
  cel: Planner,
  goal: string,
  history: PlannerStepRecord[],
  options?: SelfHealOptions,
): Promise<SelfHealResult | null> {
  const maxAttempts = options?.maxAttempts ?? 2;
  const enableVision = options?.enableVision ?? true;

  // Query past healing patterns from knowledge store (FTS5 — instant, no LLM call)
  let pastAdvice = "";
  if (options?.knowledgeStore && options?.workflowName) {
    try {
      const advice = await HistoryAdvisor.queryForReplan(
        options.knowledgeStore, goal, failureReason, options.workflowName,
      );
      if (advice) pastAdvice = `\n\n${advice}`;
    } catch {
      // Non-critical — proceed without past advice
    }
  }

  for (let attempt = 1; attempt <= maxAttempts; attempt++) {
    try {
      // 1. Capture fresh context
      const freshContext = await callbacks.getContext();

      // 2. Detect context shift
      const freshFP = contextFingerprint(freshContext);
      const contextShifted = options?.originalContextFingerprint != null
        && freshFP !== options.originalContextFingerprint;

      // 3. Build repair goal — tells the LLM what failed and asks for alternative
      const failedActionDesc = describeAction(failedAction);
      const repairGoal =
        `${goal}\n\n` +
        `REPAIR NEEDED: The previous action "${failedActionDesc}" failed.\n` +
        `Reason: ${failureReason}\n` +
        `Attempt ${attempt} of ${maxAttempts}.\n` +
        `You MUST choose a different element or approach. Do NOT repeat the failed action.` +
        (contextShifted ? `\nNOTE: The screen has changed since the failure — re-evaluate available elements.` : "") +
        pastAdvice;

      // 4. Use only last 3 history entries for repair context (keep it focused)
      const recentHistory = history.slice(-3);

      // Add the failure as a history entry so the planner sees it
      const repairHistory: PlannerStepRecord[] = [
        ...recentHistory,
        {
          step_index: history.length,
          action: failedAction,
          success: false,
          error: failureReason,
        },
      ];

      // 5. Plan with vision if available and sparse context
      let repairedStep: PlannedStep;

      const actionableCount = freshContext.elements.filter(
        (e) =>
          e.state.visible &&
          e.state.enabled &&
          e.actions &&
          e.actions.length > 0,
      ).length;

      const useVision =
        enableVision &&
        !!callbacks.screenshot &&
        (actionableCount < 5 || attempt >= 2);

      if (useVision && callbacks.screenshot) {
        try {
          const buf = await callbacks.screenshot();
          const base64 = buf.toString("base64");
          repairedStep = await cel.planStepWithVision(
            repairGoal,
            freshContext,
            base64,
            repairHistory,
          );
        } catch {
          // Vision failed — fall through to text-only
          repairedStep = await cel.planStep(
            repairGoal,
            freshContext,
            repairHistory,
          );
        }
      } else {
        repairedStep = await cel.planStep(
          repairGoal,
          freshContext,
          repairHistory,
        );
      }

      // 6. Reject if planner returned the same failed action
      if (isSameAction(repairedStep.action, failedAction)) {
        continue; // Try again with next attempt
      }

      // 7. Reject "done" or "fail" responses — we want an action
      if (
        repairedStep.action.type === "done" ||
        repairedStep.action.type === "fail"
      ) {
        continue;
      }

      // 8. Validate grounding (element exists in fresh context)
      const groundingOk = validateRepairGrounding(
        repairedStep.action,
        freshContext,
      );
      if (!groundingOk) {
        continue;
      }

      return {
        repairedAction: repairedStep.action,
        newContext: freshContext,
        attemptNumber: attempt,
        healingContext: {
          failedAction,
          failureReason,
          contextShifted,
          repairDescription: repairedStep.reasoning ?? describeAction(repairedStep.action),
        },
      };
    } catch {
      // Planning itself failed — try next attempt
      continue;
    }
  }

  return null; // All attempts exhausted
}

/** Check if a repaired action's target exists in the context. */
function validateRepairGrounding(
  action: PlannedAction,
  context: ScreenContext,
): boolean {
  if (action.type === "click" || action.type === "type") {
    return context.elements.some(
      (el) =>
        el.id === action.target_id && el.state.enabled && el.state.visible,
    );
  }
  // Non-targeted actions (key, scroll, wait) are always grounded
  return true;
}

/** Check if two actions are effectively the same (to avoid repeating failures). */
function isSameAction(a: PlannedAction, b: PlannedAction): boolean {
  if (a.type !== b.type) return false;
  switch (a.type) {
    case "click":
      return (b as { target_id: string }).target_id === a.target_id;
    case "type":
      return (
        (b as { target_id: string }).target_id === a.target_id &&
        (b as { text: string }).text === a.text
      );
    case "key":
      return (b as { key: string }).key === a.key;
    case "key_combo":
      return (
        JSON.stringify((b as { keys: string[] }).keys) ===
        JSON.stringify(a.keys)
      );
    default:
      return false;
  }
}

/** Human-readable description of a planned action. */
export function describeAction(action: PlannedAction): string {
  switch (action.type) {
    case "click":
      return `click on element ${action.target_id}`;
    case "type":
      return `type "${action.text}" into element ${action.target_id}`;
    case "set_value":
      return `set value "${action.value}" on element ${action.target_id}`;
    case "key":
      return `press key ${action.key}`;
    case "key_combo":
      return `press key combo ${action.keys.join("+")}`;
    case "scroll":
      return `scroll (${action.dx}, ${action.dy})`;
    case "drag":
      return `drag from (${action.from_x},${action.from_y}) to (${action.to_x},${action.to_y})`;
    case "wait":
      return `wait ${action.ms}ms`;
    case "custom":
      return `custom action ${action.adapter}.${action.action}`;
    case "done":
      return `done: ${action.summary}`;
    case "fail":
      return `fail: ${action.reason}`;
    case "extract":
      return `extract "${action.goal}": ${action.data.slice(0, 50)}`;
    case "act":
      return `act: ${action.instruction}`;
    case "batch":
      return `batch of ${action.actions.length} actions`;
    case "notebook_writes":
      return `notebook write (no-op)`;
    default:
      return `unknown action`;
  }
}
