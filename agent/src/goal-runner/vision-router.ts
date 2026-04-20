/**
 * Vision Router — routes vision requests between localizer (cheap) and frontier (expensive).
 *
 * When `local_first` mode is active and a localizer LLM is configured,
 * grounding/comparison tasks use the cheap model (e.g., Gemini Flash) while
 * complex visual reasoning falls back to the frontier model.
 */

import type { VisionMode } from "./vision-manager.js";

/** The type of vision task being requested. */
export type VisionTaskType = "grounding" | "reasoning" | "comparison";

/** Which vision provider to use and why. */
export interface VisionRoute {
  provider: "frontier" | "localizer";
  reason: string;
}

/**
 * Decide which vision provider to use for a given task.
 *
 * @param mode - Current vision mode
 * @param localizerAvailable - Whether a localizer LLM endpoint is configured
 * @param taskType - What kind of vision task this is
 */
export function routeVision(
  mode: VisionMode,
  localizerAvailable: boolean,
  taskType: VisionTaskType,
): VisionRoute {
  if (mode === "local_first" && localizerAvailable) {
    if (taskType === "reasoning") {
      return { provider: "frontier", reason: "Complex visual reasoning requires frontier model" };
    }
    return { provider: "localizer", reason: `${taskType} routed to cheap localizer model` };
  }

  return { provider: "frontier", reason: "Default frontier routing" };
}
