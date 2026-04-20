/**
 * Vision mode management — controls when screenshots are sent to the LLM.
 * Based on Stagehand v3's vision modes.
 *
 * Integrates with vision-router.ts for multi-model routing when a
 * localizer endpoint is configured (cheap grounding, expensive reasoning).
 */

import type { ScreenContext } from "../types.js";
import { isActionableType } from "./context-distiller.js";
import { routeVision, type VisionRoute, type VisionTaskType } from "./vision-router.js";

export type VisionMode = "always" | "auto" | "never" | "local_first";

/**
 * Decide whether to use vision (screenshot) for this step.
 * "auto": first step, sparse context, or 2+ failures.
 * "always": every step.
 * "never": never.
 * "local_first": always use vision — routes to cheap localizer model (see vision-router.ts).
 */
/**
 * Goal-aware vision triggers — when the goal itself requires visual understanding,
 * force vision regardless of element count or failure state.
 *
 * Covers: spatial tasks (drag, position, align), visual identification (color, red,
 * blue, chart, graph, logo), image-based tasks (screenshot, photo, picture, thumbnail),
 * layout tasks (resize, arrange, grid), and drawing/annotation tasks.
 */
const VISUAL_GOAL_PATTERN = /drag|drop|colou?r|\bred\b|\bblue\b|\bgreen\b|image|position|shape|icon|screenshot|visual|canvas|draw|slider|resize|move.*to|chart|graph|plot|diagram|logo|photo|picture|thumbnail|align|arrange|layout|grid|map|pixel|coordinate|crop|rotate|flip|badge|avatar|banner|carousel|gallery|infographic|annotation|highlight|underline|strikethrough/i;

export function shouldUseVision(
  mode: VisionMode,
  stepIndex: number,
  context: ScreenContext,
  consecutiveFailures: number,
  hasScreenshot: boolean,
  goal?: string,
): boolean {
  if (!hasScreenshot) return false;
  switch (mode) {
    case "always": return true;
    case "local_first": return true; // always on — localizer is cheap
    case "never": return false;
    case "auto":
    default: {
      // Only use vision when structured context is insufficient.
      const actionable = context.elements.filter((el) => isActionableType(el.element_type)).length;

      // Sparse interactive context (page has elements but few are clickable).
      // Skip vision when context only has page-text fallback — vision can't help
      // because there are no element IDs to target from screenshot analysis.
      const hasOnlyPageText = context.elements.length <= 2 &&
        context.elements.every(e => e.id === "page-text" || e.element_type === "text");
      if (context.elements.length > 0 && actionable < 5 && !hasOnlyPageText) return true;

      // Stuck — try vision as a fallback
      if (consecutiveFailures >= 2) return true;

      // Goal involves visual concepts (drag, color, shape, etc.)
      if (goal && VISUAL_GOAL_PATTERN.test(goal)) return true;

      return false;
    }
  }
}

/**
 * Determine which vision provider to use for a given step.
 * When a localizer is available and mode is "local_first",
 * cheap grounding/comparison tasks use the localizer while
 * complex reasoning uses the frontier model.
 *
 * Returns the routing decision with provider and reason.
 */
export function getVisionRoute(
  mode: VisionMode,
  localizerAvailable: boolean,
  consecutiveFailures: number,
): VisionRoute {
  // Determine task type from failure context
  const taskType: VisionTaskType = consecutiveFailures >= 2 ? "reasoning" : "grounding";
  return routeVision(mode, localizerAvailable, taskType);
}

// ─── Sub-region Vision Zoom ────────────────────────────────────────────────

