/**
 * Element Mapper — converts RawDOMElement[] into ContextElement[]
 * with confidence scoring calibrated to match the Rust merger.
 *
 * This is the canonical mapper for the DOM-walk pipeline, which runs whenever
 * the adapter has only raw DOM data (via CDP `extractDOM` or `extractDOMAllFrames`).
 * Live callers include `BrowserAdapter.getContext()`, `getContextFast()`, and the
 * `MutationTracker` incremental updates.
 *
 * The Rust mapper in `cel-context/src/merge.rs` is the counterpart for the
 * accessibility-tree pipeline (macOS AX, and eventually CDP a11y). The two
 * mappers share scoring constants and output shape (ContextElement), but take
 * different inputs:
 *
 *   DOM walk  (RawDOMElement[])         → this file (mapElements)
 *   A11y tree (AccessibilityElement)    → cel-context/src/merge.rs (flatten_a11y_tree)
 *
 * If you change scoring logic here, update the Rust side to match, and vice versa.
 *
 * License: MIT
 */

import type { ContextElement, Bounds, ElementState } from "@cellar/agent";
import type { RawDOMElement } from "./dom-extractor.js";

// --- Confidence scoring constants ---
const BASE_CONFIDENCE = 0.7;
const BONUS_HAS_LABEL = 0.08;
const BONUS_HAS_BOUNDS = 0.06;
const BONUS_VISIBLE_ENABLED = 0.04;
const BONUS_ACTIONABLE = 0.04;
const BONUS_EXPLICIT_ROLE = 0.03;
const BONUS_MAIN_DOCUMENT = 0.03;

// --- ARIA role / tag → CEL element_type mapping ---

const ROLE_MAP: Record<string, string> = {
  button: "button",
  link: "link",
  textbox: "input",
  searchbox: "input",
  checkbox: "checkbox",
  radio: "radio_button",
  combobox: "combobox",
  listbox: "combobox",
  menuitem: "menu_item",
  menuitemcheckbox: "menu_item",
  menuitemradio: "menu_item",
  tab: "tab_item",
  slider: "slider",
  spinbutton: "input",
  switch: "checkbox",
  treeitem: "tree_item",
  option: "list_item",
  gridcell: "table_cell",
  dialog: "dialog",
  alertdialog: "dialog",
  menu: "menu",
  menubar: "menu",
  navigation: "toolbar",
  tablist: "group",
  toolbar: "toolbar",
  tree: "tree_view",
  grid: "table",
  table: "table",
  row: "table_row",
  rowheader: "table_cell",
  columnheader: "table_cell",
  cell: "table_cell",
  img: "image",
  figure: "image",
  status: "status_bar",
  progressbar: "slider",
  group: "group",
  region: "group",
  list: "list",
  listitem: "list_item",
  heading: "text",
  banner: "group",
  complementary: "group",
  contentinfo: "group",
  form: "group",
  main: "group",
  search: "group",
  article: "group",
};

const TAG_MAP: Record<string, string> = {
  button: "button",
  a: "link",
  input: "input",
  textarea: "input",
  select: "combobox",
  option: "list_item",
  details: "group",
  summary: "button",
  dialog: "dialog",
  nav: "toolbar",
  menu: "menu",
  table: "table",
  thead: "group",
  tbody: "group",
  tfoot: "group",
  tr: "table_row",
  th: "table_cell",
  td: "table_cell",
  ul: "list",
  ol: "list",
  li: "list_item",
  dl: "list",
  dt: "list_item",
  dd: "list_item",
  img: "image",
  svg: "image",
  video: "image",
  audio: "image",
  canvas: "image",
  h1: "text",
  h2: "text",
  h3: "text",
  h4: "text",
  h5: "text",
  h6: "text",
  label: "text",
  output: "text",
  meter: "slider",
  progress: "slider",
  form: "group",
  fieldset: "group",
  legend: "text",
  section: "group",
  article: "group",
  aside: "group",
  header: "group",
  footer: "group",
  main: "group",
  iframe: "group",
};

