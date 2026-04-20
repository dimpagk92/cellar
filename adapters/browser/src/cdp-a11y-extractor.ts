/**
 * CDP Accessibility Tree Extractor
 *
 * Uses Chrome DevTools Protocol's Accessibility.getFullAXTree to extract
 * the browser's semantic accessibility tree. This is richer than a DOM walk
 * because it includes computed roles, names, and ARIA state from the
 * browser's internal accessibility model.
 *
 * Inspired by Stagehand v3's approach to using CDP a11y as the primary
 * context source (80-90% token reduction vs raw DOM).
 */

import type { CdpChannel } from "./cdp-channel.js";

/** A node from the CDP accessibility tree, after pruning. */
export interface A11yNode {
  /** CDP node ID. */
  nodeId: string;
  /** Backend DOM node ID for correlating with DOM data. */
  backendDOMNodeId: number;
  /** Accessibility role (button, textbox, link, etc.) */
  role: string;
  /** Computed accessible name. */
  name: string;
  /** Current value (input fields, etc.) */
  value?: string;
  /** Accessible description. */
  description?: string;
  /** Child nodes. */
  children: A11yNode[];
  /** Raw properties from CDP. */
  properties: Record<string, unknown>;
  /** Whether the node is focused. */
  focused?: boolean;
  /** Whether the node is disabled. */
  disabled?: boolean;
  /** Whether the node is expanded (for tree items, details, etc.) */
  expanded?: boolean | null;
  /** Whether the node is checked (checkboxes, radio buttons). */
  checked?: string; // "true", "false", "mixed"
  /** Whether the node is selected. */
  selected?: boolean;
}

/** Roles to prune from the tree (structural noise with no semantic value). */
const PRUNE_ROLES = new Set([
  "generic",
  "none",
  "presentation",
  "InlineTextBox",
  "LineBreak",
]);

/**
 * Extract the full accessibility tree from a CDP session.
 * Prunes structural noise and returns a clean semantic tree.
 */
export async function extractA11yTree(
  cdp: CdpChannel,
): Promise<A11yNode[]> {
  // Enable the Accessibility domain
  await cdp.send("Accessibility.enable", {});

  // Get the full tree
  const result = (await cdp.send("Accessibility.getFullAXTree", {})) as {
    nodes: CdpAXNode[];
  };

  if (!result.nodes || result.nodes.length === 0) {
    return [];
  }

  // Build the tree from flat node array
  const tree = buildTree(result.nodes);

  // Prune structural noise
  return pruneTree(tree);
}

/**
 * Extract the a11y tree for a specific frame by its frame ID.
 */
export async function extractA11yTreeForFrame(
  cdp: CdpChannel,
  frameId: string,
): Promise<A11yNode[]> {
  await cdp.send("Accessibility.enable", {});

  const result = (await cdp.send("Accessibility.getFullAXTree", {
    frameId,
  })) as { nodes: CdpAXNode[] };

  if (!result.nodes || result.nodes.length === 0) {
    return [];
  }

  return pruneTree(buildTree(result.nodes));
}

// ─── Internal types ──────────────────────────────────────────────────────────

interface CdpAXNode {
  nodeId: string;
  ignored?: boolean;
  role?: { type: string; value: string };
  name?: { type: string; value: string; sources?: unknown[] };
  description?: { type: string; value: string };
  value?: { type: string; value: unknown };
  properties?: Array<{ name: string; value: { type: string; value: unknown } }>;
  childIds?: string[];
  backendDOMNodeId?: number;
  parentId?: string;
}

// ─── Tree building ───────────────────────────────────────────────────────────

