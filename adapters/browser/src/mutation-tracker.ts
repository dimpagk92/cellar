/**
 * Mutation Tracker — MutationObserver-based incremental DOM updates.
 *
 * Instead of re-extracting the entire DOM every step (browser-use's 5-30s),
 * we inject a MutationObserver and only process changes.
 *
 * License: MIT
 */

import type { Page } from "playwright";
import type { ContextElement } from "@cellar/agent/runtime";
import { extractDOM, extractDOMLightweight, type RawDOMElement, type Evaluator } from "./dom-extractor.js";
import { mapElements } from "./element-mapper.js";
import { sanitizeElements } from "./sanitizer.js";

/** Maximum pending mutations before forcing a full re-extraction. */
const MUTATION_FLOOD_THRESHOLD = 500;

/** Staleness penalty per 30s without confirmation. */
const STALENESS_PENALTY = 0.05;

/** Staleness timeout in milliseconds (30 seconds). */
const STALENESS_TIMEOUT_MS = 30_000;

interface MutationRecord {
  type: "childList" | "attributes" | "characterData";
  targetId: string;
  addedIds: string[];
  removedIds: string[];
  attributeName: string | null;
  newValue: string | null;
}

/** Script injected into the page to set up the MutationObserver. */
const OBSERVER_SCRIPT = `(() => {
  if (window.__cel_observer) return 'already_installed';

  window.__cel_mutations = [];

  const observer = new MutationObserver((mutations) => {
    for (const m of mutations) {
      const target = m.target;
      const targetId = target.id
        ? 'dom:' + target.id
        : target.tagName
          ? 'dom:' + target.tagName.toLowerCase() + ':' + (target.__celNodeId || 0)
          : '';

      const record = {
        type: m.type,
        targetId: targetId,
        addedIds: [],
        removedIds: [],
        attributeName: m.attributeName,
        newValue: m.type === 'attributes' && m.attributeName
          ? target.getAttribute(m.attributeName)
          : m.type === 'characterData'
            ? (target.textContent || '').slice(0, 200)
            : null,
      };

      if (m.addedNodes) {
        for (const node of m.addedNodes) {
          if (node.nodeType === 1) {
            record.addedIds.push(
              node.id ? 'dom:' + node.id : 'dom:' + (node.tagName || '').toLowerCase() + ':added'
            );
          }
        }
      }

      if (m.removedNodes) {
        for (const node of m.removedNodes) {
          if (node.nodeType === 1) {
            record.removedIds.push(
              node.id ? 'dom:' + node.id : 'dom:' + (node.tagName || '').toLowerCase() + ':removed'
            );
          }
        }
      }

      window.__cel_mutations.push(record);
    }

    // Cap at 1000 pending mutations
    if (window.__cel_mutations.length > 1000) {
      window.__cel_mutations = window.__cel_mutations.slice(-500);
    }
  });

  observer.observe(document, {
    childList: true,
    attributes: true,
    characterData: true,
    subtree: true,
    attributeFilter: [
      'class', 'style', 'disabled', 'hidden', 'aria-label', 'aria-hidden',
      'aria-expanded', 'aria-selected', 'aria-checked', 'aria-disabled',
      'value', 'checked', 'selected', 'role', 'href', 'src',
    ],
  });

  window.__cel_observer = observer;
  return 'installed';
})()`;

/** Script to drain pending mutations from the page. */
const DRAIN_SCRIPT = `(() => {
  const mutations = window.__cel_mutations || [];
  window.__cel_mutations = [];
  return mutations;
})()`;

export class MutationTracker {
  private cache: Map<string, ContextElement> = new Map();
  private lastUrl: string = "";
  private lastExtractionTime: number = 0;
  private observerInstalled: boolean = false;
  private sanitize: boolean;

  constructor(options?: { sanitize?: boolean }) {
    this.sanitize = options?.sanitize ?? true;
  }

  /**
   * Get elements — uses incremental updates when possible,
   * falls back to full extraction when needed.
   */
  async getElements(page: Evaluator, currentUrl?: string): Promise<ContextElement[]> {
    const url = currentUrl ?? (typeof (page as any).url === "function" ? (page as any).url() : "");

    // Full re-extraction if: first call, URL changed, or observer not installed
    if (
      this.cache.size === 0 ||
      url !== this.lastUrl ||
      !this.observerInstalled
    ) {
      return this.fullExtraction(page);
    }

    // Drain mutations
    const mutations = await this.drainMutations(page);

    // If mutation flood, do full re-extraction
    if (mutations.length >= MUTATION_FLOOD_THRESHOLD) {
      return this.fullExtraction(page);
    }

    // If mutations exist, apply them
    if (mutations.length > 0) {
      await this.applyMutations(page, mutations);
    }

    // Apply staleness penalties
    this.applyStalenessPenalties();

    return Array.from(this.cache.values()).sort(
      (a, b) => b.confidence - a.confidence,
    );
  }

  /** URLs where full DOM extraction failed — use lightweight on retry. */
  private failedUrls = new Set<string>();

