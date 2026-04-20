/**
 * Callback Builder — constructs GoalRunnerCallbacks from a BrowserAdapter.
 *
 * Extracted from cel-run.ts to be reusable by:
 * - celRun() (benchmark pipeline — launches own browser via Playwright)
 * - MCP run_goal (connects to user's existing Chrome via CDP)
 * - Any future consumer that has a connected BrowserAdapter
 *
 * The goal-runner is adapter-agnostic — it only knows GoalRunnerCallbacks.
 * This builder produces those callbacks from a BrowserAdapter instance,
 * with smart hybrid routing: web actions through CDP, native actions through CEL.
 */

import { BrowserAdapter } from "./index.js";
import type {
  Cel, PlannedAction, ScreenContext, GoalRunnerCallbacks, Cortex,
  ContextElement, AdapterCapabilities,
} from "@cellar/agent";
import { executePlannedAction } from "@cellar/agent";
import type {
  AdapterInstance, AdapterManifest, AdapterState, AdapterPlatform,
} from "@cellar/agent";

// ─── Exported Utilities ─────────────────────────────────────────────────────

/**
 * Route a PlannedAction through BrowserAdapter with smart element resolution.
 * Uses element properties (href, css_selector, backend_node_id) for reliable
 * targeting instead of just coordinates.
 */
