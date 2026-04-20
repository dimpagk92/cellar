/**
 * Pre-Done Verification — validates "done" and "extract" claims before accepting them.
 *
 * Catches premature claims, vague summaries, wrong domains, error pages,
 * and LLM fabrication via semantic cross-checking.
 */

import type { PlannedAction, ScreenContext } from "../types.js";

/**
 * Verify a "done" or "extract" action BEFORE accepting it.
 * Catches premature claims, vague summaries, wrong domains, and error pages.
 */
export function verifyDone(
  action: PlannedAction,
  context: ScreenContext | null,
  goal: string,
  configUrl?: string,
): { verified: boolean; reason?: string } {
  // 1. Check if page-text exists and has content
  const pageText = context?.elements?.find(el =>
    el.id?.includes("page-text") || el.id?.includes("page_text")
  )?.label || "";

  const hasPageText = pageText.length > 50;

  // 2. Check claimed summary has specific data
  const summary = action.type === "done" ? action.summary : (action.type === "extract" ? action.data : "");
  if (!summary || summary.length < 10 || /^(achieved|done|completed|success|task completed|goal achieved|finished)\.?$/i.test(summary.trim())) {
    return { verified: false, reason: "Summary is too vague. Include specific data from the page (names, numbers, text you found)." };
  }

  if (action.type !== "extract") {
    const hasSpecificData = /\d|[A-Z][a-z]{2,}|".*?"|[$€£¥₹]/.test(summary);
    if (summary.length < 20 && !hasSpecificData) {
      return { verified: false, reason: "Summary lacks specific data. Include actual names, numbers, or text from the page. If data is not visible, try scrolling or using 'extract' to read the page content." };
    }
  }

  // 3. Check page URL is on target domain
  if (configUrl) {
    try {
      const targetDomain = new URL(configUrl).hostname.replace("www.", "");
      const pageUrl = context?.elements?.find(el => el.properties?.url)?.properties?.url || "";
      if (pageUrl && !pageUrl.includes(targetDomain)) {
        return { verified: false, reason: `You are on the wrong domain. Navigate to ${targetDomain}.` };
      }
    } catch { /* invalid URL — skip domain check */ }
  }

  // 4. Check for error page indicators
  if (hasPageText) {
    const errorIndicators = ["access denied", "page not found", "error occurred"];
    for (const indicator of errorIndicators) {
      if (pageText.toLowerCase().includes(indicator)) {
        return { verified: false, reason: `Page shows an error: "${indicator}". Navigate elsewhere.` };
      }
    }
  }

  // 5. Scroll-before-done on comparison/pricing tasks
  const isComparisonTask = /compar|pricing|plans?\b|difference|vs\b|versus/i.test(goal);
  const scrollY = parseInt(
    context?.elements?.find(el => el.properties?.scroll_y)?.properties?.scroll_y || "0"
  );
  if (isComparisonTask && scrollY === 0) {
    return { verified: false, reason: "You haven't scrolled. Comparison/pricing details are usually below the fold. Scroll down first." };
  }

  // 6. Semantic cross-check: verify claimed data exists in visible context
  if (hasPageText && action.type === "done" && summary.length > 20) {
    const dataTokens = summary.match(/\$[\d,.]+|€[\d,.]+|£[\d,.]+|\d{2,}(?:\.\d+)?%?|[A-Z][a-z]+(?:\s[A-Z][a-z]+){0,2}/g) ?? [];
    if (dataTokens.length > 0) {
      const visibleText = (context?.elements ?? [])
        .map(el => [el.label, el.value, el.description].filter(Boolean).join(" "))
        .join(" ")
        .toLowerCase();

      const foundCount = dataTokens.filter(token =>
        visibleText.includes(token.toLowerCase()),
      ).length;
      const foundRatio = foundCount / dataTokens.length;

      if (foundRatio < 0.3 && dataTokens.length >= 2) {
        const missing = dataTokens.filter(t => !visibleText.includes(t.toLowerCase())).slice(0, 3);
        return {
          verified: false,
          reason: `Summary contains data not visible on the page: ${missing.join(", ")}. ` +
            `Verify this data is actually on screen, or scroll/navigate to find it.`,
        };
      }
    }
  }

  return { verified: true };
}

// Debug helper — logs verification rejection reasons
export function verifyDoneDebug(
  action: PlannedAction,
  context: ScreenContext | null,
  goal: string,
  configUrl?: string,
): { verified: boolean; reason?: string } {
  const result = verifyDone(action, context, goal, configUrl);
  if (!result.verified) {
    console.log(`    [verify-done] REJECTED: ${result.reason?.slice(0, 100)}`);
  }
  return result;
}
