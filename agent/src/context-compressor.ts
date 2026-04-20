/**
 * Context Compressor — reduces accessibility tree token footprint.
 *
 * Inspired by WebTactix's snapshot_dedup.py (~1100 lines). Adapted for
 * Cellar's ContextElement[] structure (not YAML-based a11y snapshots).
 *
 * Four transforms applied in order:
 * 1. Wrapper stripping: remove structural-only containers, promote children
 * 2. Repetitive collapsing: collapse N identical siblings into a count
 * 3. Table truncation: keep first N rows, fold the rest
 * 4. Cross-snapshot dedup: collapse unchanged elements vs previous snapshot
 */

import type { ScreenContext, ContextElement } from "./types.js";
import { createHash } from "crypto";

// ─── Configuration ─────────────────────────────────────────────────────────

export interface CompressionOptions {
  /** Strip structural-only wrapper elements. Default true. */
  stripWrappers?: boolean;
  /** Collapse repetitive sibling groups. Default true. */
  collapseRepetitive?: boolean;
  /** Max table rows before truncation. Default 5. 0 = no truncation. */
  truncateTableRows?: number;
  /** Previous snapshot hash for cross-snapshot dedup. */
  dedupAgainst?: string;
}

// ─── Constants ─────────────────────────────────────────────────────────────

/** Element types that are structural wrappers (no semantic content). */
const WRAPPER_TYPES = new Set([
  "group",
  "toolbar",
  "scroll_area",
  "scroll_bar",
  "layout",
  "separator",
  "splitter",
  "generic",
  "section",
  "div",
  "span",
  "unknown",
]);

/** Element types that indicate table structure. */
const TABLE_TYPES = new Set(["table", "grid", "treegrid"]);

/** Element types that indicate table rows. */
const ROW_TYPES = new Set(["row", "table_row", "grid_row"]);

/** Minimum children of same type before collapsing. */
const COLLAPSE_THRESHOLD = 4;

// ─── Public API ────────────────────────────────────────────────────────────

/**
 * Compress a ScreenContext to reduce token usage.
 * Returns a new ScreenContext with compressed elements and a snapshot hash.
 */
export function compressContext(
  ctx: ScreenContext,
  options: CompressionOptions = {},
): { context: ScreenContext; snapshotHash: string } {
  const opts = {
    stripWrappers: options.stripWrappers ?? true,
    collapseRepetitive: options.collapseRepetitive ?? true,
    truncateTableRows: options.truncateTableRows ?? 5,
    dedupAgainst: options.dedupAgainst,
  };

  let elements = [...ctx.elements];

  // 1. Wrapper stripping
  if (opts.stripWrappers) {
    elements = stripWrappers(elements);
  }

  // 2. Repetitive collapsing
  if (opts.collapseRepetitive) {
    elements = collapseRepetitive(elements);
  }

  // 3. Table truncation
  if (opts.truncateTableRows > 0) {
    elements = truncateTables(elements, opts.truncateTableRows);
  }

  // 4. Cross-snapshot dedup
  const currentHash = computeSnapshotHash(elements);
  if (opts.dedupAgainst && opts.dedupAgainst === currentHash) {
    // Entire context is identical — return minimal summary
    elements = [{
      id: "compressed:unchanged",
      element_type: "summary",
      label: `[${ctx.elements.length} elements unchanged from previous snapshot]`,
      state: { focused: false, enabled: true, visible: true, selected: false },
      confidence: 1.0,
      source: "accessibility_tree" as const,
    }];
  } else if (opts.dedupAgainst) {
    // Partial dedup — handled at the element level via fingerprints
    // stored in the hash. For now we skip partial dedup (full snapshot
    // hash comparison is the 80/20 approach).
  }

  const compressed: ScreenContext = { ...ctx, elements };

  return { context: compressed, snapshotHash: currentHash };
}

// ─── Transform 1: Wrapper Stripping ───────────────────────────────────────

/**
 * Remove structural wrapper elements that add tree depth without info.
 * Promote their children up, preserving the parent_id chain.
 */
function stripWrappers(elements: ContextElement[]): ContextElement[] {
  // Build parent→children map
  const childrenOf = new Map<string | null, ContextElement[]>();
  for (const el of elements) {
    const pid = el.parent_id ?? null;
    if (!childrenOf.has(pid)) childrenOf.set(pid, []);
    childrenOf.get(pid)!.push(el);
  }

  // Identify wrapper IDs (structural-only, no label/value, no actions)
  const wrapperIds = new Set<string>();
  for (const el of elements) {
    if (
      WRAPPER_TYPES.has(el.element_type) &&
      !el.label &&
      !el.value &&
      (!el.actions || el.actions.length === 0) &&
      !el.state.focused
    ) {
      wrapperIds.add(el.id);
    }
  }

  if (wrapperIds.size === 0) return elements;

  // Reparent children of wrappers to their grandparent
  const result: ContextElement[] = [];
  for (const el of elements) {
    if (wrapperIds.has(el.id)) {
      // Skip this wrapper — its children will be reparented
      continue;
    }

    // Walk up parent chain to find first non-wrapper ancestor
    let newParent = el.parent_id ?? null;
    while (newParent && wrapperIds.has(newParent)) {
      const parent = elements.find((p) => p.id === newParent);
      newParent = parent?.parent_id ?? null;
    }

    result.push({
      ...el,
      parent_id: newParent,
    });
  }

  return result;
}

