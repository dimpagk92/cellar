/**
 * DOM Extractor — walks the DOM tree including shadow DOMs and iframes.
 *
 * Runs a single Runtime.evaluate call inside the page context to extract
 * all interactive and landmark elements. This avoids N round-trips per
 * element (browser-use's bottleneck) by doing everything in one JS execution.
 *
 * Improvements over prior version (informed by Stagehand + Browser-use):
 * - Paint order capture (z-index + DOM position) for occlusion detection
 * - Scroll position awareness (viewport relation + pages below)
 * - Expanded interactive element detection (event handlers, framework attrs, cursor, labels)
 * - Closed shadow DOM capture via __cel_closedShadows WeakMap
 *
 * License: MIT
 */

import type { Page, Frame } from "playwright";
import type { CdpChannel } from "./cdp-channel.js";

/** Anything that can evaluate JS — Playwright Page/Frame or raw CdpChannel. */
export type Evaluator = Page | Frame | CdpChannel;

/**
 * Raw DOM element descriptor — extracted in the browser context,
 * mapped to ContextElement by element-mapper.ts.
 */
export interface RawDOMElement {
  /** Unique incrementing ID assigned during walk. */
  backendNodeId: number;
  tag: string;
  /** DOM id attribute. */
  id: string;
  /** Computed ARIA role. */
  role: string;
  ariaLabel: string;
  ariaDescription: string;
  /** First 200 chars of innerText. */
  textContent: string;
  /** Current value (inputs, textareas, selects). */
  value: string;
  /** Input type attribute. */
  type: string;
  /** href for links. */
  href: string;
  /** Placeholder text. */
  placeholder: string;
  bounds: { x: number; y: number; width: number; height: number } | null;
  isVisible: boolean;
  isEnabled: boolean;
  isFocused: boolean;
  isChecked: boolean | null;
  isExpanded: boolean | null;
  isSelected: boolean;
  /** CEL ID of the parent element. */
  parentCelId: string;
  /** 0 = main document, 1+ = shadow DOM depth. */
  shadowDepth: number;
  /** Origin of the iframe this element is in, or null for main document. */
  iframeOrigin: string | null;
  /** CSS class name string. */
  className: string;
  /** Filtered attributes: data-*, aria-* only. */
  attributes: Record<string, string>;
  /** Paint order for occlusion detection (z-index stacking, DOM order as tiebreaker). */
  paintOrder: number;
  /** Whether the element's background is opaque (for occlusion computation). */
  isOpaque: boolean;
  /** Position relative to the current viewport. */
  viewportRelation: "visible" | "above" | "below";
  /** How many viewport-heights below the current scroll position (0 if visible/above). */
  pagesBelow: number;
  /** Whether this element is fully occluded by higher-paint-order opaque elements. */
  isOccluded: boolean;
  /**
   * For `<select>` elements: the list of `<option>` children captured
   * as { value, label }. Empty / undefined for non-select elements
   * or for selects with no options at extraction time.
   *
   * The planner needs the actual `value` attribute (which is what
   * `set_value` dispatches) — without this, run-6 evidence showed
   * the model guessing slugs like `"general-inquiry"` against a
   * select whose real option values were `"1"`, `"2"`, `"3"`, and
   * failing with `no-option:select:subject:general-inquiry` 3 trials
   * in a row.
   */
  selectOptions?: Array<{ value: string; label: string }>;
}

/** Viewport metadata captured alongside DOM elements. */
export interface ViewportInfo {
  scrollX: number;
  scrollY: number;
  innerWidth: number;
  innerHeight: number;
  scrollHeight: number;
  scrollWidth: number;
}

/** Result of DOM extraction: elements + viewport info. */
export interface ExtractionResult {
  elements: RawDOMElement[];
  viewport: ViewportInfo;
}

/**
 * The JS function injected into the page to walk the DOM.
 * Returns { elements: RawDOMElement[], viewport: ViewportInfo }.
 *
 * Key design: everything happens in a single evaluate() call —
 * no round-trips per element.
 */
