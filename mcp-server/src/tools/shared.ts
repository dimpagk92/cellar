import { z } from "zod";
import type { Cel, ScreenContext, ContextElement, ContextReference } from "@cellar/agent";

/** Async delay. */
export function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Context reference Zod schema — shared between cel_see and cel_act. */
export const contextReferenceSchema = z.object({
  element_type: z.string(),
  label: z.string().optional(),
  ancestor_path: z.array(z.string()).optional(),
  bounds_region: z
    .object({
      quadrant: z.string(),
      relative_x: z.number(),
      relative_y: z.number(),
    })
    .optional(),
  value_pattern: z.string().optional(),
});

const URL_PATTERN = /https?:\/\/[^\s"'<>]+/g;

/** Extract URLs from elements and build a numeric ID → URL map for anti-hallucination. */
export function buildUrlMap(elements: ContextElement[]): Record<number, string> {
  const urlToId = new Map<string, number>();
  let nextId = 1;

  for (const el of elements) {
    const sources = [el.value, el.label, el.description].filter(Boolean);
    for (const source of sources) {
      const matches = source!.match(URL_PATTERN);
      if (matches) {
        for (const url of matches) {
          if (!urlToId.has(url)) {
            urlToId.set(url, nextId++);
          }
        }
      }
    }
  }

  const result: Record<number, string> = {};
  for (const [url, id] of urlToId) {
    result[id] = url;
  }
  return result;
}

/** Strip an element down to compact fields only. */
export function compactElement(el: ContextElement) {
  return {
    id: el.id,
    element_type: el.element_type,
    label: el.label,
    actions: el.actions,
  };
}

/** Redact password field values from element. */
export function sanitizeElement(el: ContextElement): ContextElement {
  if (el.element_type === "password" || el.element_type?.includes("password")) {
    return { ...el, value: undefined };
  }
  return el;
}

/** Compute a fingerprint of a context snapshot for idle detection. */
export function contextFingerprint(ctx: ScreenContext): string {
  return ctx.elements
    .map((el) => `${el.id}:${el.label ?? ""}:${el.element_type}`)
    .join("|");
}

/** Check if element matches type and label criteria. */
export function elementMatches(
  el: ContextElement,
  elementType?: string,
  labelContains?: string,
): boolean {
  if (elementType && el.element_type !== elementType) return false;
  if (labelContains) {
    const label = (el.label ?? "").toLowerCase();
    if (!label.includes(labelContains.toLowerCase())) return false;
  }
  return true;
}

/** Resolve coordinates from target_ref or explicit x/y. */
export function resolveCoords(
  cel: Cel,
  action: { x?: number; y?: number; target_ref?: unknown },
): { x: number; y: number; label: string } {
  if (action.target_ref) {
    const ctx = cel.getContext();
    const resolved = cel.resolveReference(ctx, action.target_ref as ContextReference);
    if (!resolved) {
      throw new Error(
        `Could not find element matching reference: ${JSON.stringify(action.target_ref)}`,
      );
    }
    if (!resolved.bounds) {
      throw new Error(
        `Resolved element "${resolved.label ?? resolved.id}" has no bounds`,
      );
    }
    const x = resolved.bounds.x + Math.floor(resolved.bounds.width / 2);
    const y = resolved.bounds.y + Math.floor(resolved.bounds.height / 2);
    return { x, y, label: resolved.label ?? resolved.id };
  }
  if (action.x === undefined || action.y === undefined) {
    throw new Error("Requires x and y coordinates, or target_ref");
  }
  return { x: action.x, y: action.y, label: `(${action.x}, ${action.y})` };
}

/** Standard text response for MCP. */
export function textResult(data: unknown) {
  return {
    content: [{ type: "text" as const, text: JSON.stringify(data, null, 2) }],
  };
}

/** Standard error response for MCP. */
export function errorResult(message: string) {
  return {
    content: [{ type: "text" as const, text: `Error: ${message}` }],
    isError: true as const,
  };
}

/**
 * Pre-flight: ensure the host process has macOS Accessibility permission.
 * Returns null when granted (or on non-macOS where AX permission isn't a thing).
 * Returns a structured error response when denied — caller should `return` it
 * directly from the tool handler so the user sees a clear remediation path
 * instead of an empty context or a deep AX traversal error.
 */
export function axPermissionGuard(cel: Cel) {
  if (cel.isAxPermissionGranted) return null;
  return {
    content: [
      {
        type: "text" as const,
        text: [
          "macOS Accessibility permission required.",
          "",
          "Cellar reads the screen via the macOS Accessibility API, which requires",
          "explicit user grant for the host process running this MCP server",
          "(typically Terminal.app, iTerm.app, or your IDE — Cursor, Claude Code, etc.).",
          "",
          "Grant it:",
          "  1. Open System Settings → Privacy & Security → Accessibility",
          "  2. Add the host process and toggle it ON",
          "  3. Restart the host process — macOS does not pick up permission changes mid-process",
          "",
          "Or run from a shell:",
          '  open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"',
        ].join("\n"),
      },
    ],
    isError: true as const,
  };
}