export async function executeBrowserAction(
  adapter: BrowserAdapter,
  action: PlannedAction,
  context: ScreenContext,
): Promise<boolean> {
  if (!context.elements) context = { ...context, elements: [] };
  switch (action.type) {
    case "click": {
      const el = context.elements.find((e) => e.id === action.target_id);
      if (!el) throw new Error(`Element not found: ${action.target_id}`);
      // For input/textarea/combobox elements, use focus() instead of click()
      // to avoid triggering autocomplete that fills the field with garbage.
      const isInput = el.element_type === "combobox" || el.element_type === "input"
        || el.element_type === "textarea" || el.element_type === "searchbox";
      if (isInput && (el.properties?.css_selector || el.properties?.backend_node_id)) {
        const sel = el.properties.css_selector;
        const nodeId = el.properties.backend_node_id;
        try {
          if (sel) {
            await adapter.evaluate(`(() => { const el = document.querySelector(${JSON.stringify(sel)}); if (el) el.focus(); })()`);
            return true;
          }
          if (nodeId) {
            const resolved = await (adapter as any).client?.cdp?.send("DOM.resolveNode", { backendNodeId: Number(nodeId) });
            if (resolved?.object?.objectId) {
              await (adapter as any).client?.cdp?.send("Runtime.callFunctionOn", {
                objectId: resolved.object.objectId,
                functionDeclaration: "function() { this.focus(); }",
              });
              return true;
            }
          }
        } catch { /* fall through to regular click */ }
      }
      return adapter.executeAction("click", {
        href: el.properties?.href,
        css_selector: el.properties?.css_selector,
        backend_node_id: el.properties?.backend_node_id,
        ...(el.bounds ? {
          x: el.bounds.x + Math.floor(el.bounds.width / 2),
          y: el.bounds.y + Math.floor(el.bounds.height / 2),
        } : {}),
      });
    }
    case "type": {
      const el = action.target_id ? context.elements.find((e) => e.id === action.target_id) : null;
      if (el?.properties?.css_selector || el?.properties?.backend_node_id) {
        return adapter.executeAction("type", {
          selector: el.properties.css_selector,
          backend_node_id: el.properties.backend_node_id,
          text: action.text, clearFirst: true,
        });
      }
      if (el?.bounds) {
        return adapter.executeAction("type", {
          x: el.bounds.x + Math.floor(el.bounds.width / 2),
          y: el.bounds.y + Math.floor(el.bounds.height / 2),
          text: action.text, clearFirst: true,
        });
      }
      return adapter.executeAction("type", { text: action.text });
    }
    case "set_value": {
      const el = context.elements.find((e) => e.id === action.target_id);
      const isSelectLike = el?.element_type === "combobox" || el?.element_type === "select";
      if (isSelectLike && (el?.properties?.css_selector || el?.properties?.backend_node_id)) {
        return adapter.executeAction("select_option", {
          selector: el.properties?.css_selector,
          backend_node_id: el.properties?.backend_node_id,
          value: action.value,
          label: action.value,
        });
      }
      const isReadonly = el?.properties?.readonly === "true" ||
        el?.properties?.input_type === "datepicker" ||
        el?.properties?.input_type === "date";
      if (isReadonly && el?.properties?.css_selector && "setValueDirect" in adapter) {
        return (adapter as any).setValueDirect(el.properties.css_selector, action.value);
      }
      if (el?.properties?.css_selector || el?.properties?.backend_node_id) {
        return adapter.executeAction("type", {
          selector: el.properties.css_selector,
          backend_node_id: el.properties.backend_node_id,
          text: action.value, clearFirst: true,
        });
      }
      if (!el?.bounds) throw new Error(`Element not found: ${action.target_id}`);
      return adapter.executeAction("type", {
        x: el.bounds.x + Math.floor(el.bounds.width / 2),
        y: el.bounds.y + Math.floor(el.bounds.height / 2),
        text: action.value, clearFirst: true,
      });
    }
    case "key":
      return adapter.executeAction("press_key", { key: action.key });
    case "key_combo":
      return adapter.executeAction("key_combo", { keys: action.keys });
    case "scroll":
      return adapter.executeAction("scroll_by", { dx: action.dx, dy: action.dy });
    case "drag":
      return adapter.executeAction("drag", {
        fromX: action.from_x, fromY: action.from_y,
        toX: action.to_x, toY: action.to_y,
      });
    case "select":
      // Text selection: mouse drag from start to end coordinates
      return adapter.executeAction("drag", {
        fromX: action.from_x, fromY: action.from_y,
        toX: action.to_x, toY: action.to_y,
      });
    case "wait":
      await new Promise((r) => setTimeout(r, action.ms));
      return true;
    case "batch":
      for (const sub of action.actions) {
        await executeBrowserAction(adapter, sub, context);
        await new Promise((r) => setTimeout(r, 200));
      }
      return true;
    case "act": {
      const resolved = resolveActInstruction(action.instruction, context);
      if (resolved.action === "type" && resolved.targetId) {
        const el = context.elements.find((e) => e.id === resolved.targetId);
        if (el?.properties?.css_selector) {
          return adapter.executeAction("type", {
            selector: el.properties.css_selector,
            text: resolved.text, clearFirst: true,
          });
        }
        if (el?.bounds) {
          return adapter.executeAction("type", {
            x: el.bounds.x + Math.floor(el.bounds.width / 2),
            y: el.bounds.y + Math.floor(el.bounds.height / 2),
            text: resolved.text, clearFirst: true,
          });
        }
        return adapter.executeAction("type", { text: resolved.text });
      } else if (resolved.targetId) {
        const el = context.elements.find((e) => e.id === resolved.targetId);
        if (!el) throw new Error(`Act: no element matched for "${action.instruction}"`);
        return adapter.executeAction("click", {
          href: el.properties?.href,
          css_selector: el.properties?.css_selector,
          backend_node_id: el.properties?.backend_node_id,
          ...(el.bounds ? {
            x: el.bounds.x + Math.floor(el.bounds.width / 2),
            y: el.bounds.y + Math.floor(el.bounds.height / 2),
          } : {}),
        });
      }
      throw new Error(`Act: could not resolve instruction "${action.instruction}"`);
    }
    case "activate_app":
    case "ax_action":
      // Native macOS actions — should be routed through buildBrowserCallbacks'
      // executeAction wrapper (which handles these before calling executeBrowserAction).
      // If they reach here, it means executeBrowserAction was called directly
      // without the wrapper. Warn and return true to avoid blocking.
      console.warn(`executeBrowserAction: native action "${action.type}" should be handled by the hybrid wrapper, not the browser adapter`);
      return true;
    case "extract":
    case "done":
    case "fail":
      return true;
    case "custom":
      return adapter.executeAction(action.action, action.params);
    case "notebook_writes":
      return true;
    default:
      return true;
  }
}

/**
 * Resolve a natural-language "act" instruction to a concrete action
 * by fuzzy-matching against the current context elements.
 */