/** Input type → element_type overrides. */
const INPUT_TYPE_MAP: Record<string, string> = {
  submit: "button",
  reset: "button",
  button: "button",
  image: "button",
  checkbox: "checkbox",
  radio: "radio_button",
  range: "slider",
  file: "button",
};

const ACTIONABLE_TYPES = new Set([
  "button",
  "input",
  "link",
  "checkbox",
  "radio_button",
  "combobox",
  "slider",
  "menu_item",
  "tab_item",
  "tree_item",
]);

/** Map ARIA role / HTML tag to CEL element_type. */
function mapElementType(raw: RawDOMElement): string {
  // Explicit ARIA role takes precedence
  if (raw.role && ROLE_MAP[raw.role]) {
    return ROLE_MAP[raw.role];
  }

  // Input type overrides
  if (raw.tag === "input" && raw.type && INPUT_TYPE_MAP[raw.type]) {
    return INPUT_TYPE_MAP[raw.type];
  }

  // Tag-based mapping
  if (TAG_MAP[raw.tag]) {
    return TAG_MAP[raw.tag];
  }

  // Class-name based inference — elements acting as links/buttons without proper ARIA.
  // Runs for all tags including containers (div, span) since apps like MiniWoB++
  // use <span class="alink"> for links and <span class="trash"> for buttons.
  if (raw.className && typeof raw.className === "string") {
    const cls = raw.className.toLowerCase();
    if (cls.includes("link") || cls.includes("alink")) return "link";
    if (cls.includes("btn") || cls.includes("button")) return "button";
    if (cls.includes("tab")) return "tab";
    if (cls.includes("folder") || cls.includes("file")) return "link";
    if (cls.includes("thread") || cls.includes("item") || cls.includes("row")) return "button";
    if (cls.includes("star") || cls.includes("trash") || cls.includes("action")) return "button";
  }

  return "text";
}

/** Extract the best label from available sources. */
function extractLabel(raw: RawDOMElement): string | undefined {
  // Priority: aria-label > title > alt > placeholder > textContent > value > name > id
  if (raw.ariaLabel) return raw.ariaLabel;
  if (raw.attributes["aria-label"]) return raw.attributes["aria-label"];
  if (raw.attributes["title"]) return raw.attributes["title"];
  if (raw.tag === "img" && raw.attributes["alt"]) return raw.attributes["alt"];
  if (raw.placeholder) return raw.placeholder;
  if (raw.textContent) return raw.textContent;
  if (raw.value) return raw.value;
  // For inputs with no other label, use the HTML id or name as a hint.
  // This helps the planner distinguish between fields (e.g., "username" vs "password").
  if ((raw.tag === "input" || raw.tag === "textarea" || raw.tag === "select") && raw.id) {
    return raw.id;
  }
  return undefined;
}

/** Generate a unique CEL element ID. */
function generateId(raw: RawDOMElement): string {
  if (raw.iframeOrigin && !raw.id.startsWith("iframe:")) {
    const base = raw.id || `${raw.tag}:${raw.backendNodeId}`;
    return `iframe:${raw.iframeOrigin}:${base}`;
  }

  if (raw.shadowDepth > 0) {
    const base = raw.id || `${raw.tag}:${raw.backendNodeId}`;
    return `shadow:${raw.parentCelId || "root"}:${base}`;
  }

  if (raw.id) return `dom:${raw.id}`;
  return `dom:${raw.tag}:${raw.backendNodeId}`;
}

/** Determine available actions for an element type. */
function getActions(elementType: string, raw: RawDOMElement): string[] {
  switch (elementType) {
    case "button":
      return ["click", "press"];
    case "input":
      return ["activate", "set"];
    case "link":
      return ["click", "jump"];
    case "checkbox":
    case "radio_button":
      return ["toggle"];
    case "combobox":
      return ["select", "activate"];
    case "slider":
      return ["set"];
    case "menu_item":
    case "tab_item":
    case "tree_item":
    case "list_item":
      return ["click", "activate"];
    default:
      // If it has a click handler or pointer cursor, it's clickable
      if (raw.attributes["onclick"] || raw.attributes["tabindex"]) {
        return ["click"];
      }
      return [];
  }
}

