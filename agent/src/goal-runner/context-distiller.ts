/**
 * DOM Distillation — reduces context to essential elements (LEGACY).
 *
 * @deprecated Superseded by cel-planner/src/distiller.rs (Rust implementation).
 * The Rust distiller runs entirely in-process before the LLM call, eliminating
 * the FFI round-trip for context optimization. This TS version is kept for
 * backward compatibility with the TS goal-runner fallback path.
 *
 * Based on Stagehand v3's context builder and Agent-E's DOM distillation.
 *
 * Two modes:
 * 1. Generic distillation: reduces by element type (actionable first)
 * 2. Goal-aware distillation: scores elements by relevance to the goal
 */

import type { ScreenContext, ContextElement } from "../types.js";
import type { ExtractionHints } from "../cdp-extractor.js";

const ACTIONABLE_TYPES = new Set([
  "button", "input", "select", "textarea", "a", "link",
  "checkbox", "radio_button", "combobox", "slider",
  "tab", "menu_item",
]);
const GENERIC_ACTION_LABELS = new Set([
  "open", "close", "cancel", "ok", "more", "menu", "next", "back",
  "learn more", "details", "view", "edit", "delete", "remove", "select",
  "continue", "submit", "save", "apply", "retry", "dismiss",
]);
const CHROME_HINT_PATTERN = /header|nav|navbar|toolbar|menu|sidebar|breadcrumb|footer|legal|cookie|consent|account|profile|help|support|social|share|newsletter|chat|intercom/i;
const COMPLETED_ACTION_PATTERN = /\b(acknowledged|approved|saved|sent|completed|done|resolved|finished|success)\b|✓/i;

const STOP_WORDS = new Set([
  "the", "a", "an", "is", "are", "was", "were", "be", "been",
  "and", "or", "but", "in", "on", "at", "to", "for", "of",
  "with", "from", "by", "as", "it", "do", "not", "this", "that",
  "my", "me", "i", "you", "we", "can", "will", "just", "any",
  "all", "each", "find", "search", "open", "read", "get", "show",
  "tell", "what", "how", "please", "don't", "dont",
]);

/** Check if an element type is actionable. */
export function isActionableType(elementType: string): boolean {
  return ACTIONABLE_TYPES.has(elementType);
}

/**
 * Distill a screen context to only essential elements.
 * Priority: interactive elements WITH bounds first (can be clicked),
 * then interactive without bounds (reference only), then visible text.
 * Removes elements the agent can't meaningfully target.
 */
export function distillContext(ctx: ScreenContext, maxElements = 40): ScreenContext {
  if (!ctx.elements) return { ...ctx, elements: [] };
  // Interactive elements WITH bounds — can actually be clicked
  const clickable = ctx.elements.filter((el) =>
    (ACTIONABLE_TYPES.has(el.element_type) || (el.actions?.length ?? 0) > 0) &&
    el.bounds
  );

  // Text/content elements — for data extraction (page-text, headings, etc.)
  const textElements = ctx.elements.filter((el) =>
    !clickable.includes(el) &&
    el.state?.visible &&
    el.label &&
    el.label.trim().length > 0 &&
    (el.id === "page-text" || el.element_type === "text" || el.element_type === "heading" ||
     el.element_type === "static_text" || el.label.length > 20) &&
    el.element_type !== "group" && el.element_type !== "toolbar"
  ).slice(0, 20);

  return {
    ...ctx,
    elements: [...clickable, ...textElements].slice(0, maxElements),
  };
}

/**
 * Goal-aware context distillation.
 * Scores elements by relevance to the goal and extraction hints.
 * Returns only the most relevant elements.
 *
 * @param consecutiveScrolls - Number of consecutive scrolls without progress.
 *   When >= 3, forces page-text inclusion and sets forceExtract flag.
 */