export function resolveActInstruction(
  instruction: string,
  context: ScreenContext,
): { action: string; targetId?: string; text?: string } {
  const lower = instruction.toLowerCase();
  const isType = lower.startsWith("type") || lower.startsWith("enter") || lower.startsWith("fill");
  const isClick = lower.startsWith("click") || lower.startsWith("press") || lower.startsWith("select") || lower.startsWith("open");
  const targetDesc = lower.replace(/^(click|press|select|open|type|enter|fill|tap|choose)\s+(on\s+|the\s+|a\s+)*/i, "").trim();

  let bestMatch: (typeof context.elements)[number] | null = null;
  let bestScore = 0;

  for (const el of (context.elements ?? [])) {
    const label = (el.label || "").toLowerCase();
    const desc = (el.description || "").toLowerCase();
    const href = (el.properties?.url || el.properties?.href || "").toLowerCase();
    let score = 0;

    if (label === targetDesc) score += 100;
    else if (label.includes(targetDesc) && targetDesc.length > 3) score += 50;
    else if (targetDesc.includes(label) && label.length > 3) score += 30;

    const words = targetDesc.split(/\s+/).filter((w) => w.length > 2);
    for (const word of words) {
      if (label.includes(word)) score += 5;
      if (desc.includes(word)) score += 3;
      if (href.includes(word)) score += 2;
    }

    if (isClick && (el.element_type === "button" || el.element_type === "link")) score += 3;
    if (isType && (el.element_type === "input" || el.element_type === "textarea" || el.element_type === "combobox")) score += 3;
    if (el.state?.visible) score += 1;
    if (el.state?.enabled) score += 1;

    if (score > bestScore) {
      bestScore = score;
      bestMatch = el;
    }
  }

  if (bestMatch && bestScore >= 3) {
    if (isType) {
      const textMatch = instruction.match(/["']([^"']+)["']/);
      return { action: "type", targetId: bestMatch.id, text: textMatch?.[1] || "" };
    }
    return { action: "click", targetId: bestMatch.id };
  }
  return { action: "click" };
}

/**
 * Navigate to a URL and prepare the page for interaction.
 * Handles: navigation timeout, SPA hydration, cookie consent (3x),
 * and modal overlay dismissal.
 */
export async function navigateAndPrepare(
  adapter: BrowserAdapter,
  url: string,
  options?: { navTimeout?: number },
): Promise<void> {
  const NAV_TIMEOUT_MS = options?.navTimeout ?? 60_000;
  await Promise.race([
    (async () => {
      await adapter.navigate(url);
      await adapter.waitForStable({ timeout: 3000 });
    })(),
    new Promise<void>((_, reject) =>
      setTimeout(() => reject(new Error(`Navigation timeout after ${NAV_TIMEOUT_MS}ms`)), NAV_TIMEOUT_MS),
    ),
  ]);

  // SPA hydration detection
  try {
    const isSPA = await adapter.evaluate<boolean>(`(() => {
      if (window.__NEXT_DATA__ || window.__NUXT__ || window.__REACT_DEVTOOLS_GLOBAL_HOOK__) return true;
      if (document.querySelector('[data-reactroot], [id="__next"], [ng-version], [data-v-]')) return true;
      const scripts = document.querySelectorAll('script[src]');
      for (const s of scripts) {
        const src = s.getAttribute('src') || '';
        if (src.includes('_next/') || src.includes('chunk') && (src.includes('react') || src.includes('vue'))) return true;
      }
      return false;
    })()`);
    if (isSPA) {
      const initialLen = await adapter.evaluate<number>(`document.body?.innerText?.length ?? 0`);
      for (let i = 0; i < 4; i++) {
        await new Promise(r => setTimeout(r, 500));
        const currentLen = await adapter.evaluate<number>(`document.body?.innerText?.length ?? 0`);
        if (currentLen > initialLen && currentLen === await adapter.evaluate<number>(`document.body?.innerText?.length ?? 0`)) {
          break;
        }
      }
    }
  } catch { /* best effort */ }

  // Cookie consent dismissal
  try { await adapter.dismissCookieConsent(); } catch {}

  // Modal overlay dismissal
  try {
    await adapter.evaluate(`(() => {
      const dismissSelectors = [
        '[aria-label*="Dismiss" i]', '[aria-label*="Close" i]',
        '[data-testid*="close"]', '[data-testid*="dismiss"]',
        'button[class*="close" i]', '[role="dialog"] button',
      ];
      for (const sel of dismissSelectors) {
        const btns = document.querySelectorAll(sel);
        for (const btn of btns) {
          if (btn.offsetParent !== null) { btn.click(); }
        }
      }
      document.querySelectorAll('[role="dialog"], [class*="modal"]').forEach(el => {
        if (el.offsetParent !== null) el.remove();
      });
      document.body.style.overflow = '';
    })()`);
  } catch {}
}

/**
 * Detect if the page is blocked by bot detection (Cloudflare, Akamai, etc.).
 */
