/**
 * CDP Extractor — extracts structured data from web pages via Chrome DevTools Protocol.
 *
 * Inspired by Stagehand v3's extract() and Agent-E's DOM distillation.
 * Uses targeted JavaScript evaluation to pull exactly what's needed,
 * avoiding the need to send full page content to the planner LLM.
 *
 * Two strategies:
 * 1. CSS-hint-based: Direct JS using provided CSS selectors (fast, no LLM)
 * 2. LLM-generated JS: Gemini Flash writes a JS snippet to extract data (fallback)
 */

import type { BrowserBridge } from "./interfaces/browser-bridge.js";
import type { Planner } from "./interfaces/planner.js";

/** Minimal CEL capability set needed by the CDP extractor. */
type CdpExtractorDeps = BrowserBridge & Pick<Planner, "llmComplete">;

export interface ExtractionHints {
  mode: "extract_data" | "find_elements" | "full_context";
  target: string;
  css_hints?: string[];
  max_items?: number;
  element_keywords?: string[];
}

/**
 * Extract structured data from the current browser page.
 * Returns extracted text or null if extraction fails.
 */
export async function extractFromPage(
  cel: CdpExtractorDeps,
  hints: ExtractionHints,
): Promise<string | null> {
  if (hints.mode !== "extract_data") return null;

  // Strategy A: try CSS-hint-based extraction first
  if (hints.css_hints?.length) {
    const result = await extractByCssHints(cel, hints.css_hints, hints.max_items ?? 10, hints.target);
    if (result && result.trim().length > 10) return result;
  }

  // Strategy B: LLM-generated JS extraction
  const result = await extractByLlmScript(cel, hints.target, hints.max_items ?? 10);
  if (result && result.trim().length > 10) return result;

  return null;
}

/**
 * Extract data using CSS selectors — builds and runs JS directly.
 * No LLM needed. Fast (~200ms).
 */
async function extractByCssHints(
  cel: CdpExtractorDeps,
  cssHints: string[],
  maxItems: number,
  target: string,
): Promise<string | null> {
  // Try each CSS hint until one produces results
  for (const hint of cssHints) {
    try {
      const js = buildExtractionJs(hint, maxItems, target);
      const result = await cel.cdpEvaluate(js);
      if (result && typeof result === "string" && result.length > 5) {
        return result;
      }
      if (result && Array.isArray(result) && result.length > 0) {
        return JSON.stringify(result, null, 2);
      }
    } catch {
      continue;
    }
  }
  return null;
}

/**
 * Build JS extraction code for a CSS selector.
 * Returns a self-contained IIFE that extracts text content.
 */