export function distillContextByGoal(
  ctx: ScreenContext,
  goal: string,
  extraction?: ExtractionHints,
  maxElements = 30,
  consecutiveScrolls = 0,
): ScreenContext & { forceExtract?: boolean } {
  const keywords = extractKeywords(goal);

  let result: ScreenContext;
  if (extraction?.mode === "find_elements" && extraction.element_keywords?.length) {
    const allKeywords = [...keywords, ...extraction.element_keywords];
    result = filterByKeywords(ctx, allKeywords, maxElements, goal);
  } else {
    result = filterByKeywords(ctx, keywords, maxElements, goal);
  }

  // Scroll-loop breaker: after 3+ consecutive scrolls without progress,
  // force page-text into the context so the LLM extracts instead of scrolling more.
  if (consecutiveScrolls >= 3) {
    const pageText = (ctx.elements ?? []).find((el) => el.id === "page-text");
    if (pageText && !result.elements.includes(pageText)) {
      result = { ...result, elements: [pageText, ...result.elements] };
    }
    return { ...result, forceExtract: true };
  }

  return result;
}

/**
 * Semantic synonym map — expands goal keywords with related terms
 * that keyword matching would miss. E.g., "book" → also matches "reservation".
 *
 * This is a lightweight, no-LLM solution to the keyword-only distillation problem.
 * Covers common web automation domains.
 */
const SEMANTIC_SYNONYMS: Record<string, string[]> = {
  // Travel & booking
  book: ["reservation", "reservations", "booking", "reserve", "availability"],
  hotel: ["accommodation", "room", "rooms", "stay", "lodging", "inn"],
  flight: ["airline", "plane", "travel", "departure", "arrival", "boarding"],
  // Shopping
  buy: ["purchase", "order", "cart", "checkout", "add to cart", "basket"],
  price: ["cost", "pricing", "fee", "rate", "amount", "total", "charge"],
  cheap: ["affordable", "budget", "lowest", "discount", "deal", "sale"],
  // Forms
  login: ["sign in", "signin", "log in", "authenticate", "credentials"],
  register: ["sign up", "signup", "create account", "join"],
  submit: ["send", "confirm", "apply", "save", "complete"],
  // Search & navigation
  search: ["find", "look", "query", "browse", "explore", "lookup"],
  navigate: ["go to", "open", "visit", "click", "select"],
  // Data
  extract: ["read", "get", "show", "display", "list", "view"],
  compare: ["versus", "difference", "comparison", "contrast"],
};

/** Extract meaningful keywords from a goal string, expanded with semantic synonyms. */
function extractKeywords(goal: string): string[] {
  const raw = goal
    .toLowerCase()
    .replace(/[^a-z0-9\s]/g, " ")
    .split(/\s+/)
    .filter((w) => w.length > 2 && !STOP_WORDS.has(w));

  // Expand with semantic synonyms
  const expanded = new Set(raw);
  for (const word of raw) {
    const synonyms = SEMANTIC_SYNONYMS[word];
    if (synonyms) {
      for (const syn of synonyms) {
        // Add individual words from multi-word synonyms
        for (const part of syn.split(/\s+/)) {
          if (part.length > 2) expanded.add(part);
        }
      }
    }
  }

  return Array.from(expanded);
}

function normalizeText(text?: string | null): string {
  return (text ?? "").trim().toLowerCase().replace(/\s+/g, " ");
}

function buildElementContext(el: ContextElement, ctx: ScreenContext): string {
  const parts: string[] = [];
  const parentId = el.parent_id;
  if (parentId) {
    const parent = ctx.elements.find((candidate) => candidate.id === parentId);
    if (parent?.label) parts.push(parent.label);

    const siblings = ctx.elements
      .filter((candidate) => candidate.parent_id === parentId && candidate.id !== el.id)
      .flatMap((candidate) => [candidate.label, candidate.value])
      .filter((value): value is string => Boolean(value && value.trim().length > 0))
      .slice(0, 6);
    parts.push(...siblings);
  }

  return parts.join(" ").toLowerCase();
}