const EXTRACTION_SCRIPT = `(() => {
  const MAX_TEXT_LENGTH = 200;
  const MAX_DEPTH = 20;
  const MAX_ELEMENTS = 5000;

  const SKIP_TAGS = new Set([
    'SCRIPT', 'STYLE', 'NOSCRIPT', 'META', 'LINK', 'HEAD', 'BR', 'HR', 'WBR',
    'TEMPLATE', 'SLOT', 'BASE', 'COL', 'COLGROUP', 'SOURCE', 'TRACK', 'PARAM',
  ]);

  const INTERACTIVE_TAGS = new Set([
    'A', 'BUTTON', 'INPUT', 'SELECT', 'TEXTAREA', 'DETAILS', 'SUMMARY',
    'LABEL', 'OPTION', 'FIELDSET', 'LEGEND', 'OUTPUT', 'METER', 'PROGRESS',
  ]);

  const LANDMARK_TAGS = new Set([
    'NAV', 'MAIN', 'ASIDE', 'HEADER', 'FOOTER', 'SECTION', 'ARTICLE',
    'FORM', 'TABLE', 'THEAD', 'TBODY', 'TFOOT', 'TR', 'TH', 'TD',
    'UL', 'OL', 'LI', 'DL', 'DT', 'DD', 'DIALOG', 'MENU',
    'H1', 'H2', 'H3', 'H4', 'H5', 'H6', 'IMG', 'VIDEO', 'AUDIO', 'CANVAS',
    'IFRAME', 'SVG',
  ]);

  const INTERACTIVE_ROLES = new Set([
    'button', 'link', 'textbox', 'checkbox', 'radio', 'combobox', 'listbox',
    'menuitem', 'menuitemcheckbox', 'menuitemradio', 'option', 'slider',
    'spinbutton', 'switch', 'tab', 'treeitem', 'searchbox', 'gridcell',
    'row', 'cell',
  ]);

  // Event handler attributes that indicate interactivity
  const EVENT_ATTRS = [
    'onclick', 'onmousedown', 'onmouseup', 'onpointerdown', 'ontouchstart',
    'onkeydown', 'onkeypress',
  ];

  // Framework-specific interactive attributes
  const FRAMEWORK_ATTRS = [
    'data-action', 'ng-click', 'v-on:click', '@click',
  ];

  const scrollY = window.scrollY || window.pageYOffset || 0;
  const scrollX = window.scrollX || window.pageXOffset || 0;
  const innerW = window.innerWidth;
  const innerH = window.innerHeight;

  let nodeCounter = 0;
  const results = [];

  function isVisible(el) {
    if (el.offsetWidth === 0 && el.offsetHeight === 0 && !el.getClientRects().length) {
      return false;
    }
    const style = getComputedStyle(el);
    if (style.display === 'none' || style.visibility === 'hidden') return false;
    if (parseFloat(style.opacity) === 0) return false;
    return true;
  }

  function getRole(el) {
    return el.getAttribute('role') || '';
  }

  function hasEventHandlers(el) {
    for (let i = 0; i < EVENT_ATTRS.length; i++) {
      if (el.hasAttribute(EVENT_ATTRS[i])) return true;
    }
    for (let i = 0; i < FRAMEWORK_ATTRS.length; i++) {
      if (el.hasAttribute(FRAMEWORK_ATTRS[i])) return true;
    }
    return false;
  }

  function isInteractive(el, role) {
    if (INTERACTIVE_TAGS.has(el.tagName)) return true;
    if (role && INTERACTIVE_ROLES.has(role)) return true;
    if (el.hasAttribute('tabindex')) return true;
    if (el.getAttribute('contenteditable') === 'true') return true;
    if (hasEventHandlers(el)) return true;
    // CSS cursor: pointer
    try {
      if (getComputedStyle(el).cursor === 'pointer') return true;
    } catch {}
    // Labels wrapping form controls
    if (el.tagName === 'LABEL' && el.querySelector('input, select, textarea')) return true;
    return false;
  }

  function shouldExtract(el, role) {
    if (INTERACTIVE_TAGS.has(el.tagName)) return true;
    if (LANDMARK_TAGS.has(el.tagName)) return true;
    if (role && INTERACTIVE_ROLES.has(role)) return true;
    if (el.hasAttribute('role')) return true;
    if (el.hasAttribute('aria-label')) return true;
    if (el.hasAttribute('tabindex')) return true;
    if (el.getAttribute('contenteditable') === 'true') return true;
    // React/Vue data-testid elements — meaningful content containers (e.g. hotel cards, product tiles)
    if (el.hasAttribute('data-testid')) return true;
    if (hasEventHandlers(el)) return true;
    // CSS cursor: pointer
    try {
      if (getComputedStyle(el).cursor === 'pointer') return true;
    } catch {}
    // Small clickable-looking elements (dialog close buttons, icon buttons)
    var text = (el.textContent || '').trim();
    if (text.length <= 3 && /^[x×✕✖✗☒⨯]$/i.test(text)) return true;
    // Elements with clickable-sounding class names
    if (el.className && typeof el.className === 'string') {
      var cls = el.className.toLowerCase();
      if (cls.includes('close') || cls.includes('btn') || cls.includes('icon') ||
          cls.includes('link') || cls.includes('action') || cls.includes('thread') ||
          cls.includes('star') || cls.includes('trash') || cls.includes('forward') ||
          cls.includes('reply') || cls.includes('delete') || cls.includes('toggle') ||
          cls.includes('clickable') || cls.includes('selectable')) return true;
    }
    return false;
  }

  function getText(el) {
    // For inputs, don't use textContent
    if (el.tagName === 'INPUT' || el.tagName === 'SELECT') return '';
    const text = (el.innerText || el.textContent || '').trim();
    return text.slice(0, MAX_TEXT_LENGTH);
  }

  /** Find associated label text for form elements (input, textarea, select). */
  function getAssociatedLabel(el) {
    if (el.tagName !== 'INPUT' && el.tagName !== 'TEXTAREA' && el.tagName !== 'SELECT') return '';
    // 1. <label for="id"> association
    if (el.id) {
      var label = document.querySelector('label[for="' + el.id + '"]');
      if (label) return (label.textContent || '').trim().slice(0, 100);
    }
    // 2. Ancestor <label> (input wrapped in label)
    var parent = el.parentElement;
    for (var d = 0; d < 3 && parent; d++) {
      if (parent.tagName === 'LABEL') {
        var text = '';
        for (var c = 0; c < parent.childNodes.length; c++) {
          if (parent.childNodes[c].nodeType === 3) text += parent.childNodes[c].textContent;
        }
        text = text.trim();
        if (text) return text.slice(0, 100);
      }
      parent = parent.parentElement;
    }
    // 3. Previous sibling text (common pattern: <span>Username</span><input>)
    var prev = el.previousElementSibling;
    if (prev && (prev.tagName === 'LABEL' || prev.tagName === 'SPAN' || prev.tagName === 'P' || prev.tagName === 'DIV')) {
      var prevText = (prev.textContent || '').trim();
      if (prevText && prevText.length < 50) return prevText;
    }
    return '';
  }

  /** Infer a label for empty interactive elements from siblings or parent context. */
  function inferLabel(el) {
    // Only infer from siblings when the element has no text of its own.
    // Elements with their own text content (buttons, links) should use that text,
    // not steal the next sibling's label.
    var ownText = (el.textContent || '').trim();
    if (ownText && ownText.length > 0 && !/^[x×✕✖✗☒⨯]$/i.test(ownText)) {
      return '';
    }
    // For hitarea/toggle divs: use next sibling's text (e.g., folder name).
    // Only infer from siblings when the element's class hints it's a trigger for
    // neighboring content — otherwise inputs/buttons without own text would
    // incorrectly pick up the next sibling's label (e.g., placeholder-only inputs).
    var cls = (el.className && typeof el.className === 'string') ? el.className.toLowerCase() : '';
    var isHitarea = cls.includes('hitarea') || cls.includes('toggle') || cls.includes('expand') || cls.includes('collapse');
    if (isHitarea) {
      var next = el.nextElementSibling;
      if (next) {
        var nextText = (next.textContent || '').trim();
        if (nextText && nextText.length < 50) {
          return 'Expand ' + nextText;
        }
      }
    }
    // For close/dismiss buttons
    if (/^[x×✕]$/i.test(ownText)) return 'Close';
    return '';
  }

  function getFilteredAttributes(el) {
    const attrs = {};
    for (const attr of el.attributes) {
      if (attr.name.startsWith('data-') || attr.name.startsWith('aria-')) {
        attrs[attr.name] = attr.value.slice(0, 100);
      }
    }
    return attrs;
  }

  function getBounds(el) {
    try {
      const rect = el.getBoundingClientRect();
      if (rect.width === 0 && rect.height === 0) return null;
      return {
        x: Math.round(rect.x),
        y: Math.round(rect.y),
        width: Math.round(rect.width),
        height: Math.round(rect.height),
      };
    } catch {
      return null;
    }
  }

  /** Compute paint order from z-index stacking context + DOM position. */
  function getPaintOrder(el, domIndex) {
    try {
      const style = getComputedStyle(el);
      const zIndex = parseInt(style.zIndex, 10);
      // z-index only applies to positioned elements
      const isPositioned = style.position !== 'static';
      if (isPositioned && !isNaN(zIndex)) {
        // Offset by a large number so z-indexed elements always sort above non-z-indexed
        return 100000 + zIndex * 1000 + domIndex;
      }
    } catch {}
    return domIndex;
  }

  /** Check if element has an opaque background (for occlusion computation). */
  function checkOpaque(el) {
    try {
      const style = getComputedStyle(el);
      const bg = style.backgroundColor;
      // rgba(0,0,0,0) or transparent = not opaque
      if (!bg || bg === 'transparent' || bg === 'rgba(0, 0, 0, 0)') return false;
      const opacity = parseFloat(style.opacity);
      if (isNaN(opacity) || opacity < 0.8) return false;
      return true;
    } catch {
      return false;
    }
  }

  /** Get viewport relation for an element's bounds. */
  function getViewportRelation(bounds) {
    if (!bounds) return { relation: 'visible', pagesBelow: 0 };
    const bottom = bounds.y + bounds.height;
    if (bottom < 0) return { relation: 'above', pagesBelow: 0 };
    if (bounds.y > innerH) {
      const pb = Math.floor((bounds.y - innerH) / innerH) + 1;
      return { relation: 'below', pagesBelow: pb };
    }
    return { relation: 'visible', pagesBelow: 0 };
  }

  function walkDOM(root, parentCelId, shadowDepth, iframeOrigin, depth) {
    if (!root || depth > MAX_DEPTH || results.length >= MAX_ELEMENTS) return;

    var children;
    try {
      children = root.children || root.childNodes;
    } catch { return; }
    if (!children) return;
    for (let i = 0; i < children.length; i++) {
      if (results.length >= MAX_ELEMENTS) return;
      const el = children[i];
      if (!el || !el.nodeType) continue;
      if (el.nodeType !== 1) continue; // Element nodes only
      if (!el.tagName || SKIP_TAGS.has(el.tagName)) continue;

      const role = getRole(el);
      const visible = isVisible(el);

      // Skip invisible subtrees — but still check shadow roots (open + closed)
      var shadowRoot = null;
      try {
        shadowRoot = el.shadowRoot
          || (window.__cel_closedShadows && window.__cel_closedShadows.get(el));
      } catch {}
      if (!visible && !shadowRoot) {
        continue;
      }

      const extract = visible && shouldExtract(el, role);
      let celId = parentCelId;

      if (extract) {
        nodeCounter++;
        const id = el.id
          ? 'dom:' + el.id
          : 'dom:' + el.tagName.toLowerCase() + ':' + nodeCounter;

        celId = id;

        const checked = el.type === 'checkbox' || el.type === 'radio'
          ? el.checked
          : el.getAttribute('aria-checked') === 'true'
            ? true
            : el.getAttribute('aria-checked') === 'false'
              ? false
              : null;

        const expanded = el.hasAttribute('aria-expanded')
          ? el.getAttribute('aria-expanded') === 'true'
          : el.tagName === 'DETAILS'
            ? el.open
            : null;

        const bounds = getBounds(el);
        const vp = getViewportRelation(bounds);

        // For <select>, enumerate option children so the planner sees
        // exact option values up front. Without this, run-6 caught the
        // planner emitting set_value with a guessed slug ("Test", or
        // "general-inquiry") against a select whose real option values
        // were different — fails with no-option:select:subject:Test.
        // Cap at 50 options to bound prompt size on enormous selects
        // (country pickers etc.); 50 is enough to fingerprint the
        // shape so the planner can see "this is a small enum" vs
        // "this is a long list".
        // NOTE: this entire string is sent verbatim to the page via
        // Runtime.evaluate — V8 sees it as plain JS, NOT TypeScript.
        // TS type annotations here (colon-shape) parse as labelled
        // statements in JS and break with "Unexpected token ':'".
        // Pre-2026-05-25 this declaration was annotated and the
        // LIGHTWEIGHT_SCRIPT fallback covered for it on most sites;
        // flightaware / mta.info / cloudflare-fronted pages caused
        // full extraction to fail and pushed the planner into
        // extract-only mode for the whole task. Annotation-free only.
        // (Do not use backticks in this comment block — the entire
        // outer string is a TS template literal.)
        let selectOptions;
        if (el.tagName === 'SELECT') {
          const opts = [];
          const optEls = el.querySelectorAll('option');
          const cap = Math.min(optEls.length, 50);
          for (let oi = 0; oi < cap; oi++) {
            const opt = optEls[oi];
            opts.push({
              value: String((opt.value !== undefined ? opt.value : '') || ''),
              label: String((opt.textContent || '').trim()).slice(0, 80),
            });
          }
          selectOptions = opts;
        }

        results.push({
          backendNodeId: nodeCounter,
          tag: el.tagName.toLowerCase(),
          id: el.id || '',
          role: role,
          ariaLabel: el.getAttribute('aria-label') || getAssociatedLabel(el) || inferLabel(el) || '',
          ariaDescription: el.getAttribute('aria-description') || el.getAttribute('aria-describedby') || '',
          textContent: getText(el),
          value: el.value !== undefined ? String(el.value || '') : '',
          type: el.type || '',
          href: el.href || el.getAttribute('href') || '',
          placeholder: el.placeholder || '',
          bounds: bounds,
          isVisible: visible,
          isEnabled: !el.disabled && el.getAttribute('aria-disabled') !== 'true',
          isFocused: document.activeElement === el,
          isChecked: checked,
          isExpanded: expanded,
          isSelected: el.selected || el.getAttribute('aria-selected') === 'true',
          parentCelId: parentCelId,
          shadowDepth: shadowDepth,
          iframeOrigin: iframeOrigin,
          className: (el.className && typeof el.className === 'string') ? el.className : '',
          attributes: getFilteredAttributes(el),
          selectOptions: selectOptions,
          paintOrder: getPaintOrder(el, nodeCounter),
          isOpaque: checkOpaque(el),
          viewportRelation: vp.relation,
          pagesBelow: vp.pagesBelow,
          isOccluded: false, // computed in post-processing
        });
      }

      // Recurse into shadow DOM (open + closed via __cel_closedShadows)
      if (shadowRoot) {
        walkDOM(shadowRoot, celId, shadowDepth + 1, iframeOrigin, depth + 1);
      }

      // Recurse into same-origin iframes
      if (el.tagName === 'IFRAME') {
        try {
          const iframeDoc = el.contentDocument;
          if (iframeDoc) {
            const origin = el.src ? new URL(el.src, location.href).origin : location.origin;
            walkDOM(iframeDoc.body || iframeDoc, celId, shadowDepth, origin, depth + 1);
          }
        } catch {
          // Cross-origin iframe — can't access contentDocument
        }
      }

      // Recurse into children
      walkDOM(el, celId, shadowDepth, iframeOrigin, depth + 1);
    }
  }

  const root = document.body || document.documentElement;
  if (root) {
    walkDOM(root, '', 0, null, 0);
  }

  const docEl = document.documentElement;
  return {
    elements: results,
    viewport: {
      scrollX: scrollX,
      scrollY: scrollY,
      innerWidth: innerW,
      innerHeight: innerH,
      scrollHeight: docEl ? docEl.scrollHeight : 0,
      scrollWidth: docEl ? docEl.scrollWidth : 0,
    },
  };
})()`;