export function detectBotBlock(ctx: ScreenContext): boolean {
  const pageText = ctx.elements.find(e => e.id === "page-text")?.value ?? "";
  const lowerText = pageText.toLowerCase();
  return (
    (lowerText.includes("captcha") && lowerText.includes("verify")) ||
    (lowerText.includes("access denied") && ctx.elements.length <= 10) ||
    (lowerText.includes("please enable javascript") && ctx.elements.length <= 5) ||
    (lowerText.includes("are you a robot") || lowerText.includes("not a robot")) ||
    (lowerText.includes("sorry, you have been blocked") && lowerText.includes("cloudflare")) ||
    (lowerText.includes("ray id") && lowerText.includes("blocked")) ||
    (lowerText.includes("access denied") && lowerText.includes("don't have permission")) ||
    (lowerText.includes("reference #") && lowerText.includes("access denied")) ||
    (lowerText.includes("press & hold") && lowerText.includes("human")) ||
    (lowerText.includes("checking your browser") && ctx.elements.length <= 5)
  );
}

// ─── Callback Builder ───────────────────────────────────────────────────────

export interface BrowserCallbackOptions {
  /** The connected BrowserAdapter instance. */
  adapter: BrowserAdapter;
  /** CEL bindings for native fallback actions (activate_app, ax_action). */
  cel: Cel;
  /** Whether adapter is CDP-connected (external Chrome) vs Playwright-launched.
   * When true, uses getContextFast() to avoid 30s+ timeout on heavy SPAs. */
  isCdpConnected?: boolean;
  /** URL to navigate to when Chrome starts on a blank/newtab page.
   * Extracted from the goal by the caller. Defaults to google.com. */
  goalUrl?: string;
  /** Optional URL constraint — blocks navigation to search engines. */
  constrainToUrl?: string;
  /** Step progress callback. */
  onStep?: (stepIndex: number, actionType: string, reasoning: string) => void;
  /** Verification callback for "done"/"extract" actions. */
  verify?: () => Promise<boolean>;
  /** Optional cortex used for freshness-aware routing and outcome ingestion. */
  cortex?: Cortex | {
    model: { freshness?: import("@cellar/agent").FreshnessAssessment } | null;
    readFreshness?(): import("@cellar/agent").FreshnessAssessment;
    ingestActionOutcome?(outcome: import("@cellar/agent").ActionOutcome): void;
  };
  /** Goal text for ambiguity-aware routing and resolution. */
  goal?: string;
  /** Whether running headless (Playwright-launched). When true, skip native
   * macOS activateApp calls that would interfere with the user's desktop. */
  headless?: boolean;
}

type RouteAmbiguityAssessment = {
  ambiguous: boolean;
  confidence: number;
  reason: string;
  preferredTargetId?: string;
};

const SEMANTIC_ROLE_WORDS = ["viewer", "editor", "admin", "owner", "member", "guest"];
const SEMANTIC_STOP_WORDS = new Set([
  "the", "and", "for", "with", "that", "this", "click", "button", "correct",
  "user", "email", "remove", "target", "action", "goal", "who", "whose",
]);