function countRepeatedGenericActions(el: ContextElement, ctx: ScreenContext): number {
  const label = normalizeText(el.label);
  if (!GENERIC_ACTION_LABELS.has(label)) return 0;

  return ctx.elements.filter((candidate) =>
    candidate.id !== el.id &&
    candidate.element_type === el.element_type &&
    normalizeText(candidate.label) === label
  ).length;
}

function isCompletedActionLabel(label?: string | null): boolean {
  return COMPLETED_ACTION_PATTERN.test(normalizeText(label));
}

/** Score an element's relevance to a set of keywords. */
function scoreElement(el: ContextElement, ctx: ScreenContext, keywords: string[], goal?: string): number {
  let score = 0;
  const text = [el.label, el.value, el.description, el.element_type]
    .filter(Boolean)
    .join(" ")
    .toLowerCase();
  const localContext = buildElementContext(el, ctx);
  const selectorHint = String((el as any).properties?.css_selector ?? "");
  const normalizedLabel = normalizeText(el.label);

  for (const kw of keywords) {
    if (text.includes(kw)) score += 2;
    if (localContext.includes(kw)) score += 1.5;
  }

  // Multi-word phrase boost: if the element label matches a 2+ word phrase from the goal,
  // it's very likely the target (e.g., "Machine learning" link on a Wikipedia page).
  // Also penalize partial matches — "Learning" should not outrank "Machine learning".
  if (goal) {
    const goalLower = goal.toLowerCase();
    const labelLower = (el.label || "").toLowerCase();

    // Quoted phrase exact match — user explicitly quoted a target phrase.
    // +50 makes this unambiguously top-ranked regardless of other scoring.
    const quotedPhrases = goal.match(/["']([^"']+)["']/g)?.map(m => m.slice(1, -1).toLowerCase()) ?? [];
    for (const quoted of quotedPhrases) {
      if (quoted.length > 2 && labelLower === quoted) {
        score += 50;
      }
    }

    if (labelLower.length > 4) {
      const phrases = goalLower.match(/[a-z]+(?:\s[a-z]+)+/g) ?? [];
      for (const phrase of phrases) {
        if (labelLower.includes(phrase)) {
          // Exact or near-exact match gets a huge boost
          score += 20;
        } else {
          // Check if label matches only PART of a multi-word phrase
          // e.g., "Learning" matches the word "learning" in "Machine learning"
          // but is NOT the full phrase — penalize to prevent wrong clicks
          const phraseWords = phrase.split(/\s+/);
          if (phraseWords.length >= 2) {
            const matchesPartial = phraseWords.some(w => w.length > 3 && labelLower === w);
            if (matchesPartial) score -= 5;
          }
        }
      }
    }
  }

  // Extraction-goal awareness: when the goal is to extract/read data,
  // boost content elements and deprioritize filter/sidebar UI controls.
  const isExtractGoal = goal && /\bextract\b|\bread\b.*\bcontent\b|\bget\b.*\bdata\b|\bfind\b.*\b(?:name|price|rating|title)\b/i.test(goal);
  if (isExtractGoal) {
    // Boost text/content elements for extraction goals
    if (el.element_type === "text" || el.element_type === "heading" || el.element_type === "static_text") score += 3;
    // Deprioritize filter/sidebar controls that match keywords superficially
    const filterPattern = /filter|sort|checkbox|sidebar|facet|toggle|dropdown/i;
    const elContext = [el.element_type, (el as any).properties?.css_selector || ""].join(" ");
    if (filterPattern.test(elContext)) score -= 3;
    if (el.element_type === "checkbox" || el.element_type === "radio_button") score -= 2;
  }

  // Boost actionable elements
  if (ACTIONABLE_TYPES.has(el.element_type)) score += 1;
  // Boost visible + enabled
  if (el.state?.visible && el.state?.enabled) score += 0.5;
  // Boost elements with actions
  if ((el.actions?.length ?? 0) > 0) score += 0.5;

  // Boost content-rich elements — article headlines, long labels
  // This prevents short nav links ("AI", "Apps") from outscoring articles
  const labelLen = el.label?.length ?? 0;
  if (labelLen > 30) score += 2;  // Article headline
  else if (labelLen > 15) score += 1; // Moderate content
  else if (labelLen <= 5 && ACTIONABLE_TYPES.has(el.element_type)) score -= 1; // Short nav links

  // Boost page-text element (contains extractable data)
  // For extraction goals, page-text is the PRIMARY data source — give it max priority
  if (el.id === "page-text") score += isExtractGoal ? 50 : 5;

  // Deprioritize clickable elements without bounds — they can't be targeted
  if (ACTIONABLE_TYPES.has(el.element_type) && !el.bounds) score -= 3;

  // Content role awareness (prompt injection defense integration):
  // Interactive elements get a small boost (these are real UI controls).
  // Content elements are preserved for reading but not boosted for acting.
  if ((el as any).content_role === "interactive") score += 1.5;
  if ((el as any).content_role === "decorative") score -= 2;

  if (GENERIC_ACTION_LABELS.has(normalizedLabel) && ACTIONABLE_TYPES.has(el.element_type)) {
    const repeatedCount = countRepeatedGenericActions(el, ctx);
    score -= repeatedCount >= 2 ? 2.5 : 0.5;
    if (localContext.length > 0) score += 2;
  }

  // Downrank controls that look like they already represent a completed state.
  // This helps sequential workflows move on after the first sub-goal lands
  // instead of re-clicking "Acknowledged ✓", "Sent ✓", etc.
  if (ACTIONABLE_TYPES.has(el.element_type) && isCompletedActionLabel(el.label)) {
    score -= 6;
  }

  if (CHROME_HINT_PATTERN.test(selectorHint)) score -= 4;

  return score;
}

