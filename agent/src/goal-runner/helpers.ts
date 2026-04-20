/**
 * Shared helpers for the goal runner.
 */

import type { PlannedAction, ScreenContext, WorkflowAction } from "../types.js";

export function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export function simpleHash(str: string): number {
  let hash = 0;
  for (let i = 0; i < str.length; i++) {
    hash = ((hash << 5) - hash + str.charCodeAt(i)) | 0;
  }
  return hash;
}

export function contextFingerprint(context: ScreenContext): number {
  const sig = context.elements.slice(0, 10).map((e) => `${e.id}:${e.element_type}`).join(",");
  return simpleHash(`${context.app}:${context.window}:${context.elements.length}:${sig}`);
}

export function actionSignature(action: PlannedAction): string {
  switch (action.type) {
    case "click": return `click:${action.target_id}`;
    case "type": return `type:${action.target_id}`;
    case "set_value": return `set_value:${action.target_id}`;
    case "ax_action": return `ax_action:${action.target_id}:${action.action}`;
    case "activate_app": return `activate_app:${action.app_name}`;
    case "key": return `key:${action.key}`;
    case "key_combo": return `combo:${action.keys.join("+")}`;
    case "scroll": return `scroll:${action.dx},${action.dy}`;
    case "drag": return `drag:${action.from_x},${action.from_y}-${action.to_x},${action.to_y}`;
    case "wait": return `wait:${action.ms}`;
    case "custom": return `custom:${action.adapter}.${action.action}`;
    case "extract": return `extract:${action.goal}`;
    case "act": return `act:${action.instruction}`;
    case "batch": return `batch:${action.actions.length}`;
    case "done": return `done`;
    case "fail": return `fail`;
    case "notebook_writes": return `noop:notebook`;
    default: return `unknown`;
  }
}

/** Extract the target element ID from a PlannedAction, if any. */
export function getActionTargetId(action: PlannedAction): string | undefined {
  switch (action.type) {
    case "click": return action.target_id;
    case "type": return action.target_id ?? undefined;
    case "set_value": return action.target_id;
    case "ax_action": return action.target_id;
    case "drag": return undefined;
    default: return undefined;
  }
}

/** Check if an action type typically causes a state transition. */
export function isTransitionAction(action: PlannedAction): boolean {
  return action.type === "click" || action.type === "custom" || action.type === "type"
    || action.type === "set_value" || action.type === "ax_action" || action.type === "activate_app"
    || action.type === "key" || action.type === "drag" || action.type === "act";
}

/** Default tiered wait times in ms, per action type. */
export const DEFAULT_SETTLE_MS: Record<string, number> = {
  click: 800,
  ax_action: 800,
  activate_app: 1500,
  act: 800,
  custom: 500,
  type: 500,
  set_value: 500,
  key: 200,
  key_combo: 200,
  scroll: 200,
  drag: 500,
  wait: 0,
};

/** Convert PlannedAction → WorkflowAction for the executor. */
export function plannedToWorkflowAction(action: PlannedAction): WorkflowAction | null {
  switch (action.type) {
    case "click": return { type: "click", target: action.target_id };
    case "type": return { type: "type", target: action.target_id ?? "", text: action.text };
    case "set_value": return { type: "set_value", target: action.target_id, value: action.value };
    case "ax_action": return { type: "ax_action", target: action.target_id, action: action.action };
    case "key": return { type: "key", key: action.key };
    case "key_combo": return { type: "key_combo", keys: action.keys };
    case "scroll": return { type: "scroll", dx: action.dx, dy: action.dy };
    case "drag": return { type: "drag", fromX: action.from_x, fromY: action.from_y, toX: action.to_x, toY: action.to_y };
    case "wait": return { type: "wait", ms: action.ms };
    case "custom": return { type: "custom", adapter: action.adapter, action: action.action, params: action.params };
    case "act": case "extract": case "batch": case "done": case "fail": case "notebook_writes": return null;
    default: return null;
  }
}

export function cachedStepMatchesContext(step: import("../types.js").PlannedStep, context: ScreenContext): boolean {
  const action = step.action;
  if (action.type === "click" || action.type === "type") {
    return context.elements.some(
      (el) => el.id === action.target_id && el.state.enabled && el.state.visible,
    );
  }
  return true;
}

/**
 * Extract the first JSON object `{...}` from a string.
 * Used when the LLM returns prose + JSON.
 */
export function extractJsonObject(raw: string): string | null {
  const start = raw.indexOf("{");
  if (start === -1) return null;
  let depth = 0;
  let inString = false;
  let escape = false;
  for (let i = start; i < raw.length; i++) {
    const ch = raw[i];
    if (escape) { escape = false; continue; }
    if (ch === "\\") { escape = true; continue; }
    if (ch === '"') { inString = !inString; continue; }
    if (inString) continue;
    if (ch === "{") depth++;
    if (ch === "}") { depth--; if (depth === 0) return raw.slice(start, i + 1); }
  }
  return null;
}