  /** Force a full re-extraction, replacing the entire cache. */
  async fullExtraction(page: Evaluator): Promise<ContextElement[]> {
    const currentUrl = typeof (page as any).url === "function" ? (page as any).url() : "";

    // If this URL previously failed full extraction, go straight to lightweight
    let rawElements: RawDOMElement[];
    if (this.failedUrls.has(currentUrl)) {
      rawElements = await extractDOMLightweight(page);
    } else {
      rawElements = await extractDOM(page);
      // If full extraction returned empty (failed internally), mark URL and try lightweight
      if (rawElements.length === 0) {
        this.failedUrls.add(currentUrl);
        try {
          rawElements = await extractDOMLightweight(page);
        } catch {
          rawElements = [];
        }
      }
    }

    let elements = mapElements(rawElements);

    if (this.sanitize) {
      elements = sanitizeElements(elements);
    }

    // Rebuild cache
    this.cache.clear();
    for (const el of elements) {
      this.cache.set(el.id, el);
    }

    this.lastUrl = typeof (page as any).url === "function" ? (page as any).url() : "";
    this.lastExtractionTime = Date.now();

    // Install observer after first extraction
    await this.installObserver(page);

    return elements;
  }

  /** Install the MutationObserver in the page. */
  private async installObserver(page: Evaluator): Promise<void> {
    try {
      await (page as any).evaluate(OBSERVER_SCRIPT);
      this.observerInstalled = true;
    } catch {
      this.observerInstalled = false;
    }
  }

  /** Drain pending mutations from the page. */
  private async drainMutations(page: Evaluator): Promise<MutationRecord[]> {
    try {
      return (await (page as any).evaluate(DRAIN_SCRIPT)) as MutationRecord[];
    } catch {
      // Page may have navigated — observer is gone
      this.observerInstalled = false;
      return [];
    }
  }

  /**
   * Apply mutations to the cache.
   * For added/changed elements, re-extract the affected subtrees.
   */
  private async applyMutations(
    page: Evaluator,
    mutations: MutationRecord[],
  ): Promise<void> {
    const removedIds = new Set<string>();
    const needsReExtract = new Set<string>();

    for (const m of mutations) {
      // Track removals
      for (const id of m.removedIds) {
        removedIds.add(id);
      }

      // Track additions — need to extract these new elements
      if (m.addedIds.length > 0) {
        needsReExtract.add(m.targetId);
      }

      // Attribute changes — update cached element if we have it
      if (m.type === "attributes" && m.targetId) {
        const cached = this.cache.get(m.targetId);
        if (cached && m.attributeName && m.newValue !== null) {
          this.updateCachedAttribute(cached, m.attributeName, m.newValue);
        }
      }

      // Text changes — update label
      if (m.type === "characterData" && m.targetId) {
        const cached = this.cache.get(m.targetId);
        if (cached && m.newValue) {
          cached.label = m.newValue;
        }
      }
    }

    // Remove deleted elements
    for (const id of removedIds) {
      this.cache.delete(id);
    }

    // For elements with new children, do a targeted re-extraction
    // This is faster than full re-extraction but still captures new elements
    if (needsReExtract.size > 0) {
      const rawElements = await extractDOM(page);
      const elements = mapElements(rawElements);
      const sanitized = this.sanitize ? sanitizeElements(elements) : elements;

      // Merge new elements into cache
      for (const el of sanitized) {
        if (!this.cache.has(el.id) || needsReExtract.has(el.parent_id ?? "")) {
          this.cache.set(el.id, el);
        }
      }
    }

    this.lastExtractionTime = Date.now();
  }

  /** Update a single attribute on a cached element. */
  private updateCachedAttribute(
    el: ContextElement,
    attr: string,
    value: string,
  ): void {
    switch (attr) {
      case "disabled":
      case "aria-disabled":
        el.state.enabled = value === "" || value === "false";
        break;
      case "hidden":
      case "aria-hidden":
        el.state.visible = value === "" || value === "false";
        break;
      case "aria-expanded":
        el.state.expanded = value === "true";
        break;
      case "aria-selected":
        el.state.selected = value === "true";
        break;
      case "aria-checked":
      case "checked":
        el.state.checked = value === "true" || value === "";
        break;
      case "aria-label":
        el.label = value;
        break;
      case "value":
        el.value = value;
        break;
      case "href":
      case "src":
        // Don't update label; these affect navigation targets
        break;
    }
  }

  /** Penalize elements that haven't been confirmed recently.
   *  Uses lastExtractionTime as baseline and only applies delta since last penalty. */
  private applyStalenessPenalties(): void {
    const now = Date.now();
    const elapsed = now - this.lastExtractionTime;

    if (elapsed < STALENESS_TIMEOUT_MS) return;

    // Calculate penalty based on elapsed time since last extraction,
    // but cap at a reasonable maximum to avoid over-penalizing on rapid calls
    const periods = Math.floor(elapsed / STALENESS_TIMEOUT_MS);
    const penalty = Math.min(periods * STALENESS_PENALTY, 0.5);

    if (penalty > 0) {
      for (const el of this.cache.values()) {
        el.confidence = Math.max(0.1, 1.0 - penalty);
      }
    }
  }

  /** Clear the cache and observer state. */
  reset(): void {
    this.cache.clear();
    this.lastUrl = "";
    this.lastExtractionTime = 0;
    this.observerInstalled = false;
  }
}
