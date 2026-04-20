/**
 * Context Serializer — numbered element index for LLM consumption.
 *
 * Converts ScreenContext into a compact text representation with sequential
 * element indices, inspired by Browser-use's proven format:
 *   [1]<button>Submit</button>
 *   [2]<input placeholder="Email" />
 *   *[3]<button>New Option</button>  (* = new since last snapshot)
 *
 * The LLM references elements by index number, and the indexMap maps
 * indices back to element IDs for action execution.
 *
 * Benefits:
 * - 60-80% smaller than raw JSON element dumps
 * - Sequential numbers are easier for LLMs to reference than UUIDs
 * - New element markers help the LLM notice UI changes (dropdowns, modals)
 * - Scroll hints show below-viewport elements without wasting tokens
 */

import type { ContextElement, ScreenContext } from "./types.js";

/** Result of serializing context for LLM consumption. */
export interface SerializedContext {
  /** Human-readable text representation of the context. */
  text: string;
  /** Map from sequential index (1-based) to element ID. */
  indexMap: Map<number, string>;
  /** Total number of indexed elements. */
  elementCount: number;
}

/** Element types that should be indexed (actionable). */
const INDEXED_TYPES = new Set([
  "button", "input", "link", "checkbox", "radio_button",
  "combobox", "slider", "menu_item", "tab_item", "tree_item",
  "list_item", "table_cell", "dialog",
]);

/** Element types that provide structural context (not indexed). */
const STRUCTURAL_TYPES = new Set([
  "group", "toolbar", "list", "table", "table_row",
  "tree_view", "menu", "status_bar",
]);

/**
 * Serialize a ScreenContext into a compact indexed text for LLM prompts.
 *
 * @param context - Current screen context
 * @param previousContext - Previous context for change detection (* prefix on new elements)
 */
export function serializeContextForLLM(
  context: ScreenContext,
  previousContext?: ScreenContext,
): SerializedContext {
  const indexMap = new Map<number, string>();
  const previousIds = new Set<string>();

  if (previousContext) {
    for (const el of previousContext.elements) {
      previousIds.add(el.id);
    }
  }

  const lines: string[] = [];
  let index = 0;

  // Header
  lines.push(`[${context.app || "App"}] ${context.window || ""}`);
  lines.push("");

  // Group elements by parent for indentation
  const childMap = new Map<string | null, ContextElement[]>();
  for (const el of context.elements) {
    if (!el.state.visible) continue;
    const parent = el.parent_id ?? null;
    if (!childMap.has(parent)) childMap.set(parent, []);
    childMap.get(parent)!.push(el);
  }

  function renderElement(el: ContextElement, depth: number): void {
    const indent = "  ".repeat(depth);
    const isNew = previousContext && !previousIds.has(el.id);
    const shouldIndex = INDEXED_TYPES.has(el.element_type) ||
      (el.actions && el.actions.length > 0);

    if (shouldIndex) {
      index++;
      indexMap.set(index, el.id);

      const prefix = isNew ? "*" : "";
      const tag = el.element_type;
      const label = el.label ? ` "${el.label}"` : "";
      const value = el.value ? ` value="${el.value}"` : "";
      const scrollHint = el.properties?.scroll_hint
        ? ` (${el.properties.scroll_hint})`
        : "";
      const stateStr = formatState(el);

      lines.push(
        `${indent}${prefix}[${index}]<${tag}${label}${value}${stateStr} />${scrollHint}`,
      );
    } else if (STRUCTURAL_TYPES.has(el.element_type)) {
      // Structural element — show as context without index
      const label = el.label ? ` "${el.label}"` : "";
      lines.push(`${indent}<${el.element_type}${label}>`);
    }

    // Render children
    const children = childMap.get(el.id);
    if (children) {
      for (const child of children) {
        renderElement(child, depth + 1);
      }
    }
  }

  // Render root-level elements (no parent)
  const roots = childMap.get(null) ?? [];
  for (const el of roots) {
    renderElement(el, 0);
  }

  // Also render any elements whose parent wasn't in the context
  // (orphaned children — their parent was filtered out)
  const renderedIds = new Set<string>();
  function collectRendered(parentId: string | null) {
    const children = childMap.get(parentId) ?? [];
    for (const el of children) {
      renderedIds.add(el.id);
      collectRendered(el.id);
    }
  }
  collectRendered(null);

  for (const [parentId, children] of childMap) {
    if (parentId === null) continue;
    if (renderedIds.has(parentId)) continue;
    // This parent wasn't rendered — render its children at root level
    for (const el of children) {
      if (!renderedIds.has(el.id)) {
        renderElement(el, 0);
      }
    }
  }

  return {
    text: lines.join("\n"),
    indexMap,
    elementCount: index,
  };
}

/** Format element state as compact attributes. */
function formatState(el: ContextElement): string {
  const parts: string[] = [];
  if (!el.state.enabled) parts.push("disabled");
  if (el.state.focused) parts.push("focused");
  if (el.state.checked === true) parts.push("checked");
  if (el.state.checked === false && el.element_type === "checkbox") parts.push("unchecked");
  if (el.state.expanded === true) parts.push("expanded");
  if (el.state.expanded === false) parts.push("collapsed");
  if (el.state.selected) parts.push("selected");
  return parts.length > 0 ? ` [${parts.join(",")}]` : "";
}

/**
 * Resolve an element index back to its element ID.
 * Used by the goal runner after the LLM references an element by index.
 */
export function resolveIndex(
  indexMap: Map<number, string>,
  index: number,
): string | undefined {
  return indexMap.get(index);
}