// ─── Occlusion Detection (post-processing) ──────────────────────────────────

/**
 * Detect occluded elements using paint order and bounding rectangles.
 * Inspired by Browser-use's disjoint rectangle algorithm.
 *
 * Process elements in reverse paint order. For each element, check if its
 * bounds are fully covered by previously-seen opaque elements. If so,
 * mark as occluded.
 */
function computeOcclusion(elements: RawDOMElement[]): void {
  // Sort by paint order descending (highest = frontmost first)
  const sorted = [...elements]
    .filter((e) => e.bounds && e.isVisible)
    .sort((a, b) => b.paintOrder - a.paintOrder);

  // Occupied rectangles from higher-paint-order opaque elements
  const occupied: Array<{ x: number; y: number; x2: number; y2: number }> = [];

  for (const el of sorted) {
    if (!el.bounds) continue;

    const { x, y, width, height } = el.bounds;
    const x2 = x + width;
    const y2 = y + height;

    // Check if this element is fully covered by any occupied rectangle
    let fullyOccluded = false;
    for (const rect of occupied) {
      if (x >= rect.x && y >= rect.y && x2 <= rect.x2 && y2 <= rect.y2) {
        fullyOccluded = true;
        break;
      }
    }

    if (fullyOccluded) {
      el.isOccluded = true;
    }

    // Add this element's bounds to occupied if it's opaque
    if (el.isOpaque && width > 0 && height > 0) {
      occupied.push({ x, y, x2, y2 });
    }
  }
}

