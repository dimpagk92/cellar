/**
 * Hybrid Snapshot — CDP Accessibility Tree + DOM XPath Map + URL Map
 *
 * Combines the browser's CDP accessibility tree (semantic, 80-90% smaller
 * than raw DOM) with DOM XPath mappings and URL extraction to produce a
 * rich, structured snapshot suitable for LLM consumption.
 *
 * This replaces the previous JS-based DOM walk with a CDP-native approach
 * inspired by Stagehand v3's hybrid snapshot architecture.
 *
 * Key improvements:
 * - Uses Accessibility.getFullAXTree (browser-computed roles, names, states)
 * - Adds XPath mapping for each element (stable selector references)
 * - Builds URL map for anti-hallucination
 * - OOPIF support via Page.getFrameTree
 * - Adaptive DOM depth fallback
 */

import type { Page } from "playwright";
import type { ContextElement, Bounds, ElementState } from "@cellar/agent";
import type { CdpChannel } from "./cdp-channel.js";
import {
  extractA11yTree,
  flattenA11yTree,
  type A11yNode,
} from "./cdp-a11y-extractor.js";
import { UrlMap } from "./url-map.js";

/** Frame info for multi-frame support. */
export interface FrameInfo {
  id: string;
  url: string;
  parentId?: string;
  name?: string;
}

/** The complete hybrid snapshot. */
export interface HybridSnapshot {
  /** Mapped context elements with confidence scores. */
  elements: ContextElement[];
  /** Element ID → XPath selector mapping. */
  xpathMap: Map<string, string>;
  /** URL map for anti-hallucination. */
  urlMap: UrlMap;
  /** Frame tree for OOPIF support. */
  frameTree: FrameInfo[];
}

// ─── Element type mapping, confidence scoring, and action assignment ─────────
//
// REMOVED: These are now handled by the Rust CEL core via build_from_external().
// - aria_role_to_cel_type() in cel-context/src/merge.rs normalizes ARIA roles
// - score_element_confidence() in cel-context/src/merge.rs scores elements
// - assign_default_actions() in cel-context/src/merge.rs assigns actions
//
// Elements returned by createHybridSnapshot() have raw ARIA roles as element_type,
// confidence=0.0, and actions=[]. The BrowserAdapter.getContext() routes them
// through Rust for the canonical pipeline.

// ─── Main snapshot function ──────────────────────────────────────────────────

/**
 * Create a hybrid snapshot combining CDP accessibility tree with DOM data.
 *
 * @param cdp - CDP channel for accessibility tree and DOM queries
 * @param page - Optional Playwright page for frame enumeration and bounds
 */