function buildTree(nodes: CdpAXNode[]): A11yNode[] {
  const nodeMap = new Map<string, A11yNode>();
  const childMap = new Map<string, string[]>();

  // First pass: create A11yNode for each CDP node
  for (const node of nodes) {
    if (node.ignored) continue;

    const role = node.role?.value ?? "none";
    const name = node.name?.value ?? "";
    const value = node.value?.value;

    // Extract properties
    const props: Record<string, unknown> = {};
    let focused = false;
    let disabled = false;
    let expanded: boolean | null = null;
    let checked: string | undefined;
    let selected = false;

    if (node.properties) {
      for (const prop of node.properties) {
        props[prop.name] = prop.value.value;
        switch (prop.name) {
          case "focused":
            focused = prop.value.value === true;
            break;
          case "disabled":
            disabled = prop.value.value === true;
            break;
          case "expanded":
            expanded = prop.value.value as boolean;
            break;
          case "checked":
            checked = String(prop.value.value);
            break;
          case "selected":
            selected = prop.value.value === true;
            break;
        }
      }
    }

    const a11yNode: A11yNode = {
      nodeId: node.nodeId,
      backendDOMNodeId: node.backendDOMNodeId ?? 0,
      role,
      name,
      value: value != null ? String(value) : undefined,
      description: node.description?.value,
      children: [],
      properties: props,
      focused,
      disabled,
      expanded,
      checked,
      selected,
    };

    nodeMap.set(node.nodeId, a11yNode);

    if (node.childIds) {
      childMap.set(node.nodeId, node.childIds);
    }
  }

  // Second pass: wire up children
  for (const [parentId, childIds] of childMap) {
    const parent = nodeMap.get(parentId);
    if (!parent) continue;
    for (const childId of childIds) {
      const child = nodeMap.get(childId);
      if (child) {
        parent.children.push(child);
      }
    }
  }

  // Find root nodes (no parent)
  const childNodeIds = new Set<string>();
  for (const childIds of childMap.values()) {
    for (const id of childIds) childNodeIds.add(id);
  }

  const roots: A11yNode[] = [];
  for (const [id, node] of nodeMap) {
    if (!childNodeIds.has(id)) {
      roots.push(node);
    }
  }

  return roots;
}

// ─── Pruning ─────────────────────────────────────────────────────────────────

function pruneTree(nodes: A11yNode[]): A11yNode[] {
  const result: A11yNode[] = [];

  for (const node of nodes) {
    const pruned = pruneNode(node);
    if (pruned) {
      result.push(pruned);
    }
  }

  return result;
}

function pruneNode(node: A11yNode): A11yNode | null {
  // Prune structural roles with no name/value
  if (PRUNE_ROLES.has(node.role) && !node.name && !node.value) {
    // But keep children — "lift" them up
    const liftedChildren: A11yNode[] = [];
    for (const child of node.children) {
      const pruned = pruneNode(child);
      if (pruned) liftedChildren.push(pruned);
    }
    // If only structural wrapper with one child, return the child
    if (liftedChildren.length === 1) return liftedChildren[0];
    // If multiple children, keep them (they'll be returned as root-level)
    if (liftedChildren.length > 1) {
      // Can't lift multiple children to same level without wrapper — keep as generic group
      return { ...node, children: liftedChildren };
    }
    return null;
  }

  // Remove redundant StaticText nodes whose text equals parent's name
  // (handled at parent level — see below)

  // Recurse into children
  const prunedChildren: A11yNode[] = [];
  for (const child of node.children) {
    // Skip redundant StaticText
    if (
      child.role === "StaticText" &&
      child.name === node.name &&
      !child.value
    ) {
      continue;
    }

    const pruned = pruneNode(child);
    if (pruned) prunedChildren.push(pruned);
  }

  return { ...node, children: prunedChildren };
}

/**
 * Flatten an A11yNode tree into a flat array (depth-first).
 * Useful for mapping to ContextElement[].
 */
export function flattenA11yTree(
  nodes: A11yNode[],
  parentId?: string,
): Array<A11yNode & { parentNodeId?: string; depth: number }> {
  const result: Array<A11yNode & { parentNodeId?: string; depth: number }> = [];

  function walk(node: A11yNode, parent: string | undefined, depth: number): void {
    result.push({ ...node, parentNodeId: parent, depth });
    for (const child of node.children) {
      walk(child, node.nodeId, depth + 1);
    }
  }

  for (const node of nodes) {
    walk(node, parentId, 0);
  }

  return result;
}