/** Calculate confidence score for a DOM element. */
function calculateConfidence(
  raw: RawDOMElement,
  elementType: string,
  label: string | undefined,
): number {
  let confidence = BASE_CONFIDENCE;

  // +0.08 for having a label or visible text
  if (label && label.trim().length > 0) {
    confidence += BONUS_HAS_LABEL;
  }

  // +0.06 for having valid bounds
  if (raw.bounds && raw.bounds.width > 0 && raw.bounds.height > 0) {
    confidence += BONUS_HAS_BOUNDS;
  }

  // +0.04 for being visible and enabled
  if (raw.isVisible && raw.isEnabled) {
    confidence += BONUS_VISIBLE_ENABLED;
  }

  // +0.04 for being an actionable type
  if (ACTIONABLE_TYPES.has(elementType)) {
    confidence += BONUS_ACTIONABLE;
  }

  // +0.03 for having an explicit ARIA role
  if (raw.role) {
    confidence += BONUS_EXPLICIT_ROLE;
  }

  // +0.03 for being in the main document
  if (raw.shadowDepth === 0 && !raw.iframeOrigin) {
    confidence += BONUS_MAIN_DOCUMENT;
  }

  return Math.min(confidence, 0.98);
}

/** Map a single RawDOMElement to a ContextElement. */
function mapElement(raw: RawDOMElement): ContextElement {
  const elementType = mapElementType(raw);
  const label = extractLabel(raw);
  let confidence = calculateConfidence(raw, elementType, label);

  // Penalize occluded elements — they're covered by other UI and likely not actionable
  if (raw.isOccluded) {
    confidence = Math.max(confidence - 0.3, 0.1);
  }

  const bounds: Bounds | undefined = raw.bounds
    ? {
        x: raw.bounds.x,
        y: raw.bounds.y,
        width: raw.bounds.width,
        height: raw.bounds.height,
      }
    : undefined;

  const state: ElementState = {
    focused: raw.isFocused,
    enabled: raw.isEnabled,
    visible: raw.isVisible && !raw.isOccluded,
    selected: raw.isSelected,
    expanded: raw.isExpanded,
    checked: raw.isChecked,
  };

  const actions = getActions(elementType, raw);

  // Build extended properties
  const properties: Record<string, string> = {};

  // Scroll position awareness
  if (raw.viewportRelation === "below" && raw.pagesBelow > 0) {
    properties.scroll_hint = `~${raw.pagesBelow} page${raw.pagesBelow > 1 ? "s" : ""} down`;
  } else if (raw.viewportRelation === "above") {
    properties.scroll_hint = "above viewport";
  }

  // Action targeting data — enables smart execution cascade:
  // href → navigate, backend_node_id → CDP click, css_selector → locator
  if (raw.href) properties.href = raw.href;
  if (raw.backendNodeId) properties.backend_node_id = String(raw.backendNodeId);
  if (raw.placeholder) properties.placeholder = raw.placeholder;

  // Expose input type for specialized handling (date pickers, etc.)
  if (raw.tag === "input" && raw.type) {
    properties.input_type = raw.type;
    if (raw.type === "date" || raw.type === "datetime-local" || raw.type === "time") {
      properties.settable = "true";
    }
    // Detect jQuery/custom datepicker inputs (type=text with datepicker in id or label)
    const idLower = (raw.id || "").toLowerCase();
    const labelLower = (raw.ariaLabel || "").toLowerCase();
    if (raw.type === "text" && (idLower.includes("date") || labelLower.includes("date"))) {
      properties.settable = "true";
      properties.input_type = "datepicker";
    }
  }

  // Build a CSS selector for Playwright locator targeting
  const selector = buildCssSelector(raw);
  if (selector) properties.css_selector = selector;

  return {
    id: generateId(raw),
    label,
    description: raw.ariaDescription || undefined,
    element_type: elementType,
    value: raw.value || undefined,
    bounds,
    state,
    parent_id: raw.parentCelId || null,
    actions: actions.length > 0 ? actions : undefined,
    confidence,
    source: "native_api",
    properties: Object.keys(properties).length > 0 ? properties : undefined,
  };
}