export async function createHybridSnapshot(
  cdp: CdpChannel,
  page?: Page,
): Promise<HybridSnapshot> {
  // 1. Get frame tree for OOPIF support
  const frameTree = await getFrameTree(cdp);

  // 2. Extract accessibility tree via CDP
  const a11yTree = await extractA11yTree(cdp);

  // 3. Flatten tree for mapping
  const flatNodes = flattenA11yTree(a11yTree);

  // 4. Get DOM bounds for elements (a11y tree doesn't include geometry)
  const boundsMap = await getBoundsForNodes(cdp, flatNodes);

  // 5. Build XPath map for each element
  const xpathMap = new Map<string, string>();

  // 6. Map A11yNodes to ContextElements
  const elements: ContextElement[] = [];
  let counter = 0;

  for (const node of flatNodes) {
    // Skip nodes with no semantic value (no name, no value, generic role, leaf)
    if (!node.name && !node.value && node.children.length === 0) {
      const role = node.role;
      if (role === "generic" || role === "none" || role === "presentation" || role === "StaticText") {
        continue;
      }
    }

    counter++;
    const id = `a11y:${node.backendDOMNodeId || counter}`;
    const bounds = boundsMap.get(node.backendDOMNodeId);

    const state: ElementState = {
      focused: node.focused ?? false,
      enabled: !node.disabled,
      visible: bounds ? bounds.width > 0 && bounds.height > 0 : true,
      selected: node.selected ?? false,
      expanded: node.expanded ?? null,
      checked: node.checked === "true" ? true : node.checked === "false" ? false : null,
    };

    // Pass raw ARIA role as element_type — Rust normalizes via aria_role_to_cel_type()
    // Pass confidence=0 and actions=[] — Rust scores and assigns via build_from_external()
    const element: ContextElement = {
      id,
      label: node.name || undefined,
      description: node.description || undefined,
      element_type: node.role, // Raw ARIA role — Rust normalizes
      value: node.value || undefined,
      bounds: bounds || undefined,
      state,
      parent_id: node.parentNodeId ? `a11y:${findBackendId(flatNodes, node.parentNodeId)}` : null,
      confidence: 0, // Rust scores via score_element_confidence()
      source: "accessibility_tree",
    };

    elements.push(element);

    // Build XPath entry (backendDOMNodeId → simple XPath)
    if (node.backendDOMNodeId) {
      xpathMap.set(id, `//*[@data-cel-id="${id}"]`);
    }
  }

  // 6.5. DOM enrichment: discover interactive elements the A11y tree missed,
  // and infer semantic purpose from CSS classes, ARIA, and DOM context.
  try {
    const existingLabels = new Set(
      elements.filter(e => e.label).map(e => e.label!.substring(0, 20))
    );

    const hiddenInteractives = await cdp.evaluate<Array<{
      tag: string;
      text: string;
      role: string;
      semanticType: string;
      context: string;
      position: string;
      href?: string;
      hidden: boolean;
    }>>(`(() => {
      const results = [];
      const seen = new Set();

      // CSS class → semantic purpose mapping
      function inferSemanticType(el) {
        const cls = (el.className || '').toLowerCase();
        const ariaLabel = (el.getAttribute('aria-label') || '').toLowerCase();
        const title = (el.getAttribute('title') || '').toLowerCase();
        const all = cls + ' ' + ariaLabel + ' ' + title;

        // Action buttons
        if (all.includes('like') || all.includes('heart') || all.includes('favorite')) return 'like-button';
        if (all.includes('reply') || all.includes('comment')) return 'reply-button';
        if (all.includes('retweet') || all.includes('repost') || all.includes('share')) return 'share-button';
        if (all.includes('bookmark') || all.includes('save')) return 'bookmark-button';
        if (all.includes('delete') || all.includes('trash') || all.includes('remove')) return 'delete-button';
        if (all.includes('edit') || all.includes('pencil')) return 'edit-button';
        if (all.includes('close') || all.includes('dismiss') || all.includes('×')) return 'close-button';
        if (all.includes('menu') || all.includes('more') || all.includes('dots') || all.includes('ellipsis')) return 'menu-button';
        if (all.includes('star') || all.includes('rating')) return 'star-button';
        if (all.includes('search')) return 'search-button';
        if (all.includes('submit') || all.includes('send')) return 'submit-button';
        if (all.includes('copy')) return 'copy-button';
        if (all.includes('mute') || all.includes('block') || all.includes('report')) return cls.split(/\\s+/).find(c => /mute|block|report/.test(c)) + '-button';
        if (all.includes('download')) return 'download-button';
        if (all.includes('upload')) return 'upload-button';
        if (all.includes('expand') || all.includes('collapse') || all.includes('toggle')) return 'toggle-button';
        if (all.includes('next') || all.includes('forward')) return 'next-button';
        if (all.includes('prev') || all.includes('back')) return 'prev-button';
        if (all.includes('play') || all.includes('pause')) return 'media-button';

        // Navigation / structural
        if (all.includes('tab')) return 'tab';
        if (all.includes('nav') || all.includes('breadcrumb')) return 'nav-item';
        if (all.includes('dropdown')) return 'dropdown';
        if (all.includes('modal') || all.includes('dialog')) return 'dialog';
        if (all.includes('carousel') || all.includes('slider')) return 'slider';
        if (all.includes('pagination') || all.includes('page-link') || all.includes('page-number')) return 'pagination-link';

        // Search results
        if (all.includes('search-result') || all.includes('search-title') || all.includes('result-link') || all.includes('result-title')) return 'search-result';
        if (all.includes('product-title') || all.includes('item-title') || all.includes('listing-title')) return 'search-result';

        // Data display
        if (all.includes('username') || all.includes('user-name') || all.includes('author')) return 'username';
        if (all.includes('avatar') || all.includes('profile-pic')) return 'avatar';
        if (all.includes('timestamp') || all.includes('date') || all.includes('time-ago')) return 'timestamp';
        if (all.includes('badge') || all.includes('tag') || all.includes('chip')) return 'badge';
        if (all.includes('price') || all.includes('cost') || all.includes('amount')) return 'price';

        return '';
      }

      // Find nearest context (e.g., which user a button belongs to)
      function getContext(el) {
        let parent = el.closest('[data-user], [data-username], .tweet, .post, .comment, .email, .message, .card, .item, .row, .media, article');
        if (!parent) return '';
        const userEl = parent.querySelector('.username, .user-name, .author, [data-user], .from, .sender, .name');
        return userEl ? userEl.textContent.trim().substring(0, 30) : '';
      }

      const candidates = document.querySelectorAll(
        'a[href], [onclick], [role="button"], [role="link"], [role="tab"], ' +
        '[tabindex], [data-action], [class*="btn"], [aria-label], ' +
        '[class*="like"], [class*="reply"], [class*="share"], [class*="star"], ' +
        '[class*="delete"], [class*="edit"], [class*="menu"], [class*="more"], ' +
        '[class*="mute"], [class*="block"], [class*="report"], [class*="copy"]'
      );

      for (const el of candidates) {
        if (seen.has(el)) continue;
        seen.add(el);
        const tag = el.tagName.toLowerCase();
        if (['body', 'html', 'head', 'script', 'style', 'svg', 'path'].includes(tag)) continue;

        const text = (el.textContent || '').trim().substring(0, 100);
        const ariaLabel = el.getAttribute('aria-label') || '';
        const title = el.getAttribute('title') || '';
        const role = el.getAttribute('role') || (tag === 'a' ? 'link' : tag === 'button' ? 'button' : '');
        const semanticType = inferSemanticType(el);
        const context = getContext(el);
        const hidden = el.closest('[style*="display: none"], [style*="visibility: hidden"], .hide, .hidden, [aria-hidden="true"]') !== null
          || getComputedStyle(el).display === 'none';

        // Include if it has semantic meaning or text
        if (!semanticType && !text && !ariaLabel) continue;

        // Build positional info for search results and pagination
        let position = '';
        if (semanticType === 'search-result') {
          const parent = el.closest('[data-result], .search-results, .results, ol, ul');
          if (parent) {
            const siblings = Array.from(parent.querySelectorAll('[class*="result"], [class*="item"], li'));
            const idx = siblings.indexOf(el.closest('[class*="result"], [class*="item"], li') || el);
            if (idx >= 0) position = 'result #' + (idx + 1);
          }
        } else if (semanticType === 'pagination-link') {
          const pageNum = (text || ariaLabel).match(/\\d+/);
          if (pageNum) position = 'page ' + pageNum[0];
        }

        results.push({
          tag,
          text: text || ariaLabel || title || semanticType,
          role: semanticType || role || tag,
          semanticType,
          context,
          position,
          href: el.getAttribute('href') || undefined,
          hidden,
        });

        if (results.length >= 40) break;
      }
      return results;
    })()`);

    if (hiddenInteractives) {
      for (const item of hiddenInteractives) {
        // Skip if already in the a11y tree by text
        if (item.text && existingLabels.has(item.text.substring(0, 20))) continue;

        counter++;
        const id = `dom:${counter}`;

        // Build description with semantic context and position info
        let description = "";
        if (item.semanticType) {
          description += item.semanticType.toUpperCase();
          if (item.position) description += ` (${item.position})`;
        }
        if (item.context) description += (description ? " for " : "") + item.context;
        if (item.href && item.href !== "#") description += (description ? " " : "") + `href: ${item.href}`;
        if (item.hidden) description += (description ? " " : "") + "(hidden)";

        // Boost confidence for semantically typed elements (from 0.3 to 0.6)
        const confidence = item.semanticType ? 0.6 : 0.3;

        elements.push({
          id,
          label: item.text || item.semanticType || undefined,
          element_type: item.role || item.tag,
          value: undefined,
          description: description || undefined,
          bounds: undefined,
          state: {
            focused: false,
            enabled: true,
            visible: !item.hidden,
            selected: false,
          },
          parent_id: null,
          confidence,
          source: "dom_enrichment" as any,
        });
      }
    }
  } catch {
    // DOM enrichment is best-effort — don't fail the snapshot
  }

  // 7. Build URL map
  const urlMap = UrlMap.build(elements);

  return { elements, xpathMap, urlMap, frameTree };
}

