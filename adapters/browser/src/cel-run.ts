/**
 * cel.run() — give it a goal, CEL figures out the rest.
 *
 * CEL determines what app/context is needed based on the goal:
 * - URL in the goal or config? → Launch browser, navigate, automate
 * - Desktop app goal? → Use native accessibility + input
 * - Already on the right page? → Just execute
 *
 * Handles everything: browser lifecycle, navigation, cookie dismissal,
 * context extraction, action routing, vision, self-heal, loop detection.
 *
 * Usage:
 *   const result = await celRun(cel, {
 *     goal: "Go to funda.nl and extract Amsterdam listings",
 *   });
 *
 *   const result = await celRun(cel, {
 *     url: "https://finance.yahoo.com/quote/AAPL/",
 *     goal: "Extract the current stock price and 52-week range",
 *   });
 */

import { BrowserAdapter } from "./index.js";
import {
  navigateAndPrepare, detectBotBlock, buildBrowserCallbacks,
} from "./callback-builder.js";
import {
  runGoal, ActCache, MemoryCacheStorage, Cortex,
  type Cel, type GoalResult, type GoalRunnerCallbacks,
} from "@cellar/agent";

/** Try the Rust goal-runner first; fall back to TS on error. */
async function runGoalWithRustFallback(
  cel: Cel,
  config: Parameters<typeof runGoal>[1],
  callbacks: GoalRunnerCallbacks,
): Promise<GoalResult> {
  const prof = process.env.CELLAR_PROFILE === "1";
  const emitRF = (phase: string, ms: number) => {
    if (prof) process.stderr.write(`{"lvl":"profile","step":"runGoalFallback","phase":"${phase}","ms":${ms}}\n`);
  };
  const tRustStart = Date.now();
  try {
    // NB: cel.runGoalRust takes an object (Record<string, unknown>) and
    // handles its own serialization. Previously this call double-stringified
    // the payload — once here, once inside cel.runGoalRust — which caused
    // the Rust side's JSON parser to see a string-of-a-string and fail with
    // "invalid type: string, expected struct GoalConfig". Every benchmark
    // run was silently falling back to the TS path as a result.
    const rustResult = await cel.runGoalRust({
      goal: config.goal,
      max_steps: config.maxSteps ?? 30,
      timeout_ms: config.taskTimeout ?? 120_000,
      workflow_name: config.workflowName ?? null,
      step_delay_ms: 500,
      max_consecutive_failures: config.maxConsecutiveFailures ?? 5,
    });
    emitRF("rust_ok", Date.now() - tRustStart);
    return rustResult as GoalResult;
  } catch (e) {
    emitRF("rust_fail", Date.now() - tRustStart);
    console.error(`[cel-run] Rust runner failed: ${e}. Falling back to TS.`);
    const tTsStart = Date.now();
    const r = await runGoal(cel, config, callbacks);
    emitRF("ts_fallback", Date.now() - tTsStart);
    return r;
  }
}

export interface CelRunConfig {
  /** Natural language goal to achieve. */
  goal: string;
  /** URL to navigate to (optional — CEL infers from goal if not provided). */
  url?: string;
  /** Run browser in headless mode (default: false — visible). */
  headless?: boolean;
  /** Max steps before giving up (default: 30). */
  maxSteps?: number;
  /** Total timeout in ms (default: 120000). */
  timeout?: number;
  /** Enable action cache for faster repeated runs (default: false). */
  cache?: boolean;
  /** Enable vision/screenshot escalation (default: true). */
  vision?: boolean;
  /** Enable self-healing on action failures (default: true). */
  selfHeal?: boolean;
  /** Callback for step progress logging. */
  onStep?: (stepIndex: number, actionType: string, reasoning: string) => void;
  /** Reuse an existing BrowserAdapter instead of creating a new one.
   * When provided, celRun will NOT disconnect the adapter on completion. */
  adapter?: BrowserAdapter;
  /** Enable cortex (always-on perception) for this run.
   * When true, the goal-runner reads from a continuously-updated mental model
   * instead of calling getContext() on each step. */
  cortex?: boolean;
  /** Optional verification callback — called when the agent claims "done" or "extract".
   * Return true if the goal is actually achieved, false to continue the loop. */
  verify?: () => Promise<boolean>;
  /** Per-step timeout in ms (default: 30000). */
  stepTimeout?: number;
  /** Max steps without context fingerprint change before bailing out (default: 10). */
  maxStepsWithoutProgress?: number;
  /** Enable milestone decomposition for complex goals (default: false). */
  decompose?: boolean;
  /** Workflow name for history-informed planning (default: undefined). */
  workflowName?: string;
}