// ─── Closed Shadow DOM Capture ───────────────────────────────────────────────

/**
 * JavaScript to inject early (before page loads) to capture closed shadow roots.
 * Patches Element.prototype.attachShadow to store closed roots in a WeakMap.
 */
export const CLOSED_SHADOW_PATCH = `
  if (!window.__cel_closedShadows) {
    window.__cel_closedShadows = new WeakMap();
    const origAttach = Element.prototype.attachShadow;
    Element.prototype.attachShadow = function(init) {
      const shadow = origAttach.call(this, init);
      if (init.mode === 'closed') {
        window.__cel_closedShadows.set(this, shadow);
      }
      return shadow;
    };
  }
`;

// ─── Public API ──────────────────────────────────────────────────────────────

/**
 * Extract DOM elements from a page, frame, or CDP channel.
 * Returns elements with occlusion detection applied.
 */
export async function extractDOM(
  evaluator: Evaluator,
): Promise<RawDOMElement[]> {
  try {
    // Cast to any to resolve union type incompatibility between Playwright and CdpChannel evaluate()
    const result = await (evaluator as any).evaluate(EXTRACTION_SCRIPT) as ExtractionResult;
    if (!result || !result.elements) return [];
    const elements = result.elements;
    computeOcclusion(elements);
    return elements;
  } catch (error) {
    // Page may have navigated, been closed, or context destroyed (e.g. BBC News navigation).
    // "Execution context was destroyed" is common when a page navigates mid-extraction.
    const msg = error instanceof Error ? error.message : String(error);
    const isContextDestroyed = msg.includes("context was destroyed") || msg.includes("context is not available") || msg.includes("Target closed");
    if (!isContextDestroyed) {
      console.warn("DOM extraction failed:", error);
    }
    // Fall back to lightweight extraction that avoids deep reference chains.
    try {
      return await extractDOMLightweight(evaluator);
    } catch {
      return [];
    }
  }
}

