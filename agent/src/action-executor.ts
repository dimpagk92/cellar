/**
 * Action Executor — maps WorkflowStep actions to CEL input calls (LEGACY).
 *
 * @deprecated Execution now goes through the Cortex adapter system.
 * The Rust goal-runner dispatches actions via cortex.execute(), which
 * routes to the owning adapter. This TS executor is kept for backward
 * compatibility with the TS goal-runner fallback path.
 *
 * Original: the "glue" that translates declarative workflow actions
 * into real desktop input via the Cel native bindings.
 *
 * Integrates browser-use learnings:
 * - Cascading element resolution (ID → label → fuzzy → spatial)
 * - Retry with exponential backoff on element not found
 * - ContextReference-based resolution when available
 * - Variable substitution in type actions
 */

import type { InputController } from "./interfaces/input-controller.js";
import type { ContextProvider } from "./interfaces/context-provider.js";
import type { WorkflowStep, WorkflowAction, ScreenContext, ContextReference } from "./types.js";

/** Minimal CEL capability set needed by the action executor. */
type ActionExecutorDeps = InputController & Pick<ContextProvider, "resolveReference">;

/** Interface for adapters that can execute custom actions. */
export interface ActionAdapter {
  executeAction(
    action: string,
    params: Record<string, unknown>,
  ): Promise<boolean>;
}

/** Registry of adapters keyed by name (e.g., "browser", "excel"). */
export type AdapterRegistry = Record<string, ActionAdapter>;

/** Extended step with optional ContextReference for resilient replay. */
export interface ReplayableStep extends WorkflowStep {
  target_ref?: ContextReference;
}

/** Options for action execution. */
export interface ExecuteOptions {
  /** Callback to refresh the screen context (for retry). */
  getContext?: () => Promise<ScreenContext>;
  /** Maximum retries on target resolution failure. Default: 0 (no retry). */
  maxRetries?: number;
  /** Runtime variables for {{placeholder}} substitution. */
  variables?: Record<string, string>;
}

/**
 * Cascading element resolution: tries multiple strategies to find the target.
 *
 * 1. Exact ID match
 * 2. Case-insensitive label match
 * 3. Fuzzy label match (contains in either direction)
 * 4. ContextReference resolution (multi-signal: type + label + bounds + value)
 */
function resolveTargetCascading(
  cel: ActionExecutorDeps,
  target: string,
  context: ScreenContext,
  targetRef?: ContextReference,
): { x: number; y: number } | null {
  // 1. ContextReference resolution (highest fidelity if available)
  if (targetRef) {
    const resolved = cel.resolveReference(context, targetRef);
    if (resolved?.bounds) {
      return {
        x: resolved.bounds.x + Math.floor(resolved.bounds.width / 2),
        y: resolved.bounds.y + Math.floor(resolved.bounds.height / 2),
      };
    }
  }

  // 2. Exact ID match
  const byId = context.elements.find((e) => e.id === target);
  if (byId?.bounds) {
    return {
      x: byId.bounds.x + Math.floor(byId.bounds.width / 2),
      y: byId.bounds.y + Math.floor(byId.bounds.height / 2),
    };
  }

  // 3. Case-insensitive label match
  const byLabel = context.elements.find(
    (e) => e.label?.toLowerCase() === target.toLowerCase(),
  );
  if (byLabel?.bounds) {
    return {
      x: byLabel.bounds.x + Math.floor(byLabel.bounds.width / 2),
      y: byLabel.bounds.y + Math.floor(byLabel.bounds.height / 2),
    };
  }

  // 4. Fuzzy label match (contains in either direction)
  const targetLower = target.toLowerCase();
  const byFuzzy = context.elements.find((e) => {
    if (!e.label || !e.bounds) return false;
    const labelLower = e.label.toLowerCase();
    return labelLower.includes(targetLower) || targetLower.includes(labelLower);
  });
  if (byFuzzy?.bounds) {
    return {
      x: byFuzzy.bounds.x + Math.floor(byFuzzy.bounds.width / 2),
      y: byFuzzy.bounds.y + Math.floor(byFuzzy.bounds.height / 2),
    };
  }

  return null;
}

/**
 * Resolve target with retry and exponential backoff.
 * Refreshes context between retries to handle DOM transitions.
 */