// ─── Transform 2: Repetitive Collapsing ──────────────────────────────────

/**
 * When a parent has >N children of the same type with no actions,
 * collapse them into a single summary element.
 */
function collapseRepetitive(elements: ContextElement[]): ContextElement[] {
  // Build parent→children map
  const childrenOf = new Map<string | null, ContextElement[]>();
  for (const el of elements) {
    const pid = el.parent_id ?? null;
    if (!childrenOf.has(pid)) childrenOf.set(pid, []);
    childrenOf.get(pid)!.push(el);
  }

  // Find groups to collapse
  const idsToRemove = new Set<string>();
  const summariesToAdd: ContextElement[] = [];

  for (const [parentId, children] of childrenOf) {
    // Group children by element_type
    const byType = new Map<string, ContextElement[]>();
    for (const child of children) {
      const t = child.element_type;
      if (!byType.has(t)) byType.set(t, []);
      byType.get(t)!.push(child);
    }

    for (const [type, group] of byType) {
      // Only collapse non-actionable groups above threshold
      const noActions = group.every(
        (el) => !el.actions || el.actions.length === 0,
      );
      if (group.length >= COLLAPSE_THRESHOLD && noActions) {
        // Keep the first element, collapse the rest
        const kept = group[0];
        for (let i = 1; i < group.length; i++) {
          idsToRemove.add(group[i].id);
        }
        // Add a summary annotation to the kept element
        summariesToAdd.push({
          ...kept,
          label: `${kept.label ?? type} [+${group.length - 1} similar ${type} items]`,
        });
        idsToRemove.add(kept.id); // remove original, add annotated
      }
    }
  }

  if (idsToRemove.size === 0) return elements;

  const result = elements.filter((el) => !idsToRemove.has(el.id));
  result.push(...summariesToAdd);
  return result;
}

// ─── Transform 3: Table Truncation ───────────────────────────────────────

/**
 * Detect table-like structures and truncate rows beyond maxRows.
 * Keeps header row + first maxRows data rows.
 */
function truncateTables(
  elements: ContextElement[],
  maxRows: number,
): ContextElement[] {
  // Find table elements
  const tableIds = new Set<string>();
  for (const el of elements) {
    if (TABLE_TYPES.has(el.element_type)) {
      tableIds.add(el.id);
    }
  }

  if (tableIds.size === 0) return elements;

  const idsToRemove = new Set<string>();
  const summariesToAdd: ContextElement[] = [];

  for (const tableId of tableIds) {
    // Find rows that are direct children of this table
    const rows = elements.filter(
      (el) => el.parent_id === tableId && ROW_TYPES.has(el.element_type),
    );

    if (rows.length <= maxRows + 1) continue; // +1 for header

    // Keep first maxRows+1, remove the rest
    const toRemove = rows.slice(maxRows + 1);
    const removedCount = toRemove.length;

    for (const row of toRemove) {
      idsToRemove.add(row.id);
      // Also remove all children of removed rows
      for (const el of elements) {
        if (el.parent_id === row.id) {
          idsToRemove.add(el.id);
        }
      }
    }

    // Add truncation notice
    summariesToAdd.push({
      id: `compressed:table-truncated:${tableId}`,
      element_type: "summary",
      label: `[${removedCount} more rows folded — use data extraction to see all]`,
      parent_id: tableId,
      state: { focused: false, enabled: false, visible: true, selected: false },
      confidence: 1.0,
      source: "accessibility_tree" as const,
    });
  }

  if (idsToRemove.size === 0) return elements;

  const result = elements.filter((el) => !idsToRemove.has(el.id));
  result.push(...summariesToAdd);
  return result;
}

// ─── Hashing ─────────────────────────────────────────────────────────────

/**
 * Compute a hash of the element list for cross-snapshot dedup.
 * Uses (id, element_type, label, value, state) as fingerprint inputs.
 */
function computeSnapshotHash(elements: ContextElement[]): string {
  const hash = createHash("sha256");
  for (const el of elements) {
    hash.update(
      `${el.id}|${el.element_type}|${el.label ?? ""}|${el.value ?? ""}|${el.state.focused}|${el.state.selected}|${el.state.expanded ?? ""}|${el.state.checked ?? ""}`,
    );
  }
  return hash.digest("hex").slice(0, 16); // 16 chars is enough for dedup
}