function buildExtractionJs(cssSelector: string, maxItems: number, _target: string): string {
  // Escape single quotes in CSS selector to prevent JS injection
  const selector = cssSelector.replace(/'/g, "\\'");
  // Detect table-like patterns — use textContent (more robust than cell-by-cell)
  // Skip first row (header) and start from data rows
  if (selector.includes("table") || selector.includes("tr")) {
    return `(() => {
      const rows = document.querySelectorAll('${selector}');
      const data = [];
      for (let i = 1; i < Math.min(rows.length, ${maxItems + 3}) && data.length < ${maxItems}; i++) {
        const text = rows[i].textContent?.trim()?.replace(/\\s+/g, ' ')?.slice(0, 200);
        if (text && text.length > 10) data.push(text);
      }
      return data.join('\\n---\\n');
    })()`;
  }

  // Detect list/heading patterns
  if (cssSelector.includes("h1") || cssSelector.includes("h2") || cssSelector.includes("h3") || cssSelector.includes("li")) {
    return `(() => {
      const els = document.querySelectorAll('${selector}');
      const data = [];
      for (let i = 0; i < Math.min(els.length, ${maxItems}); i++) {
        const text = els[i].textContent?.trim();
        if (text && text.length > 3) data.push(text);
      }
      return data.join('\\n');
    })()`;
  }

  // Generic: extract text content
  return `(() => {
    const els = document.querySelectorAll('${selector}');
    const data = [];
    for (let i = 0; i < Math.min(els.length, ${maxItems}); i++) {
      const text = els[i].textContent?.trim();
      if (text && text.length > 3) data.push(text.slice(0, 200));
    }
    return data.join('\\n');
  })()`;
}

/**
 * Use Gemini Flash to generate a JS extraction script, then run it via CDP.
 * Fallback when CSS hints fail or aren't provided.
 */
async function extractByLlmScript(
  cel: CdpExtractorDeps,
  target: string,
  maxItems: number,
): Promise<string | null> {
  try {
    // Get page title for context
    const titleResult = await cel.cdpEvaluate("document.title");
    const pageTitle = typeof titleResult === "string" ? titleResult : "Unknown page";

    const prompt = `Write a JavaScript expression (NOT a function, just an expression) that extracts "${target}" from a web page titled "${pageTitle}".

Requirements:
- Return a single string with the extracted data, one item per line
- Maximum ${maxItems} items
- Use document.querySelectorAll or other DOM methods
- The expression must be self-contained (wrap in IIFE if needed)
- Handle the case where elements don't exist (return empty string)
- Extract just the text content, no HTML

For tables/lists of data, format each row as: "Name | Value | Other columns"
For headings/articles, format as: "Headline text"
For prices, format as: "Name: $Price"

Return ONLY the JavaScript expression, no explanation, no markdown, no backticks.`;

    const js = await cel.llmComplete(prompt, target, 512);
    // Clean the response
    const cleaned = js
      .replace(/```javascript?\n?/g, "")
      .replace(/```/g, "")
      .trim();

    if (!cleaned || cleaned.length < 10) return null;

    // Safety: reject LLM-generated JS that contains dangerous patterns.
    // Only allow DOM read operations (querySelector, textContent, innerText, etc.)
    const dangerousPatterns = [
      /window\.location\s*=/, // navigation hijack
      /document\.cookie/,      // cookie theft
      /fetch\s*\(/,            // network requests
      /XMLHttpRequest/,        // network requests
      /eval\s*\(/,             // nested eval
      /Function\s*\(/,         // dynamic function creation
      /import\s*\(/,           // dynamic imports
      /\.src\s*=/,             // script/image injection
      /\.href\s*=/,            // link hijack
      /localStorage/,          // storage access
      /sessionStorage/,        // storage access
      /postMessage/,           // cross-origin messaging
      /\.write\s*\(/,          // document.write
      /\.remove\s*\(/,         // DOM mutation
      /\.delete\s*\(/,         // DOM mutation
      /\.submit\s*\(/,         // form submission
    ];

    for (const pattern of dangerousPatterns) {
      if (pattern.test(cleaned)) {
        console.warn(`[cdp-extractor] Blocked dangerous LLM-generated JS: ${pattern}`);
        return null;
      }
    }

    // Run the sanitized JS in a read-only wrapper
    const sandboxed = `(function() { "use strict"; return (${cleaned}); })()`;
    const result = await cel.cdpEvaluate(sandboxed);
    if (result === null || result === undefined) return null;
    return typeof result === "string" ? result : JSON.stringify(result);
  } catch (e) {
    console.warn(`[cdp-extractor] LLM script extraction failed: ${String(e).slice(0, 100)}`);
    return null;
  }
}

/**
 * Dismiss a blocking overlay (cookie consent, paywall, modal) via CDP.
 *
 * CDP-only equivalent of the browser adapter's overlay-detector — used when
 * goal-runner has only a Cel/CDP handle (no Playwright Page). Same structural
 * detection: CMP fingerprint → ARIA → positional X → multilingual text.
 *
 * Runs best-effort; never throws.
 */
export async function dismissCookieBanner(cel: CdpExtractorDeps): Promise<boolean> {
  const js = `(() => {
    // CMP-specific reject selectors (high reliability)
    const cmpReject = [
      "#onetrust-reject-all-handler",
      "#CybotCookiebotDialogBodyButtonDecline",
      "#CybotCookiebotDialogBodyLevelButtonLevelOptinDeclineAll",
      "#didomi-notice-disagree-button",
      ".didomi-continue-without-agreeing",
      ".sp_choice_type_REJECT_ALL",
      ".fc-cta-do-not-consent",
      '[data-cookiefirst-action="reject"]',
      ".cky-btn-reject",
      ".cm-btn-decline",
      ".cc-deny",
      "#W0wltc",
    ];
    const cmpAccept = [
      "#onetrust-accept-btn-handler",
      "#CybotCookiebotDialogBodyLevelButtonLevelOptinAllowAll",
      "#didomi-notice-agree-button",
      ".sp_choice_type_11",
      ".fc-cta-consent",
      '[data-cookiefirst-action="accept"]',
      ".cky-btn-accept",
      ".cc-allow",
      "#L2AGLb",
    ];
    function isVisible(el) {
      if (!el || el.offsetParent === null) {
        const cs = el && getComputedStyle(el);
        if (!cs || (cs.position !== "fixed" && cs.position !== "sticky")) return false;
      }
      const r = el.getBoundingClientRect();
      return r.width >= 1 && r.height >= 1;
    }
    // Stage 1: CMP reject (privacy-preserving)
    for (const sel of cmpReject) {
      const el = document.querySelector(sel);
      if (el && isVisible(el)) { el.click(); return "cmp_reject:" + sel; }
    }
    // Stage 2: ARIA dismiss
    const ariaSel = [
      'button[aria-label*="reject" i]',
      'button[aria-label*="decline" i]',
      'button[aria-label*="refuse" i]',
      'button[aria-label*="dismiss" i]',
      'button[aria-label*="close" i]',
    ];
    for (const sel of ariaSel) {
      const el = document.querySelector(sel);
      if (el && isVisible(el)) { el.click(); return "aria:" + sel; }
    }
    // Stage 3: Multilingual text match
    const REJECT = ["reject","decline","refuse","deny","disagree","no thanks","not now",
      "ablehnen","verweigern","refuser","rechazar","rifiuta","rejeitar","afwijzen",
      "απόρριψη","διαφωνώ","拒否","拒绝","거부","отклонить"];
    const buttons = document.querySelectorAll('button, [role="button"], a[role="button"]');
    for (const btn of buttons) {
      if (!isVisible(btn)) continue;
      const text = (btn.textContent || "").trim();
      if (!text || text.length > 50) continue;
      const lower = text.toLowerCase();
      if (REJECT.some(w => lower === w || lower.startsWith(w))) {
        btn.click();
        return "text_reject:" + text;
      }
    }
    // Stage 4: CMP accept (last resort — at least unblocks the page)
    for (const sel of cmpAccept) {
      const el = document.querySelector(sel);
      if (el && isVisible(el)) { el.click(); return "cmp_accept:" + sel; }
    }
    // Stage 5: Same-origin consent iframes
    const iframes = document.querySelectorAll('iframe[src*="consent"], iframe[title*="consent" i]');
    for (const iframe of iframes) {
      try {
        const doc = iframe.contentDocument;
        if (!doc) continue;
        for (const sel of cmpReject) {
          const el = doc.querySelector(sel);
          if (el) { el.click(); return "iframe_reject:" + sel; }
        }
      } catch {}
    }
    return null;
  })()`;
  try {
    const result = await cel.cdpEvaluate(js);
    return typeof result === "string" && result.length > 0;
  } catch {
    return false;
  }
}
