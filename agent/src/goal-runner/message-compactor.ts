/**
 * Message compaction — keeps history manageable.
 *
 * Two mechanisms (inspired by WebTactix's "partially_done"):
 * 1. Periodic compaction: every N steps, summarize completed work
 * 2. Threshold compaction: when history exceeds character limit
 *
 * Unlike simple truncation, compaction preserves WHAT was accomplished
 * by building a summary of successful actions + extracted data.
 */

import type { PlannedAction, PlannerStepRecord } from "../types.js";

const COMPACTION_CHAR_THRESHOLD = 40_000;
const COMPACT_EVERY_N_STEPS = 15;
const RECENT_STEPS_TO_KEEP = 8;

/**
 * Build a meaningful summary from compacted steps.
 * Extracts key information: successful navigations, clicks, extracted data.
 */
function summarizeCompactedSteps(steps: PlannerStepRecord[]): string {
  const parts: string[] = [];
  const successes = steps.filter((s) => s.success);
  const failures = steps.filter((s) => !s.success);

  for (const step of successes) {
    const action = step.action as PlannedAction;
    switch (action.type) {
      case "custom":
        if (action.adapter === "browser" && action.action === "navigate") {
          parts.push(`Navigated to ${(action.params as any)?.url ?? "page"}`);
        }
        break;
      case "click":
        if (step.element_label) {
          parts.push(`Clicked "${step.element_label}"`);
        }
        break;
      case "type":
        parts.push(`Typed "${action.text?.slice(0, 30) ?? ""}"`);
        break;
      case "extract":
        parts.push(`Extracted: ${action.data?.slice(0, 100) ?? ""}`);
        break;
      case "scroll":
        // Skip scroll actions — not informative
        break;
      default:
        break;
    }
  }

  // Limit to most important actions
  const summary = parts.slice(0, 8).join(" → ");
  const failNote = failures.length > 0 ? ` (${failures.length} failed attempts)` : "";
  return `[Checkpoint: ${summary}${failNote}]`;
}

/**
 * Compact history in-place if it exceeds thresholds.
 * Preserves a meaningful checkpoint summary of what was accomplished,
 * plus the most recent steps for context.
 * Returns true if compaction was performed.
 */
export function compactHistoryIfNeeded(
  history: PlannerStepRecord[],
  stepIndex: number,
): boolean {
  if (history.length <= RECENT_STEPS_TO_KEEP) return false;

  const historySize = JSON.stringify(history).length;
  const shouldCompact =
    historySize > COMPACTION_CHAR_THRESHOLD ||
    (stepIndex > 0 && stepIndex % COMPACT_EVERY_N_STEPS === 0);

  if (!shouldCompact) return false;

  const recent = history.slice(-RECENT_STEPS_TO_KEEP);
  const compacted = history.slice(0, -RECENT_STEPS_TO_KEEP);
  const checkpointSummary = summarizeCompactedSteps(compacted);

  history.length = 0;
  history.push({
    step_index: -1,
    action: { type: "done", summary: checkpointSummary } as PlannedAction,
    success: true,
  });
  history.push(...recent);

  return true;
}
