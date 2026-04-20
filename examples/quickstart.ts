#!/usr/bin/env npx tsx
/**
 * CEL Quickstart Demo
 *
 * Shows what CEL sees when it looks at a web page:
 * - Extracts DOM elements as structured ContextElements
 * - Shows confidence scores, element types, available actions
 * - Demonstrates how the planner would see this context
 *
 * Usage:
 *   npx tsx examples/quickstart.ts [url]
 *
 * Examples:
 *   npx tsx examples/quickstart.ts
 *   npx tsx examples/quickstart.ts https://news.ycombinator.com
 *   npx tsx examples/quickstart.ts https://github.com/login
 */

import { BrowserAdapter } from "../adapters/browser/src/index.js";

const DEFAULT_URL = "https://example.com";

async function main() {
  const url = process.argv[2] || DEFAULT_URL;

  console.log("╔══════════════════════════════════════════════════════╗");
  console.log("║           CEL — Context Execution Layer             ║");
  console.log("║       What the agent sees when it looks at a page   ║");
  console.log("╚══════════════════════════════════════════════════════╝");
  console.log();

  // 1. Connect to a browser
  console.log("⏳ Launching browser...");
  const adapter = new BrowserAdapter({
    browser: "chromium",
    useCdp: true,
    headless: true,
    sanitize: true,
    incrementalUpdates: false,
  });

  await adapter.connect();

  // 2. Navigate
  console.log(`⏳ Navigating to ${url}...`);
  await adapter.navigate(url);

  // Wait for page to settle
  await sleep(1000);

  // 3. Extract context
  console.log("⏳ Extracting context...\n");
  const startTime = Date.now();
  const context = await adapter.getContext();
  const elapsed = Date.now() - startTime;

  // 4. Display results
  console.log("┌────────────────────────────────────────────────────────┐");
  console.log("│  SCREEN CONTEXT                                        │");
  console.log("├────────────────────────────────────────────────────────┤");
  console.log(`│  App:      ${context.app}`);
  console.log(`│  Window:   ${context.window}`);
  console.log(`│  Elements: ${context.elements.length}`);
  console.log(`│  Network:  ${context.network_events?.length ?? 0} events`);
  console.log(`│  Extracted in: ${elapsed}ms`);
  console.log("└────────────────────────────────────────────────────────┘");
  console.log();

  // 5. Show elements table
  console.log("┌────────────────────────────────────────────────────────────────────────────────────┐");
  console.log("│  UI ELEMENTS (sorted by confidence)                                                │");
  console.log("├──────┬────────────┬────────────────────────┬──────────┬─────────┬──────────────────┤");
  console.log("│ Conf │ Type       │ Label                  │ State    │ Source  │ Actions          │");
  console.log("├──────┼────────────┼────────────────────────┼──────────┼─────────┼──────────────────┤");

  const displayElements = context.elements.slice(0, 30);
  for (const el of displayElements) {
    const conf = el.confidence.toFixed(2);
    const type = pad(el.element_type, 10);
    const label = pad(el.label || "-", 22);
    const state = pad(formatState(el.state), 8);
    const source = pad(el.source.replace("_", " "), 7);
    const actions = (el.actions || []).join(", ");

    console.log(
      `│ ${conf} │ ${type} │ ${label} │ ${state} │ ${source} │ ${pad(actions, 16)} │`
    );
  }

  if (context.elements.length > 30) {
    console.log(
      `│ ...  │ ...        │ +${context.elements.length - 30} more elements        │ ...      │ ...     │ ...              │`
    );
  }
  console.log("└──────┴────────────┴────────────────────────┴──────────┴─────────┴──────────────────┘");

  // 6. Show what the planner prompt would look like
  console.log();
  console.log("┌────────────────────────────────────────────────────────┐");
  console.log("│  PLANNER PROMPT PREVIEW                                │");
  console.log("│  This is what the LLM receives to decide the next step │");
  console.log("├────────────────────────────────────────────────────────┤");
  console.log();

  const promptElements = context.elements.slice(0, 15);
  console.log("  ## Goal");
  console.log("  (your natural-language goal here)");
  console.log();
  console.log(`  ## Current Screen`);
  console.log(`  App: ${context.app} | Window: ${context.window}`);
  console.log();
  console.log("  ## UI Elements");
  console.log("  | ID | Type | Label | Value | State | Actions |");
  console.log("  |-----|------|-------|-------|-------|---------|");
  for (const el of promptElements) {
    const label = el.label || "-";
    const value = el.value || "-";
    const state = formatState(el.state);
    const actions = (el.actions || []).join(", ") || "-";
    console.log(
      `  | ${el.id} | ${el.element_type} | ${label.slice(0, 25)} | ${value.slice(0, 15)} | ${state} | ${actions} |`
    );
  }
  if (context.elements.length > 15) {
    console.log(`  (${context.elements.length - 15} more elements)`);
  }
  console.log();
  console.log("  ## Your Next Step");
  console.log("  Respond with ONE action as JSON.");
  console.log();
  console.log("└────────────────────────────────────────────────────────┘");

  // 7. Show stats
  console.log();
  console.log("── Stats ────────────────────────────────────────────────");
  const types = new Map<string, number>();
  for (const el of context.elements) {
    types.set(el.element_type, (types.get(el.element_type) || 0) + 1);
  }
  const sortedTypes = [...types.entries()].sort((a, b) => b[1] - a[1]);
  for (const [type, count] of sortedTypes) {
    console.log(`  ${pad(type, 15)} ${count}`);
  }
  console.log();

  const actionable = context.elements.filter(
    (e) => e.actions && e.actions.length > 0
  );
  const avgConfidence =
    context.elements.reduce((sum, e) => sum + e.confidence, 0) /
    (context.elements.length || 1);

  console.log(`  Total elements:      ${context.elements.length}`);
  console.log(`  Actionable elements: ${actionable.length}`);
  console.log(`  Avg confidence:      ${avgConfidence.toFixed(3)}`);
  console.log(`  Extraction time:     ${elapsed}ms`);
  console.log();

  // 8. Incremental update demo
  console.log("── Incremental Update Demo ──────────────────────────────");
  const adapter2 = new BrowserAdapter({
    browser: "chromium",
    useCdp: true,
    headless: true,
    incrementalUpdates: true,
  });
  await adapter2.connect();
  await adapter2.navigate(url);
  await sleep(500);

  const t1 = Date.now();
  await adapter2.getElements(); // First call: full extraction
  const fullTime = Date.now() - t1;

  const t2 = Date.now();
  await adapter2.getElements(); // Second call: incremental (MutationObserver)
  const incrementalTime = Date.now() - t2;

  console.log(`  Full extraction:     ${fullTime}ms`);
  console.log(`  Incremental update:  ${incrementalTime}ms`);
  console.log(
    `  Speedup:             ${(fullTime / (incrementalTime || 1)).toFixed(1)}x`
  );
  console.log();

  // Cleanup
  await adapter.disconnect();
  await adapter2.disconnect();

  console.log("Done. Try with a different URL:");
  console.log("  npx tsx examples/quickstart.ts https://news.ycombinator.com");
  console.log("  npx tsx examples/quickstart.ts https://github.com/login");
}

function formatState(state: {
  focused: boolean;
  enabled: boolean;
  visible: boolean;
  selected: boolean;
}): string {
  const flags: string[] = [];
  if (state.focused) flags.push("focused");
  if (!state.enabled) flags.push("disabled");
  if (!state.visible) flags.push("hidden");
  if (state.selected) flags.push("selected");
  return flags.length > 0 ? flags.join(",") : "normal";
}

function pad(s: string, len: number): string {
  return s.length >= len ? s.slice(0, len) : s + " ".repeat(len - s.length);
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

main().catch((err) => {
  console.error("Error:", err.message || err);
  process.exit(1);
});