/** Filter context elements by keyword relevance. */
function filterByKeywords(
  ctx: ScreenContext,
  keywords: string[],
  maxElements: number,
  goal?: string,
): ScreenContext {
  if (keywords.length === 0) {
    return distillContext(ctx, maxElements);
  }

  // For navigation goals, include more elements to avoid filtering out the target link
  const isNavGoal = goal && /\bclick\b.*\blink\b|\bfind\b.*\blink\b|\bnavigate\b|\bclick\s+on\b/i.test(goal);
  const effectiveMax = isNavGoal ? Math.max(maxElements, 50) : maxElements;

  // Score all elements
  const scored = (ctx.elements ?? []).map((el) => ({
    el,
    score: scoreElement(el, ctx, keywords, goal),
  }));

  // Sort by score descending
  scored.sort((a, b) => b.score - a.score);

  // Take top elements, but always include at least some interactive ones
  const relevant = scored.filter((s) => s.score > 0).map((s) => s.el);
  const interactive = (ctx.elements ?? []).filter((el) =>
    !relevant.includes(el) && ACTIONABLE_TYPES.has(el.element_type)
  ).slice(0, 5);

  // Always preserve page-text element (needed for content verification)
  const pageText = (ctx.elements ?? []).find((el) => el.id === "page-text");
  const combined = [...relevant, ...interactive]
    .filter((el, index, all) => {
      const label = normalizeText(el.label);
      if (!GENERIC_ACTION_LABELS.has(label) || !ACTIONABLE_TYPES.has(el.element_type)) return true;
      const sameLabelSeen = all.slice(0, index).filter((candidate) =>
        candidate.element_type === el.element_type && normalizeText(candidate.label) === label
      ).length;
      const localContext = buildElementContext(el, ctx);
      return sameLabelSeen < 2 || localContext.length > 0;
    })
    .slice(0, effectiveMax);
  if (pageText && !combined.includes(pageText)) combined.push(pageText);
  const elements = combined;

  return { ...ctx, elements };
}