// ─── Helper: get frame tree ──────────────────────────────────────────────────

async function getFrameTree(cdp: CdpChannel): Promise<FrameInfo[]> {
  try {
    const result = (await cdp.send("Page.getFrameTree", {})) as {
      frameTree: CdpFrameTree;
    };
    return flattenFrameTree(result.frameTree);
  } catch {
    return [];
  }
}

interface CdpFrameTree {
  frame: { id: string; url: string; parentId?: string; name?: string };
  childFrames?: CdpFrameTree[];
}

function flattenFrameTree(tree: CdpFrameTree): FrameInfo[] {
  const result: FrameInfo[] = [
    {
      id: tree.frame.id,
      url: tree.frame.url,
      parentId: tree.frame.parentId,
      name: tree.frame.name,
    },
  ];
  if (tree.childFrames) {
    for (const child of tree.childFrames) {
      result.push(...flattenFrameTree(child));
    }
  }
  return result;
}

// ─── Helper: get bounds for nodes via CDP ────────────────────────────────────

async function getBoundsForNodes(
  cdp: CdpChannel,
  nodes: Array<A11yNode & { depth: number }>,
): Promise<Map<number, Bounds>> {
  const boundsMap = new Map<number, Bounds>();

  // Batch resolve: get box model for each backend DOM node
  // Use DOM.getBoxModel which is efficient for bulk lookups
  const promises: Array<Promise<void>> = [];

  for (const node of nodes) {
    if (!node.backendDOMNodeId || node.backendDOMNodeId === 0) continue;

    const p = (async () => {
      try {
        const result = (await cdp.send("DOM.getBoxModel", {
          backendNodeId: node.backendDOMNodeId,
        })) as { model?: { content: number[] } };

        if (result.model?.content) {
          const c = result.model.content;
          // content quad: [x1,y1, x2,y2, x3,y3, x4,y4]
          const x = Math.min(c[0], c[2], c[4], c[6]);
          const y = Math.min(c[1], c[3], c[5], c[7]);
          const maxX = Math.max(c[0], c[2], c[4], c[6]);
          const maxY = Math.max(c[1], c[3], c[5], c[7]);
          boundsMap.set(node.backendDOMNodeId, {
            x: Math.round(x),
            y: Math.round(y),
            width: Math.round(maxX - x),
            height: Math.round(maxY - y),
          });
        }
      } catch {
        // Node may not have a box model (hidden, detached, etc.)
      }
    })();

    promises.push(p);

    // Batch in groups of 50 to avoid overwhelming CDP
    if (promises.length >= 50) {
      await Promise.all(promises);
      promises.length = 0;
    }
  }

  if (promises.length > 0) {
    await Promise.all(promises);
  }

  return boundsMap;
}

// ─── Helper: find backend ID by node ID ──────────────────────────────────────

function findBackendId(
  nodes: Array<A11yNode & { parentNodeId?: string }>,
  nodeId: string,
): number {
  const node = nodes.find((n) => n.nodeId === nodeId);
  return node?.backendDOMNodeId ?? 0;
}
