/**
 * Context Differ — Compares two ScreenContext snapshots
 *
 * After a UI-changing action (dropdown open, modal appear, autocomplete expand),
 * diffs the before/after context to identify only NEW elements. This allows
 * the planner to receive a focused prompt showing just what changed,
 * saving 40-80% of tokens on compound interactions.
 *
 * Inspired by Stagehand v3's two-step action diffing.
 */

import type { ScreenContext, ContextElement } from "./types.js";

/** The result of diffing two context snapshots. */
export interface ContextDiff {
  /** Elements present in `after` but not in `before`. */
  added: ContextElement[];
  /** Element IDs present in `before` but not in `after`. */
  removed: string[];
  /** Elements present in both but with different value or state. */
  changed: ChangedElement[];
  /** Number of elements unchanged between snapshots. */
  unchangedCount: number;
}

/** An element that changed between snapshots. */
export interface ChangedElement {
  element: ContextElement;
  changes: string[];
}

/**
 * Diff two ScreenContext snapshots by element ID.
 *
 * - `added`: IDs in `after` that don't exist in `before`
 * - `removed`: IDs in `before` that don't exist in `after`
 * - `changed`: Same ID, different value/state (selected, expanded, checked, value)
 * - `unchangedCount`: Elements with identical ID and state
 */
export function diffContexts(
  before: ScreenContext,
  after: ScreenContext,
): ContextDiff {
  const beforeMap = new Map<string, ContextElement>();
  for (const el of before.elements) {
    beforeMap.set(el.id, el);
  }

  const afterMap = new Map<string, ContextElement>();
  for (const el of after.elements) {
    afterMap.set(el.id, el);
  }

  const added: ContextElement[] = [];
  const changed: ChangedElement[] = [];
  let unchangedCount = 0;

  for (const [id, afterEl] of afterMap) {
    const beforeEl = beforeMap.get(id);
    if (!beforeEl) {
      added.push(afterEl);
    } else {
      const changes = detectChanges(beforeEl, afterEl);
      if (changes.length > 0) {
        changed.push({ element: afterEl, changes });
      } else {
        unchangedCount++;
      }
    }
  }

  const removed: string[] = [];
  for (const id of beforeMap.keys()) {
    if (!afterMap.has(id)) {
      removed.push(id);
    }
  }

  return { added, removed, changed, unchangedCount };
}

/** Detect what changed between two versions of the same element. */
function detectChanges(
  before: ContextElement,
  after: ContextElement,
): string[] {
  const changes: string[] = [];

  if (before.value !== after.value) {
    changes.push(`value: "${before.value ?? ""}" → "${after.value ?? ""}"`);
  }
  if (before.state.selected !== after.state.selected) {
    changes.push(`selected: ${before.state.selected} → ${after.state.selected}`);
  }
  if (before.state.expanded !== after.state.expanded) {
    changes.push(`expanded: ${before.state.expanded} → ${after.state.expanded}`);
  }
  if (before.state.checked !== after.state.checked) {
    changes.push(`checked: ${before.state.checked} → ${after.state.checked}`);
  }
  if (before.state.focused !== after.state.focused) {
    changes.push(`focused: ${before.state.focused} → ${after.state.focused}`);
  }
  if (before.state.visible !== after.state.visible) {
    changes.push(`visible: ${before.state.visible} → ${after.state.visible}`);
  }
  if (before.label !== after.label) {
    changes.push(`label: "${before.label ?? ""}" → "${after.label ?? ""}"`);
  }

  return changes;
}

/**
 * Determine if a diff is significant enough to warrant diff-mode prompting.
 * Returns true if there are meaningful new/changed elements (not just noise).
 */
export function isDiffSignificant(diff: ContextDiff): boolean {
  // Significant if new actionable elements appeared (dropdown options, modal buttons, etc.)
  if (diff.added.length === 0 && diff.changed.length === 0) return false;

  // At least one added element must be interactive
  const hasInteractiveAdd = diff.added.some(
    (el) =>
      el.state.visible &&
      el.state.enabled &&
      (el.actions?.length ?? 0) > 0,
  );

  // Or meaningful state/content changes
  const hasMeaningfulChange = diff.changed.some(
    (c) =>
      c.changes.some(
        (ch) =>
          ch.startsWith("expanded:") ||
          ch.startsWith("selected:") ||
          ch.startsWith("visible:") ||
          ch.startsWith("label:") ||
          ch.startsWith("value:") ||
          ch.startsWith("checked:"),
      ),
  );

  return hasInteractiveAdd || hasMeaningfulChange || diff.added.length >= 3 || diff.removed.length >= 1;
}

/**
 * Ultra-compact diff format for incremental context updates.
 * Produces ~200-400 tokens instead of 2000-4000 for a full element table.
 * Used in conversation-aware planning (step 3+) to send only what changed.
 */
export function formatCompactDiff(diff: ContextDiff): string {
  const lines: string[] = [];

  if (diff.added.length > 0) {
    const addedSummary = diff.added.slice(0, 15).map((el) => {
      const label = (el.label ?? "").slice(0, 40);
      return `[${el.id}] ${el.element_type} "${label}"`;
    });
    lines.push(`+ ADDED (${diff.added.length}): ${addedSummary.join(", ")}`);
    if (diff.added.length > 15) lines.push(`  ... and ${diff.added.length - 15} more`);
  }

  if (diff.removed.length > 0) {
    lines.push(`- REMOVED (${diff.removed.length}): ${diff.removed.slice(0, 10).join(", ")}`);
  }

  if (diff.changed.length > 0) {
    const changedSummary = diff.changed.slice(0, 10).map((c) =>
      `[${c.element.id}] ${c.changes.join("; ")}`,
    );
    lines.push(`~ CHANGED (${diff.changed.length}): ${changedSummary.join(", ")}`);
  }

  lines.push(`= UNCHANGED: ${diff.unchangedCount} elements (not shown)`);

  return lines.join("\n");
}

/**
 * Format a context diff for inclusion in a planner prompt.
 * Returns a string showing only new/changed elements in a compact table.
 */
export function formatDiffForPrompt(diff: ContextDiff): string {
  const lines: string[] = [];

  if (diff.added.length > 0) {
    lines.push("## NEW elements appeared after the previous action:");
    lines.push("");
    lines.push("| ID | Type | Label | Value | Actions |");
    lines.push("|---|---|---|---|---|");
    for (const el of diff.added) {
      const label = el.label ?? "";
      const value = el.value ?? "";
      const actions = el.actions?.join(", ") ?? "";
      lines.push(`| ${el.id} | ${el.element_type} | ${label} | ${value} | ${actions} |`);
    }
    lines.push("");
  }

  if (diff.changed.length > 0) {
    lines.push("## Elements that CHANGED:");
    lines.push("");
    for (const c of diff.changed) {
      lines.push(`- ${c.element.id} (${c.element.element_type}): ${c.changes.join("; ")}`);
    }
    lines.push("");
  }

  lines.push(
    `${diff.unchangedCount} elements remain unchanged (not shown to save tokens).`,
  );
  if (diff.removed.length > 0) {
    lines.push(`${diff.removed.length} elements were removed.`);
  }

  return lines.join("\n");
}
