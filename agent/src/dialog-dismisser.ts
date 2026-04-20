/**
 * Dialog Dismisser — detects common blocking dialogs (cookies, notifications, etc.)
 *
 * Inspired by WebTactix's background modal watcher that auto-dismisses
 * consent dialogs. Cellar's version is observation-only: it flags dismissable
 * elements but does NOT auto-click. The MCP caller (Claude) decides.
 *
 * Safety principle: cortex observes, agent acts.
 */

import type { ScreenContext } from "./types.js";

// ─── Types ─────────────────────────────────────────────────────────────────

export interface DismissableDialog {
  /** Element ID of the button to dismiss. */
  elementId: string;
  /** Label of the button. */
  label: string;
  /** What kind of dialog this is. */
  dialogType: "cookie_consent" | "notification_prompt" | "generic_dismiss";
  /** Suggested action. */
  action: "click";
}

// ─── Patterns ──────────────────────────────────────────────────────────────

interface DismissPattern {
  label: RegExp;
  dialogType: DismissableDialog["dialogType"];
  /** Higher = prefer this over other matches on the same dialog. */
  priority: number;
}

/**
 * Ordered by priority — prefer privacy-preserving actions.
 * E.g., "Reject all" over "Accept all" for cookies.
 */
const DISMISS_PATTERNS: DismissPattern[] = [
  // Cookie consent — prefer reject
  { label: /^reject\s*(all)?$/i, dialogType: "cookie_consent", priority: 10 },
  { label: /^decline\s*(all)?$/i, dialogType: "cookie_consent", priority: 10 },
  { label: /^deny\s*(all)?$/i, dialogType: "cookie_consent", priority: 9 },
  { label: /^only\s*essential/i, dialogType: "cookie_consent", priority: 9 },
  { label: /^accept\s*(all\s*)?cookies?$/i, dialogType: "cookie_consent", priority: 5 },
  { label: /^accept\s*all$/i, dialogType: "cookie_consent", priority: 5 },
  { label: /^(i\s*)?agree$/i, dialogType: "cookie_consent", priority: 4 },

  // Notification prompts — prefer deny
  { label: /^(no\s*thanks?|not?\s*now|later|maybe\s*later|skip)$/i, dialogType: "notification_prompt", priority: 8 },
  { label: /^deny$/i, dialogType: "notification_prompt", priority: 8 },
  { label: /^block$/i, dialogType: "notification_prompt", priority: 7 },

  // Generic dismiss
  { label: /^(dismiss|close|got\s*it|ok|okay)$/i, dialogType: "generic_dismiss", priority: 6 },
  { label: /^×$/, dialogType: "generic_dismiss", priority: 3 }, // × close button
];

// ─── Public API ────────────────────────────────────────────────────────────

/**
 * Scan the current context for dismissable blocking dialogs.
 * Returns the best dismiss target, or null if none found.
 */
export function findDismissableDialog(
  ctx: ScreenContext,
): DismissableDialog | null {
  let bestMatch: DismissableDialog | null = null;
  let bestPriority = -1;

  for (const el of ctx.elements) {
    // Only consider visible, enabled buttons/links
    if (!el.state?.visible || !el.state?.enabled) continue;
    if (el.element_type !== "button" && el.element_type !== "link") continue;

    const label = (el.label ?? "").trim();
    if (label.length === 0) continue;

    for (const pattern of DISMISS_PATTERNS) {
      if (pattern.label.test(label) && pattern.priority > bestPriority) {
        bestMatch = {
          elementId: el.id,
          label,
          dialogType: pattern.dialogType,
          action: "click",
        };
        bestPriority = pattern.priority;
      }
    }
  }

  return bestMatch;
}