/** Region definition for sub-region zoom. */
export interface ZoomRegion {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Result of coordinate refinement via sub-region zoom. */
export interface RefinedCoordinate {
  x: number;
  y: number;
  confidence: "high" | "medium" | "low";
}

/**
 * Compute a zoom region centered around target coordinates.
 * The region is clamped to screen bounds and provides ~4x pixel resolution
 * over the full screenshot for spatial precision tasks.
 *
 * @param targetX - Approximate X coordinate from initial vision pass
 * @param targetY - Approximate Y coordinate from initial vision pass
 * @param screenWidth - Full screenshot width
 * @param screenHeight - Full screenshot height
 * @param regionSize - Size of the zoom region (default 200px square)
 */
export function computeZoomRegion(
  targetX: number,
  targetY: number,
  screenWidth: number,
  screenHeight: number,
  regionSize: number = 200,
): ZoomRegion {
  const halfSize = Math.floor(regionSize / 2);
  // Clamp to screen bounds
  let x = Math.max(0, Math.min(targetX - halfSize, screenWidth - regionSize));
  let y = Math.max(0, Math.min(targetY - halfSize, screenHeight - regionSize));
  const width = Math.min(regionSize, screenWidth - x);
  const height = Math.min(regionSize, screenHeight - y);
  return { x, y, width, height };
}

/**
 * Build a refinement prompt for the LLM to confirm exact click position
 * within a zoomed sub-region.
 *
 * @param goal - The original goal or action description
 * @param region - The zoom region bounds (in full-screenshot coordinates)
 * @returns Prompt text to send with the cropped image
 */
export function buildZoomRefinementPrompt(goal: string, region: ZoomRegion): string {
  return (
    `This is a ${region.width}x${region.height} pixel crop from a larger screenshot. ` +
    `The crop starts at (${region.x}, ${region.y}) in the full image.\n\n` +
    `Goal: ${goal}\n\n` +
    `Identify the EXACT pixel position to click within this cropped region. ` +
    `Return coordinates relative to the FULL screenshot (add the crop offset back).\n` +
    `Respond with JSON: {"x": <number>, "y": <number>, "confidence": "high"|"medium"|"low"}`
  );
}

/**
 * Crop a screenshot buffer to a sub-region.
 * Uses raw pixel slicing — assumes the screenshot is a flat RGBA buffer
 * or delegates to the callback for format-aware cropping.
 *
 * @param screenshot - Full screenshot as Buffer (PNG or raw)
 * @param region - Region to crop
 * @param cropFn - Optional callback for format-aware cropping (e.g., sharp, canvas)
 * @returns Cropped image buffer
 */
export async function cropScreenshot(
  screenshot: Buffer,
  region: ZoomRegion,
  cropFn?: (buf: Buffer, region: ZoomRegion) => Promise<Buffer>,
): Promise<Buffer> {
  if (cropFn) {
    return cropFn(screenshot, region);
  }
  // Fallback: return full screenshot with region metadata
  // (the LLM can visually focus on the described region)
  return screenshot;
}

/**
 * Perform a sub-region zoom refinement pass.
 * Takes the initial coordinates from a vision-assisted plan step,
 * crops the screenshot around those coordinates, and asks the LLM
 * to refine the exact click position.
 *
 * @param initialX - X from initial vision pass
 * @param initialY - Y from initial vision pass
 * @param screenshot - Full screenshot buffer
 * @param screenWidth - Full screenshot width
 * @param screenHeight - Full screenshot height
 * @param goal - The action goal (for context in the refinement prompt)
 * @param visionFn - Callback to send image + prompt to the vision LLM
 * @param cropFn - Optional format-aware crop function
 * @returns Refined coordinates in full-screenshot space
 */
export async function refineWithZoom(
  initialX: number,
  initialY: number,
  screenshot: Buffer,
  screenWidth: number,
  screenHeight: number,
  goal: string,
  visionFn: (image: Buffer, prompt: string) => Promise<string>,
  cropFn?: (buf: Buffer, region: ZoomRegion) => Promise<Buffer>,
): Promise<RefinedCoordinate> {
  const region = computeZoomRegion(initialX, initialY, screenWidth, screenHeight);
  const cropped = await cropScreenshot(screenshot, region, cropFn);
  const prompt = buildZoomRefinementPrompt(goal, region);

  try {
    const response = await visionFn(cropped, prompt);
    // Parse the LLM's JSON response
    const jsonMatch = response.match(/\{[^}]+\}/);
    if (jsonMatch) {
      const parsed = JSON.parse(jsonMatch[0]);
      if (typeof parsed.x === "number" && typeof parsed.y === "number") {
        return {
          x: parsed.x,
          y: parsed.y,
          confidence: parsed.confidence ?? "medium",
        };
      }
    }
  } catch {
    // Refinement failed — fall back to original coordinates
  }

  return { x: initialX, y: initialY, confidence: "low" };
}
