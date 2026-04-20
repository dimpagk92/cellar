/**
 * Grounding validation + post-action validation.
 * Ensures planned steps reference real elements and actions actually worked.
 */

import type { PlannedStep, ScreenContext } from "../types.js";

// Re-export the dedicated validator types and function
export type { ValidationResult, ValidatorConfig, ValidateActionParams } from "./validator.js";
export { validateAction } from "./validator.js";

const ERROR_KEYWORDS = ["error", "failed", "denied", "forbidden", "unauthorized"];

/**
 * Validate that a planned step references real elements and isn't claiming false success.
 * Returns an error message, or null if valid.
 */
export function validateGrounding(
  step: PlannedStep,
  context: ScreenContext,
  pageOrigin?: string | null,
): string | null {
  const action = step.action;

  // Click always requires a valid target_id
  if (action.type === "click") {
    const targetId = action.target_id;
    if (!targetId) {
      return "Click requires target_id";
    }
    const exists = context.elements.some((el) => el.id === targetId);
    if (!exists) {
      const available = context.elements.slice(0, 10).map((el) => el.id);
      return `Element ID '${targetId}' not found in context. Available: [${available.join(", ")}]`;
    }
  }

  // Type: target_id is OPTIONAL — without it, types into the currently focused element
  if (action.type === "type" && action.target_id) {
    const exists = context.elements.some((el) => el.id === action.target_id);
    if (!exists && context.elements.length > 0) {
      const available = context.elements.slice(0, 10).map((el) => el.id);
      return `Element ID '${action.target_id}' not found in context. Available: [${available.join(", ")}]`;
    }
    // If context has no elements (blind mode), allow any target_id — it'll be ignored
  }

  if (action.type === "done") {
    for (const el of context.elements) {
      if (el.label && el.state?.visible) {
        const lower = el.label.toLowerCase();
        if (ERROR_KEYWORDS.some((kw) => lower.includes(kw))) {
          return `Cannot claim done — error element visible: '${el.label}' (${el.id})`;
        }
      }
    }
    for (const ev of context.http_events ?? []) {
      if (ev.status_code && ev.status_code >= 400) {
        if (pageOrigin && ev.url) {
          try { if (new URL(ev.url).origin !== pageOrigin) continue; } catch { continue; }
        }
        return `Cannot claim done — HTTP ${ev.status_code} on ${ev.url?.slice(0, 50)}`;
      }
    }
    // Evidence IDs are optional — the planner may cite element IDs as proof of completion.
    // Only validate when context has elements AND the IDs look like real element refs (not numbers).
    // Numeric IDs ("1", "2") are planner artifacts from the numbered prompt, not real refs.
    // CDP-sourced context uses "cdp:N" IDs which also may not match.
    const evidenceIds = (action as { evidence_ids?: string[] }).evidence_ids;
    if (evidenceIds && evidenceIds.length > 0 && context.elements.length > 0) {
      const hasRealIds = evidenceIds.some((eid) => eid.includes(":") || eid.length > 5);
      if (hasRealIds) {
        for (const eid of evidenceIds) {
          // Skip numeric-only IDs (planner artifacts)
          if (/^\d+$/.test(eid)) continue;
          if (!context.elements.some((el) => el.id === eid)) {
            return `Evidence element '${eid}' not found in context`;
          }
        }
      }
      // If all IDs are numeric, trust the planner — it extracted data from vision/CDP
    }
  }

  return null;
}

/**
 * Post-action validation: check if a transition action actually changed state.
 * Returns an error hint if state didn't change, null if OK.
 */
export function validatePostAction(
  preActionFP: string | undefined,
  postActionFP: string | undefined,
): string | null {
  if (preActionFP !== undefined && postActionFP !== undefined && preActionFP === postActionFP) {
    return "Action executed but state unchanged — click may not have landed";
  }
  return null;
}