/**
 * Build a CSS selector for Playwright locator targeting.
 * Priority: #id > [data-testid] > tag[aria-label] > tag:nth-of-type
 */
function buildCssSelector(raw: RawDOMElement): string | null {
  // DOM id is most reliable
  const rawId = typeof raw.id === 'string' ? raw.id : raw.id != null ? String(raw.id) : '';
  if (rawId && !rawId.includes(":") && /^[a-zA-Z][\w-]*$/.test(rawId)) {
    return `#${rawId}`;
  }
  // data-testid (common in React/Vue apps)
  if (raw.attributes?.["data-testid"]) {
    return `[data-testid="${raw.attributes["data-testid"]}"]`;
  }
  // aria-label — skip if contains newlines (Fix C: causes selector failures)
  if (raw.ariaLabel && raw.ariaLabel.length < 60 && !raw.ariaLabel.includes('\n')) {
    const escaped = raw.ariaLabel.replace(/"/g, '\\"');
    return `${raw.tag}[aria-label="${escaped}"]`;
  }
  // Tag + role (for ARIA-annotated elements)
  if (raw.role && raw.role !== raw.tag) {
    return `${raw.tag}[role="${raw.role}"]`;
  }
  // Tag + class (fallback for elements without id/aria-label/role)
  if (raw.className && typeof raw.className === 'string') {
    const classes = raw.className.trim().split(/\s+/).filter(c => c.length > 1 && c.length < 40);
    if (classes.length > 0) {
      return `${raw.tag}.${classes[0]}`;
    }
  }
  // Tag + name attribute (for form elements)
  if (raw.attributes?.name) {
    return `${raw.tag}[name="${raw.attributes.name}"]`;
  }
  // Tag + text content (last resort for short text elements)
  const text = (raw.textContent || '').trim();
  if (text && text.length < 30 && !text.includes('"')) {
    return `${raw.tag}:text("${text}")`;
  }
  return null;
}

// Tags that indicate interactive elements (for occlusion filtering)
const INTERACTIVE_TAGS_SET = new Set([
  "a", "button", "input", "select", "textarea", "details", "summary",
]);

// ARIA roles that indicate interactive elements
const INTERACTIVE_ROLES_SET = new Set([
  "button", "link", "textbox", "checkbox", "radio", "combobox", "listbox",
  "menuitem", "menuitemcheckbox", "menuitemradio", "option", "slider",
  "spinbutton", "switch", "tab", "treeitem", "searchbox", "gridcell",
]);

/**
 * Map an array of RawDOMElements to ContextElements.
 * Filters out fully occluded non-interactive elements, then sorts by confidence.
 */
export function mapElements(rawElements: RawDOMElement[]): ContextElement[] {
  // Filter out fully occluded elements that aren't interactive
  // (occluded interactive elements are kept but with reduced confidence)
  const filtered = rawElements.filter((raw) => {
    if (!raw.isOccluded) return true;
    // Keep occluded elements if they're interactive (might be in a stacking context)
    if (INTERACTIVE_ROLES_SET.has(raw.role) || INTERACTIVE_TAGS_SET.has(raw.tag)) {
      return true;
    }
    return false;
  });

  const mapped = filtered.map(mapElement);

  // Sort by confidence descending (matches Rust merger output convention)
  mapped.sort((a, b) => b.confidence - a.confidence);

  return mapped;
}