/**
 * Lightweight DOM extraction — used as fallback when full extraction fails
 * (e.g., "Object reference chain is too long" on deeply nested pages).
 * Only extracts visible interactive elements and page text, avoiding deep tree walks.
 */
export async function extractDOMLightweight(evaluator: Evaluator): Promise<RawDOMElement[]> {
  const LIGHTWEIGHT_SCRIPT = `(() => {
    const results = [];
    // Get interactive elements only (buttons, links, inputs, selects)
    const interactiveSelectors = 'a, button, input, select, textarea, [role="button"], [role="link"], [role="tab"], [role="menuitem"], [role="checkbox"], [role="radio"]';
    const interactive = document.querySelectorAll(interactiveSelectors);
    for (let i = 0; i < Math.min(interactive.length, 100); i++) {
      const el = interactive[i];
      const rect = el.getBoundingClientRect();
      if (rect.width === 0 && rect.height === 0) continue;
      const tag = el.tagName.toLowerCase();
      const role = el.getAttribute('role') || '';
      const type = el.getAttribute('type') || '';
      // Find associated label for form elements
      let ariaLabel = el.getAttribute('aria-label') || '';
      if (!ariaLabel && (tag === 'input' || tag === 'textarea' || tag === 'select')) {
        if (el.id) {
          const lbl = document.querySelector('label[for="' + el.id + '"]');
          if (lbl) ariaLabel = (lbl.textContent || '').trim().slice(0, 100);
        }
        if (!ariaLabel) {
          let p = el.parentElement;
          for (let d = 0; d < 3 && p; d++) {
            if (p.tagName === 'LABEL') { ariaLabel = (p.textContent || '').trim().slice(0, 100); break; }
            p = p.parentElement;
          }
        }
        if (!ariaLabel) {
          const prev = el.previousElementSibling;
          if (prev && ['LABEL','SPAN','P','DIV'].includes(prev.tagName)) {
            const pt = (prev.textContent || '').trim();
            if (pt && pt.length < 50) ariaLabel = pt;
          }
        }
      }
      // Mirror the full extractor: capture option values for <select>
      // so the planner sees usable set_value targets even in the
      // lightweight fallback path.
      let selectOptions = undefined;
      if (tag === 'select') {
        const opts = [];
        const optEls = el.querySelectorAll('option');
        const cap = Math.min(optEls.length, 50);
        for (let oi = 0; oi < cap; oi++) {
          const opt = optEls[oi];
          opts.push({
            value: String((opt.value !== undefined ? opt.value : '') || ''),
            label: String((opt.textContent || '').trim()).slice(0, 80),
          });
        }
        selectOptions = opts;
      }
      results.push({
        tag,
        role,
        type,
        text: (el.textContent || '').trim().slice(0, 200),
        ariaLabel,
        placeholder: el.getAttribute('placeholder') || '',
        href: el.getAttribute('href') || '',
        name: el.getAttribute('name') || '',
        className: el.className || '',
        id: el.id || ('lw-' + i),
        value: el.value || '',
        bounds: { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
        visible: rect.width > 0 && rect.height > 0 && getComputedStyle(el).display !== 'none',
        disabled: el.disabled || false,
        selectOptions: selectOptions,
      });
    }
    return { elements: results };
  })()`;

  const result = await (evaluator as any).evaluate(LIGHTWEIGHT_SCRIPT) as { elements: any[] };
  return result.elements.map((el: any, i: number): RawDOMElement => ({
    backendNodeId: i,
    tag: el.tag,
    id: el.id || `lw-${i}`,
    role: el.role || el.tag,
    ariaLabel: el.ariaLabel || "",
    ariaDescription: "",
    textContent: el.text || "",
    value: el.value || "",
    type: el.type || "",
    href: el.href || "",
    placeholder: el.placeholder || "",
    bounds: el.bounds,
    isVisible: el.visible ?? true,
    isEnabled: !el.disabled,
    isFocused: false,
    isChecked: null,
    isExpanded: null,
    isSelected: false,
    parentCelId: "",
    shadowDepth: 0,
    iframeOrigin: null,
    className: el.className || "",
    attributes: {},
    paintOrder: 0,
    isOpaque: false,
    viewportRelation: "visible",
    pagesBelow: 0,
    isOccluded: false,
    selectOptions: el.selectOptions,
  }));
}