async function resolveTargetWithRetry(
  cel: ActionExecutorDeps,
  target: string,
  context: ScreenContext,
  opts: ExecuteOptions,
  targetRef?: ContextReference,
): Promise<{ x: number; y: number; ctx: ScreenContext }> {
  const maxRetries = opts.maxRetries ?? 0;
  let currentCtx = context;

  // First attempt
  const result = resolveTargetCascading(cel, target, currentCtx, targetRef);
  if (result) return { ...result, ctx: currentCtx };

  // Retry with exponential backoff
  let delay = 500;
  for (let attempt = 0; attempt < maxRetries; attempt++) {
    if (!opts.getContext) break;
    await sleep(delay);
    currentCtx = await opts.getContext();
    const retryResult = resolveTargetCascading(cel, target, currentCtx, targetRef);
    if (retryResult) return { ...retryResult, ctx: currentCtx };
    delay *= 2; // 500 → 1000 → 2000
  }

  throw new Error(
    `Target element not found after ${maxRetries + 1} attempts: "${target}" for step`,
  );
}

/** Resolve {{variable}} placeholders in a string. */
function substituteVariables(text: string, variables?: Record<string, string>): string {
  if (!variables) return text;
  return text.replace(/\{\{([\w-]+)\}\}/g, (_match, key) => {
    return variables[key] ?? `{{${key}}}`;
  });
}

/**
 * Execute a single workflow action using the Cel native bindings.
 * Returns true on success, throws on failure.
 */
export async function executeAction(
  cel: ActionExecutorDeps,
  step: WorkflowStep | ReplayableStep,
  context: ScreenContext,
  adapters?: AdapterRegistry,
  opts?: ExecuteOptions,
): Promise<boolean> {
  const action = step.action;
  const execOpts = opts ?? {};
  const targetRef = (step as ReplayableStep).target_ref;

  switch (action.type) {
    case "click": {
      const { x, y } = await resolveTargetWithRetry(
        cel, action.target, context, execOpts, targetRef,
      );
      if (action.button === "right") {
        cel.rightClick(x, y);
      } else {
        cel.click(x, y);
      }
      return true;
    }

    case "type": {
      const text = substituteVariables(action.text, execOpts.variables);
      if (action.target && action.target !== "") {
        // Click target field first to focus it, then type
        const { x, y } = await resolveTargetWithRetry(
          cel, action.target, context, execOpts, targetRef,
        );
        cel.click(x, y);
        await sleep(100); // Brief pause for focus
      }
      // Type into whatever is currently focused
      cel.typeText(text);
      return true;
    }

    case "key": {
      cel.keyPress(action.key);
      return true;
    }

    case "key_combo": {
      cel.keyCombo(action.keys);
      return true;
    }

    case "wait": {
      await sleep(action.ms);
      return true;
    }

    case "scroll": {
      cel.scroll(action.dx, action.dy);
      return true;
    }

    case "set_value": {
      // Use native accessibility API — target is an a11y element ID, not coordinates.
      const success = cel.axSetValue(action.target, action.value);
      if (!success) {
        throw new Error(`axSetValue failed for element "${action.target}"`);
      }
      return true;
    }

    case "ax_action": {
      // Use native accessibility API — target is an a11y element ID, not coordinates.
      const success = cel.axPerformAction(action.target, action.action);
      if (!success) {
        throw new Error(`axPerformAction "${action.action}" failed for element "${action.target}"`);
      }
      return true;
    }

    case "drag": {
      cel.drag(action.fromX, action.fromY, action.toX, action.toY);
      return true;
    }

    case "select": {
      // Text selection: mouse down at start, move to end, mouse up
      // This creates a text selection drag from (from_x,from_y) to (to_x,to_y)
      cel.drag(action.from_x, action.from_y, action.to_x, action.to_y);
      return true;
    }

    case "custom": {
      // Dispatch to the registered adapter
      if (adapters?.[action.adapter]) {
        return adapters[action.adapter].executeAction(
          action.action,
          action.params,
        );
      }
      console.warn(
        `Custom action "${action.action}" on adapter "${action.adapter}" — adapter not registered`,
      );
      return true;
    }

    default: {
      const _exhaustive: never = action;
      throw new Error(`Unknown action type: ${JSON.stringify(_exhaustive)}`);
    }
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