function normalizeSemanticText(value?: string | null): string {
  return (value ?? "").toLowerCase().replace(/\s+/g, " ").trim();
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function getElementSemanticText(element: ContextElement, context: ScreenContext): string {
  const parts: string[] = [
    element.label ?? "",
    element.description ?? "",
    element.value ?? "",
    String(element.properties?.placeholder ?? ""),
  ];

  if (element.parent_id) {
    const parent = context.elements.find((candidate) => candidate.id === element.parent_id);
    if (parent?.label) parts.push(parent.label);
    const siblings = context.elements
      .filter((candidate) => candidate.parent_id === element.parent_id && candidate.id !== element.id)
      .flatMap((candidate) => [candidate.label, candidate.value])
      .filter((candidate): candidate is string => Boolean(candidate && candidate.trim().length > 0))
      .slice(0, 8);
    parts.push(...siblings);
  }

  if (element.bounds) {
    const centerY = element.bounds.y + element.bounds.height / 2;
    const nearbyContainers = context.elements
      .filter((candidate) =>
        candidate.id !== element.id
        && Boolean(candidate.label)
        && Boolean(candidate.bounds)
        && ["table_row", "row", "list_item", "group"].includes(candidate.element_type)
      )
      .filter((candidate) => {
        const bounds = candidate.bounds!;
        return bounds.y <= centerY && centerY <= bounds.y + bounds.height;
      })
      .sort((a, b) => (a.bounds!.height * a.bounds!.width) - (b.bounds!.height * b.bounds!.width))
      .slice(0, 3)
      .map((candidate) => candidate.label!);
    parts.push(...nearbyContainers);
  }

  return normalizeSemanticText(parts.join(" "));
}

function extractGoalSignals(goal?: string): {
  phrases: string[];
  emails: string[];
  roles: string[];
  words: string[];
} {
  const lowerGoal = normalizeSemanticText(goal);
  const phraseMatches = Array.from(goal?.matchAll(/["']([^"']+)["']/g) ?? [])
    .map((match) => normalizeSemanticText(match[1]))
    .filter((match) => match.length > 2);
  const emails = Array.from(goal?.matchAll(/[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}/gi) ?? [])
    .map((match) => normalizeSemanticText(match[0]));
  const roles = SEMANTIC_ROLE_WORDS.filter((role) => new RegExp(`\\b${role}\\b`, "i").test(goal ?? ""));
  const words = lowerGoal
    .split(/\W+/)
    .map((word) => word.trim())
    .filter((word) =>
      word.length > 2
      && !SEMANTIC_ROLE_WORDS.includes(word)
      && !SEMANTIC_STOP_WORDS.has(word)
    );

  return {
    phrases: Array.from(new Set(phraseMatches)),
    emails: Array.from(new Set(emails)),
    roles,
    words: Array.from(new Set(words)),
  };
}

function isClickableCandidate(element: ContextElement): boolean {
  return (element.actions ?? []).includes("click")
    || element.element_type === "button"
    || element.element_type === "link"
    || element.element_type === "a";
}

function scoreGoalMatch(element: ContextElement, context: ScreenContext, goal?: string): number {
  const text = getElementSemanticText(element, context);
  if (!text) return 0;

  const signals = extractGoalSignals(goal);
  let score = 0;

  for (const email of signals.emails) {
    if (text.includes(email)) score += 18;
  }
  for (const phrase of signals.phrases) {
    if (new RegExp(`(^|[^a-z0-9])${escapeRegExp(phrase)}($|[^a-z0-9-])`, "i").test(text)) score += 12;
    else {
      const phraseWords = phrase.split(/\s+/);
      const partialMatches = phraseWords.filter((word) => word.length > 2 && text.includes(word)).length;
      score += partialMatches;
    }
  }
  for (const role of signals.roles) {
    if (new RegExp(`\\b${escapeRegExp(role)}\\b`, "i").test(text)) score += 5;
  }
  for (const word of signals.words) {
    if (new RegExp(`\\b${escapeRegExp(word)}\\b`, "i").test(text)) score += 1;
  }

  if (isClickableCandidate(element)) score += 1;
  if (element.state?.visible) score += 0.5;
  if (element.state?.enabled) score += 0.5;

  return score;
}

export function assessActionAmbiguity(
  action: PlannedAction,
  context: ScreenContext,
  goal?: string,
): RouteAmbiguityAssessment | null {
  if (action.type !== "click" || !goal) return null;

  const selected = context.elements.find((element) => element.id === action.target_id);
  if (!selected || !isClickableCandidate(selected)) return null;

  const candidates = (context.elements ?? []).filter((element) => isClickableCandidate(element) && element.id !== selected.id);
  if (candidates.length === 0) return null;

  const selectedScore = scoreGoalMatch(selected, context, goal);
  const ranked = candidates
    .map((candidate) => ({
      candidate,
      score: scoreGoalMatch(candidate, context, goal),
    }))
    .sort((a, b) => b.score - a.score);

  const best = ranked[0];
  if (!best || best.score < 8) return null;

  if (best.score >= selectedScore + 4) {
    return {
      ambiguous: true,
      confidence: 0.82,
      reason: `Goal matches ${best.candidate.label ?? best.candidate.id} better than the planner-selected target`,
      preferredTargetId: best.candidate.id,
    };
  }

  return null;
}

// ─── Browser Adapter Instance (Adapter Registry) ──────────────────────────

/** Browser adapter manifest — static capabilities. */
export const BROWSER_ADAPTER_MANIFEST: AdapterManifest = {
  name: "browser",
  displayName: "Browser (CDP + Playwright)",
  platforms: ["macos", "windows", "linux"] as AdapterPlatform[],
  supportedActionTypes: new Set([
    "click", "type", "set_value", "key", "key_combo", "scroll",
    "navigate", "extract", "screenshot", "select_option", "ax_action",
    "drag", "act", "done", "custom",
  ]),
  requiresApp: undefined,
  appPatterns: [/chrome/i, /chromium/i, /brave/i, /edge/i, /firefox/i, /safari/i, /browser/i, /arc/i],
};

/**
 * BrowserAdapterInstance — wraps BrowserAdapter + CEL for the AdapterRegistry.
 *
 * Produces AdapterCapabilities identical to the inline construction in
 * buildBrowserCallbacks (line 758), but accessible through the registry.
 */
export class BrowserAdapterInstance implements AdapterInstance {
  readonly manifest = BROWSER_ADAPTER_MANIFEST;
  state: AdapterState = "disconnected";

  constructor(
    private adapter: BrowserAdapter,
    private cel: Cel,
    private opts: {
      isCdpConnected?: boolean;
      cortex?: BrowserCallbackOptions["cortex"];
      goal?: string;
    } = {},
  ) {}

  async connect(): Promise<void> {
    this.state = "connecting";
    try {
      // Browser adapter is typically pre-connected by the caller
      this.state = "connected";
    } catch (e) {
      this.state = "error";
      throw e;
    }
  }

  async disconnect(): Promise<void> {
    try {
      await this.adapter.disconnect();
    } catch { /* best-effort */ }
    this.state = "disconnected";
  }

  async probe(): Promise<boolean> {
    try {
      await this.adapter.evaluate<string>("document.title");
      return true;
    } catch {
      return false;
    }
  }

  buildCapabilities(): AdapterCapabilities {
    const { adapter, opts } = this;
    const isCdpConnected = opts.isCdpConnected ?? false;

    const getContextWithTimeout = async (timeoutMs = 5000): Promise<ScreenContext> => {
      if (!isCdpConnected) return adapter.getContext();
      try {
        return await Promise.race([
          adapter.getContextFast(),
          new Promise<ScreenContext>((_, reject) =>
            setTimeout(() => reject(new Error("getContextFast timeout")), timeoutMs),
          ),
        ]);
      } catch {
        return { app: "Browser", window: "Loading...", elements: [], timestamp_ms: Date.now() } as ScreenContext;
      }
    };

    return {
      readContext: () => getContextWithTimeout(),
      executeStructured: (a, c) => executeBrowserAction(adapter, a, c),
      resolveSemantic: (a, c) => adapter.resolveSemanticAction(a, c),
      captureScreenshot: () => adapter.screenshot(),
      postNavigateCleanup: async () => { await adapter.dismissCookieConsent(); },
    };
  }

  async healthCheck(): Promise<boolean> {
    return this.probe();
  }
}

/**
 * Build GoalRunnerCallbacks from a connected BrowserAdapter.
 *
 * This is the proven benchmark pipeline, extracted for reuse.
 * Handles: DOM context extraction, CSS selector action routing,
 * cookie/consent dismissal, page stability, bot detection fallback.
 *
 * Hybrid action routing: web actions through adapter (CDP),
 * native actions (activate_app, ax_action) through CEL.
 */
export function buildBrowserCallbacks(opts: BrowserCallbackOptions): GoalRunnerCallbacks {
  const { adapter, cel, isCdpConnected, constrainToUrl, cortex } = opts;

  // DOM content hash for caching — only re-extract when content changes
  let lastDomHash = "";
  const getContentFingerprint = (): string => {
    const url = adapter.getPageUrl();
    if (!lastDomHash || lastDomHash === "0:" || lastDomHash === "") {
      return `${url}::${Date.now()}`;
    }
    return `${url}::${lastDomHash}`;
  };

  // Helper: getContextFast with a 5s timeout to prevent 30-84s hangs
  // on empty pages, internal chrome:// pages, or navigating pages.
  const getContextWithTimeout = async (timeoutMs = 5000): Promise<import("@cellar/agent").ScreenContext> => {
    if (!isCdpConnected) return adapter.getContext();
    try {
      return await Promise.race([
        adapter.getContextFast(),
        new Promise<import("@cellar/agent").ScreenContext>((_, reject) =>
          setTimeout(() => reject(new Error("getContextFast timeout")), timeoutMs),
        ),
      ]);
    } catch {
      // Timeout or error — return minimal context with page info
      try {
        const title = await Promise.race([
          adapter.evaluate<string>("document.title || ''"),
          new Promise<string>((_, reject) => setTimeout(() => reject(new Error("title timeout")), 2000)),
        ]);
        return {
          app: "Browser", window: title || "Loading...",
          elements: [], timestamp_ms: Date.now(),
        } as import("@cellar/agent").ScreenContext;
      } catch {
        return {
          app: "Browser", window: "Loading...",
          elements: [], timestamp_ms: Date.now(),
        } as import("@cellar/agent").ScreenContext;
      }
    }
  };

  // Track consecutive empty context reads to detect stale CDP connections
  let consecutiveEmptyReads = 0;

  return {
    getContext: async () => {
      // Detect chrome://newtab and navigate to a useful page.
      // chrome://newtab has no real DOM — CDP can't extract elements from it.
      // If the goal mentions a URL or known site, navigate directly there.
      try {
        const currentUrl = await Promise.race([
          adapter.evaluate<string>("location.href"),
          new Promise<string>((_, reject) => setTimeout(() => reject("timeout"), 2000)),
        ]);
        if (currentUrl && (currentUrl.startsWith("chrome://") || currentUrl === "about:blank")) {
          // Extract target URL from the goal (passed via closure from buildBrowserCallbacks)
          const goalUrl = opts.goalUrl ?? "https://www.google.com";
          await adapter.navigate(goalUrl);
          await adapter.waitForStable({ timeout: 5000 });
          try { await adapter.dismissCookieConsent(); } catch {}
        }
      } catch { /* best effort */ }

      let ctx = await getContextWithTimeout();

      // Empty context handling
      if (ctx.elements.length === 0) {
        consecutiveEmptyReads++;

        // After 2+ empty reads, the CDP connection is likely stale.
        // Try reconnecting to the current active tab.
        if (consecutiveEmptyReads >= 2 && isCdpConnected) {
          try {
            // Reconnect CDP to whatever tab is now active
            await adapter.reconnect?.();
            const reconnected = await getContextWithTimeout(3000);
            if (reconnected.elements.length > 0) {
              consecutiveEmptyReads = 0;
              const elemSig = reconnected.elements.slice(0, 10).map(e => `${e.id}:${e.element_type}`).join(",");
              lastDomHash = `${reconnected.elements.length}:${elemSig}`;
              return reconnected;
            }
          } catch { /* reconnect failed */ }
        }

        // Try cookie dismiss + retry
        try { await adapter.dismissCookieConsent?.(); } catch {}
        await new Promise(r => setTimeout(r, 1500));
        const retry = await getContextWithTimeout(3000);
        if (retry.elements.length > 0) {
          consecutiveEmptyReads = 0;
          const elemSig = retry.elements.slice(0, 10).map(e => `${e.id}:${e.element_type}`).join(",");
          lastDomHash = `${retry.elements.length}:${elemSig}`;
          return retry;
        }
        // Still empty — try page text with timeout (2s max)
        try {
          const pageText = await Promise.race([
            adapter.evaluate<string>(
              `(() => {
                const main = document.querySelector('main, [role="main"], #content, #main, [data-testid="search-results"], article');
                if (main && main.innerText.length > 200) return main.innerText.slice(0, 8000);
                return document.body?.innerText?.slice(0, 6000) ?? "";
              })()`,
            ),
            new Promise<string>((_, reject) => setTimeout(() => reject(new Error("timeout")), 2000)),
          ]);
          if (pageText && pageText.length > 20) {
            ctx.elements = [{
              id: "page-text",
              element_type: "text",
              label: pageText.slice(0, 500),
              value: pageText,
              bounds: { x: 0, y: 0, width: 1280, height: 800 },
              state: { visible: true, enabled: true, focused: false, selected: false },
              actions: [],
              confidence: 0.6,
              source: "cdp_fallback" as any,
            }];
          }
        } catch {}
      }

      if (ctx.elements.length > 0) consecutiveEmptyReads = 0;
      const elemSig = ctx.elements.slice(0, 10).map(e => `${e.id}:${e.element_type}`).join(",");
      lastDomHash = `${ctx.elements.length}:${elemSig}`;

      // Late cookie dismiss — catches consent banners that appear after load
      try {
        const hasLateConsent = ctx.elements.some(e =>
          (e.label?.toLowerCase().includes("cookie") || e.label?.toLowerCase().includes("consent") ||
           e.label?.toLowerCase().includes("privacy") || e.label?.toLowerCase().includes("accept all"))
          && e.state?.visible
        );
        if (hasLateConsent) {
          await adapter.dismissCookieConsent();
        }
      } catch {}

      return ctx;
    },

    getContextTier1: () => adapter.getContextTier1(),
    getContextTier2: () => adapter.getContextTier2(),

    screenshot: async () => adapter.screenshot(),

    stateFingerprint: () => getContentFingerprint(),

    waitForSettle: async (actionType) => {
      const baseTimeouts: Record<string, number> = {
        custom: 3000, click: 1500, key: 1500, key_combo: 1500, type: 800,
      };
      let t = baseTimeouts[actionType];
      if (!t) return;

      // Adaptive SPA state tracking: if the DOM is still mutating, extend the
      // settle timeout to catch async React/Vue/Angular re-renders.
      try {
        const hasPending = await Promise.race([
          adapter.evaluate<boolean>(
            `(() => {
              const last = window.__celLastMutation;
              return last ? (Date.now() - last < 500) : false;
            })()`,
          ),
          new Promise<boolean>((resolve) => setTimeout(() => resolve(false), 1000)),
        ]);
        if (hasPending) {
          // DOM is still changing — double the timeout (max 6s for navigate, 3s for click)
          const maxTimeout = actionType === "custom" ? 6000 : 3000;
          t = Math.min(t * 2, maxTimeout);
        }
      } catch { /* mutation tracking not available */ }

      await adapter.waitForStable({ timeout: t, idleTime: 150 });

      // Proactive cookie consent dismissal after navigation-like actions
      if (actionType === "custom" || actionType === "click") {
        try { await adapter.dismissCookieConsent(); } catch {}
      }
    },

    executeAction: async (action, ctx) => {
      const logRoute = (message: string, extra?: Record<string, unknown>) => {
        console.info(`[runtime-route] ${message}`, extra ?? {});
      };

      // Hybrid routing: native actions through CEL, web actions through adapter

      // Native macOS actions — bypass browser adapter.
      // Skip entirely in headless mode (Playwright) — no desktop app to activate.
      if (!opts.headless) {
        if (action.type === "activate_app" && "app_name" in action) {
          return (cel as any).activateApp?.(action.app_name) ?? false;
        }
        if (action.type === "ax_action" && "target_id" in action) {
          (cel as any).activateApp?.("Google Chrome");
          await new Promise(r => setTimeout(r, 200));
          return cel.axPerformAction(action.target_id, action.action);
        }
      } else {
        // Headless: activate_app is a no-op, ax_action falls through to CDP path
        if (action.type === "activate_app") return true;
      }

      // For keyboard/type actions targeting a11y elements (not CDP DOM elements),
      // re-focus Chrome first. CDP actions go through the adapter and don't need this.
      // Skip in headless mode — no desktop Chrome to refocus.
      const isNativeTarget = action.type === "click" && "target_id" in action && action.target_id.startsWith("ax:");
      const isNativeType = action.type === "type" && "target_id" in action && action.target_id?.startsWith("ax:");
      const isKeyboardAction = action.type === "key" || action.type === "key_combo";
      if (!opts.headless && (isNativeTarget || isNativeType || isKeyboardAction)) {
        (cel as any).activateApp?.("Google Chrome");
        await new Promise(r => setTimeout(r, 200));
      }

      // Domain constraint — block navigation to search engines
      if (action.type === "custom" && action.adapter === "browser" && action.action === "navigate") {
        const targetUrl = action.params?.url as string | undefined;
        if (targetUrl && constrainToUrl) {
          try {
            const targetDomain = new URL(targetUrl).hostname.replace("www.", "");
            const configDomain = new URL(constrainToUrl).hostname.replace("www.", "");
            if (targetDomain !== configDomain) {
              const searchEngines = ["google.com", "bing.com", "yahoo.com", "duckduckgo.com"];
              if (searchEngines.includes(targetDomain)) {
                return true; // Skip silently
              }
            }
          } catch { /* invalid URL, let it through */ }
        }
      }

      // Web actions — delegate to the runtime kernel for route→execute→verify
      const capabilities: AdapterCapabilities = {
        readContext: () => getContextWithTimeout(),
        executeStructured: (a, c) => executeBrowserAction(adapter, a, c),
        resolveSemantic: (a, c) => adapter.resolveSemanticAction(a, c),
        captureScreenshot: () => adapter.screenshot(),
        postNavigateCleanup: async () => { await adapter.dismissCookieConsent(); },
      };

      const outcome = await executePlannedAction({
        action,
        context: ctx,
        capabilities,
        readFreshness: () => cortex?.readFreshness?.() ?? cortex?.model?.freshness ?? null,
        ingestOutcome: (payload) => {
          cortex?.ingestActionOutcome?.(payload);
        },
        assessAmbiguity: (a, c) => assessActionAmbiguity(a, c, opts.goal),
        logRoute,
      });

      return outcome.success;
    },

    verifyGoal: opts.verify,

    onStepPlanned: opts.onStep
      ? (step, i) => opts.onStep!(i, step.action.type, step.reasoning)
      : undefined,
  };
}