/**
 * Extract DOM elements with viewport metadata.
 */
export async function extractDOMWithViewport(
  evaluator: Evaluator,
): Promise<ExtractionResult> {
  try {
    const result = await (evaluator as any).evaluate(EXTRACTION_SCRIPT) as ExtractionResult;
    computeOcclusion(result.elements);
    return result;
  } catch (error) {
    console.warn("DOM extraction failed:", error);
    return { elements: [], viewport: { scrollX: 0, scrollY: 0, innerWidth: 0, innerHeight: 0, scrollHeight: 0, scrollWidth: 0 } };
  }
}

/**
 * Extract DOM from all frames (main + iframes).
 * Cross-origin iframes are accessed via Playwright's frame API.
 */
export async function extractDOMAllFrames(
  page: Page,
): Promise<RawDOMElement[]> {
  // Extract from main frame
  const mainElements = await extractDOM(page);

  // Extract from child frames (handles cross-origin)
  const frames = page.frames();
  for (const frame of frames) {
    if (frame === page.mainFrame()) continue;
    try {
      const url = frame.url();
      if (!url || url === "about:blank") continue;

      const origin = new URL(url).origin;
      const frameElements = await extractDOM(frame);

      // Prefix IDs to avoid collision and mark iframe origin
      for (const el of frameElements) {
        el.backendNodeId += mainElements.length + 10000;
        if (!el.id.startsWith("iframe:")) {
          const baseId = el.id || `dom:${el.tag}:${el.backendNodeId}`;
          el.id = `iframe:${origin}:${baseId}`;
        }
        el.iframeOrigin = el.iframeOrigin || origin;
      }

      mainElements.push(...frameElements);
    } catch {
      // Frame may have been removed or is inaccessible
    }
  }

  return mainElements;
}
