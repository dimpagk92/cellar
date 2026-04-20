/**
 * Multi-Provider CUA (Computer Use Agent) Abstraction.
 *
 * Provides a unified interface across Anthropic, OpenAI, and Google
 * computer-use APIs. Each provider has different viewport requirements
 * and action formats — this module normalizes them into a single
 * CUAAction type and maps to PlannedAction for the execution layer.
 *
 * License: MIT
 */

import type { PlannedAction } from "./types.js";

/** Screen action from a CUA provider. */
export interface CUAAction {
  type: "click" | "type" | "key" | "scroll" | "screenshot" | "wait" | "done";
  x?: number;
  y?: number;
  text?: string;
  key?: string;
  dx?: number;
  dy?: number;
}

/** CUA provider interface — abstracts across Anthropic/OpenAI/Google CUA APIs. */
export interface CUAProvider {
  name: string;
  /** Get the recommended viewport dimensions for this provider. */
  viewport(): { width: number; height: number };
  /** Send a screenshot + instruction, get back an action. */
  step(
    screenshot: Buffer,
    instruction: string,
    history?: CUAAction[],
  ): Promise<CUAAction>;
}

/** Map a CUA action to a PlannedAction. */
export function cuaToPlannedAction(action: CUAAction): PlannedAction {
  switch (action.type) {
    case "click":
      if (action.x !== undefined && action.y !== undefined) {
        return {
          type: "custom",
          adapter: "browser",
          action: "click",
          params: { x: action.x, y: action.y },
        };
      }
      return { type: "fail", reason: "click action missing coordinates" };

    case "type":
      return {
        type: "custom",
        adapter: "browser",
        action: "input_text",
        params: { text: action.text ?? "", x: action.x, y: action.y },
      };

    case "key":
      return { type: "key", key: action.key ?? "" };

    case "scroll":
      return { type: "scroll", dx: action.dx ?? 0, dy: action.dy ?? 0 };

    case "screenshot":
      return {
        type: "custom",
        adapter: "browser",
        action: "screenshot",
        params: {},
      };

    case "wait":
      return { type: "wait", ms: 1000 };

    case "done":
      return { type: "done", summary: action.text ?? "CUA task completed" };

    default:
      return { type: "fail", reason: `Unknown CUA action type: ${action.type}` };
  }
}

/** Provider registry with recommended viewport dimensions. */
export const CUA_PROVIDERS: Record<
  string,
  { viewport: { width: number; height: number } }
> = {
  anthropic: { viewport: { width: 1280, height: 800 } },
  openai: { viewport: { width: 1024, height: 768 } },
  google: { viewport: { width: 1288, height: 711 } },
};