export interface CelRunResult extends GoalResult {
  /** Number of elements detected on the page. */
  elementsDetected: number;
}

// executeAction and resolveActInstruction are imported from callback-builder.ts
// This ensures improvements to element resolution, action routing, and fuzzy matching
// automatically apply to both the benchmark pipeline AND the MCP run_goal handler.

/** Extract a URL from the goal text if present. */
function extractUrl(goal: string): string | null {
  const match = goal.match(/https?:\/\/[^\s"'<>]+/);
  return match ? match[0] : null;
}

/** Does this goal need a browser? */
function needsBrowser(config: CelRunConfig): boolean {
  if (config.url) return true;
  if (extractUrl(config.goal)) return true;
  const webKeywords = /\b(website|webpage|page|site|browse|http|www\.|\.com|\.org|\.net|\.io)\b/i;
  return webKeywords.test(config.goal);
}

/**
 * Run a goal — CEL figures out the context needed.
 *
 * If a URL is provided or detected in the goal, launches a browser.
 * Otherwise uses native desktop automation (accessibility tree + native input).
 */
export async function celRun(
  cel: Cel,
  config: CelRunConfig,
): Promise<CelRunResult> {
  const url = config.url ?? extractUrl(config.goal);
  const useBrowser = needsBrowser(config);

  if (useBrowser) {
    return runWithBrowser(cel, config, url);
  }
  return runNative(cel, config);
}

// Keep backward-compatible alias
export const runBrowserGoal = celRun;

/** Browser path: launch adapter (or reuse), navigate, run goal, cleanup. */
async function runWithBrowser(
  cel: Cel,
  config: CelRunConfig,
  url: string | null,
): Promise<CelRunResult> {
  const headless = config.headless ?? false;
  const ownsAdapter = !config.adapter;

  const adapter = config.adapter ?? new BrowserAdapter({
    cel,
    browser: "chromium",
    useCdp: true,
    headless,
    stealth: headless,
    viewport: { width: 1280, height: 800 },
    sanitize: true,
    incrementalUpdates: true,
  });

  // Opt-in profiler — CELLAR_PROFILE=1 to see phase breakdown. Zero cost off.
  const prof = process.env.CELLAR_PROFILE === "1";
  const emitP = (phase: string, ms: number) => {
    if (prof) process.stderr.write(`{"lvl":"profile","step":"celrun","phase":"${phase}","ms":${ms}}\n`);
  };
  try {
    if (ownsAdapter) {
      const t0 = Date.now();
      await adapter.connect();
      emitP("adapter.connect", Date.now() - t0);
    }

    // Navigate + prepare page (SPA hydration, cookie consent, modal dismissal)
    if (url) {
      const t0 = Date.now();
      await navigateAndPrepare(adapter, url);
      emitP("navigateAndPrepare", Date.now() - t0);
    }

    const tCtx = Date.now();
    let ctx = await adapter.getContextFast();
    emitP("getContextFast", Date.now() - tCtx);

    // Empty initial context — retry with another cookie dismiss
    if (ctx.elements.length === 0) {
      const tRetry = Date.now();
      try { await adapter.dismissCookieConsent?.(); } catch {}
      await new Promise(r => setTimeout(r, 1500));
      ctx = await adapter.getContextFast();
      emitP("contextRetry", Date.now() - tRetry);
    }

    // Detect bot-blocked pages early
    if (detectBotBlock(ctx)) {
      return {
        status: "failed" as const,
        summary: "Page blocked by bot detection",
        totalSteps: 0,
        history: [],
        metrics: { totalMs: 0, llmCalls: 0, contextExtractionMs: 0, errorCount: 1, visionCalls: 0, stateChanges: 0, loopWarnings: 0 },
        elementsDetected: ctx.elements.length,
      };
    }

    // Late cookie dismiss — catches consent banners that load after initial page render
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

    // Boot cortex with adapter's context provider if requested.
    // The cortex background loop calls adapter.getContext() (CDP-based),
    // so the mental model reflects browser state, not desktop a11y.
    let cortex: Cortex | undefined;
    if (config.cortex) {
      cortex = new Cortex(cel, {
        getContext: () => adapter.getContext(),
        getQuickContext: () => {
          // Quick check: just return a minimal context with app/window
          const url = adapter.getPageUrl();
          return { app: "Browser", window: url, elements: [], timestamp_ms: Date.now() };
        },
      });
      await cortex.boot();
    }

    const actCache = config.cache ? new ActCache(new MemoryCacheStorage()) : undefined;

    // Build callbacks from the shared callback-builder (single source of truth).
    // Benchmark pipeline and MCP run_goal both use the same builder.
    const callbacks: GoalRunnerCallbacks = buildBrowserCallbacks({
      adapter,
      cel,
      goal: config.goal,
      cortex,
      isCdpConnected: false, // Playwright-launched: full getContext() is safe
      headless: config.headless ?? false, // Skip native activateApp in headless mode
      constrainToUrl: url ?? undefined,
      onStep: config.onStep,
      verify: config.verify,
    });

    // NOTE: Do NOT boot Rust Cortex here — the benchmark path uses its own
    // Playwright adapter with per-task browser instances. The Rust Cortex would
    // discover the user's Chrome and interfere. The runGoalWithRustFallback
    // will fail (no Cortex) and fall back to TS runGoal with Playwright callbacks.

    // Env-var plumbing for the tier-replan hardening flags. Lets bench
    // runs and ad-hoc CLI invocations enable/disable the flags without
    // touching the caller. Defaults mirror the goal-runner's own defaults
    // (all false), so unset env vars == no behavior change.
    const boolEnv = (name: string): boolean => {
      const v = process.env[name];
      return v === "1" || v === "true" || v === "yes";
    };

    const goalConfig = {
      goal: config.goal,
      maxSteps: config.maxSteps ?? 100,
      taskTimeout: config.timeout ?? 300_000,
      maxConsecutiveFailures: 5,
      enableVision: config.vision ?? true,
      visionMode: "auto" as const,
      selfHeal: config.selfHeal ?? true,
      distillContext: true,
      skipRouter: true,
      stepTimeout: config.stepTimeout,
      maxStepsWithoutProgress: config.maxStepsWithoutProgress,
      actCache,
      cortex,
      enableDecomposition: config.decompose ?? true,
      enableNotebook: true,
      workflowName: config.workflowName,
      // Tier-replan hardening flags. See docs/replan-tiers.md.
      enableTierReplan: boolEnv("CELLAR_ENABLE_TIER_REPLAN"),
      enableSemanticStallEscalation: boolEnv("CELLAR_ENABLE_SEMANTIC_STALL"),
      enableTier4Reassessment: boolEnv("CELLAR_ENABLE_TIER4"),
      enableFeasibilityPreSteps: boolEnv("CELLAR_ENABLE_PRE_STEPS"),
    };

    const tRun = Date.now();
    const result = await runGoalWithRustFallback(cel, goalConfig, callbacks);
    emitP("runGoalWithRustFallback", Date.now() - tRun);

    cortex?.shutdown();

    return { ...result, elementsDetected: ctx.elements.length };
  } finally {
    // Only disconnect if we own the adapter (created it ourselves)
    if (ownsAdapter) {
      await adapter.disconnect();
    }
  }
}

/** Native desktop path: use CEL accessibility + native input directly. */
async function runNative(
  cel: Cel,
  config: CelRunConfig,
): Promise<CelRunResult> {
  const actCache = config.cache ? new ActCache(new MemoryCacheStorage()) : undefined;

  const callbacks: GoalRunnerCallbacks = {
    getContext: async () => cel.getContext(),
    screenshot: async () => cel.captureScreen(),
    stateFingerprint: () => {
      const ctx = cel.getQuickContext();
      return `${ctx.app}::${ctx.window}`;
    },
    onStepPlanned: config.onStep
      ? (step, i) => config.onStep!(i, step.action.type, step.reasoning)
      : undefined,
  };

  // Boot cortex if requested
  let cortex: Cortex | undefined;
  if (config.cortex) {
    cortex = new Cortex(cel);
    await cortex.boot();
  }

  const nativeGoalConfig = {
    goal: config.goal,
    maxSteps: config.maxSteps ?? 30,
    taskTimeout: config.timeout ?? 120_000,
    maxConsecutiveFailures: 8,
    enableVision: config.vision ?? true,
    visionMode: "auto" as const,
    selfHeal: config.selfHeal ?? true,
    distillContext: true,
    enableContextLazy: !config.cortex,
    actCache,
    cortex,
    enableDecomposition: config.decompose ?? true,
    enableNotebook: true,
    workflowName: config.workflowName,
  };

  const result = await runGoalWithRustFallback(cel, nativeGoalConfig, callbacks);

  cortex?.shutdown();

  let elementsDetected = 0;
  try { elementsDetected = cel.getContext().elements.length; } catch {}

  return { ...result, elementsDetected };
}
