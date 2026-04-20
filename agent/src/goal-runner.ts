/**
 * Goal Runner — TypeScript implementation (LEGACY FALLBACK).
 *
 * @deprecated Use the Rust goal-runner (cel-goal-runner) via NAPI for new work.
 * This TS implementation is kept as a fallback path and for backward compatibility.
 * The Rust runner reads from the Cortex directly, plans via cel-planner, and
 * dispatches execution through the Cortex adapter system — all without FFI.
 *
 * See docs/architecture.md for the new architecture.
 *
 * Original architecture based on Stagehand v3 + Browser-Use patterns:
 * - Phase-based loop: prepare → plan → execute → validate
 * - DOM distillation (token reduction)
 * - Graduated failure recovery (nudges → forced done)
 * - Loop detection (repeat, ping-pong, stale context)
 * - Message compaction (40K char threshold)
 * - Vision modes (always/auto/never)
 * - Self-healing on action failures
 * - Multi-LLM routing (planner/observer/vision roles)
 */

import type { Cel } from "./cel-bindings.js";
import { celConfig } from "./config.js";
import { discoverCanonicalCdpTargets } from "./cdp-browser.js";
import { createLogger } from "./logger.js";

const logger = createLogger("goal-runner");
import type {
  ScreenContext,
  PlannedStep,
  PlannedAction,
  PlannerStepRecord,
  GoalMetrics,
  WorkflowStep,
} from "./types.js";
import { executeAction, type AdapterRegistry } from "./action-executor.js";
import { AdapterRegistry as FormalAdapterRegistry } from "./runtime/adapter-registry.js";
import { selfHeal, describeAction } from "./self-healer.js";
import { diffContexts, isDiffSignificant, formatDiffForPrompt } from "./context-differ.js";
import { isSkeletonScreen, skeletonWaitMs } from "./skeleton-detector.js";

// Re-export types for backward compatibility
export type { GoalRunnerConfig, GoalResult, GoalRunnerCallbacks, ActionResult } from "./goal-runner/config.js";
export { plannedToWorkflowAction } from "./goal-runner/helpers.js";

// Import from modules
import type { GoalRunnerConfig, GoalResult, GoalRunnerCallbacks } from "./goal-runner/config.js";
import { LoopDetector } from "./goal-runner/loop-detector.js";
import { distillContext, distillContextByGoal } from "./goal-runner/context-distiller.js";
import { shouldUseVision, refineWithZoom, type VisionMode } from "./goal-runner/vision-manager.js";
import { compactHistoryIfNeeded } from "./goal-runner/message-compactor.js";
import { getFailureNudge, getFailureEscalation, triggerReplan } from "./goal-runner/failure-recovery.js";
import { validateGrounding, validatePostAction } from "./goal-runner/validation.js";
import { validateAction } from "./goal-runner/validator.js";
import { planStep, PlannerConversation } from "./goal-runner/planner.js";
import {
  sleep,
  contextFingerprint,
  isTransitionAction,
  getActionTargetId,
  actionSignature,
  DEFAULT_SETTLE_MS,
  plannedToWorkflowAction,
  cachedStepMatchesContext,
} from "./goal-runner/helpers.js";

// ── Cognitive loop modules ────────────────────────────────────────────────────
import { CognitiveTrail } from "./goal-runner/cognitive-trail.js";
import { Notebook } from "./goal-runner/notebook.js";
import { StrategyTracker } from "./goal-runner/strategy-tracker.js";
import { CheckpointManager } from "./goal-runner/checkpoint-manager.js";
import { HistoryAdvisor } from "./goal-runner/history-advisor.js";
import { CortexBridge } from "./goal-runner/cortex-bridge.js";
import { PhaseProfiler } from "./goal-runner/phase-profiler.js";
import { ReplanEventEmitter } from "./goal-runner/replan-events.js";
import { filterPreSteps } from "./goal-runner/pre-step-safety.js";

// ── Extracted modules ────────────────────────────────────────────────────────
import { verifyDone } from "./goal-runner/verify-done.js";
import { routeGoal, openAppActions, type GoalRoute } from "./goal-runner/goal-router.js";

// ─── Pre-Done Verification (re-exported for backwards compatibility) ─────────
export { verifyDone } from "./goal-runner/verify-done.js";
export { routeGoal, type GoalRoute } from "./goal-runner/goal-router.js";

// ─── (Inline verifyDone, GoalRoute, routeGoal removed — now in separate files) ──

// ─── Milestone decomposition helper (shared by pre-flight and Tier 4) ─────────
interface DecomposeArgs {
  cel: Cel;
  callbacks: GoalRunnerCallbacks;
  config: GoalRunnerConfig;
  maxSteps: number;
  historyAdvice: string | null;
  cognitiveTrail: CognitiveTrail;
  strategyTracker: StrategyTracker;
  setMilestone: (m: string) => void;
  setMilestonesContext: (s: string) => void;
  /** Optional extra context appended to the prompt — used for Tier 4 reassess. */
  additionalContext?: string;
  /** Pre-extracted initial context (avoids redundant getContext() calls). */
  initialCtx?: ScreenContext;
  /** When provided, the helper increments this on its LLM call so the caller's metrics reflect pre-flight cost. */
  metrics?: { llmCalls: number };
}

/**
 * Run milestone decomposition. Returns true when new milestones were
 * successfully produced and registered, false when the LLM call/parse failed
 * or returned no milestones (the caller's existing state is unchanged).
 */
async function decomposeMilestones(args: DecomposeArgs): Promise<boolean> {
  const { cel, callbacks, config, maxSteps, historyAdvice, cognitiveTrail, strategyTracker,
    setMilestone, setMilestonesContext, additionalContext, initialCtx, metrics } = args;
  try {
    const ctx = initialCtx ?? await callbacks.getContext();
    const elementLabels = ctx.elements
      .slice(0, 20)
      .map(e => e.label || e.element_type)
      .join(", ");
    const decompPrompt =
      `Decompose this goal into 3-6 milestones. Each milestone is a human-recognizable checkpoint (not individual actions).\n` +
      `Goal: "${config.goal}"\n` +
      `Current: App=${ctx.app}, Window=${ctx.window}\n` +
      `Elements: ${elementLabels}\n` +
      `Step budget: ${maxSteps}\n` +
      (historyAdvice ? `Past experience:\n${historyAdvice}\n` : "") +
      (additionalContext ? `${additionalContext}\n` : "") +
      `Return JSON: {"milestones": [{"label": "on_search_page", "description": "Navigate to search", "step_budget": 5}]}\nJSON only:`;
    const raw = await cel.llmComplete(decompPrompt, config.goal, 512);
    if (metrics) metrics.llmCalls++;
    const cleaned = raw.replace(/```json?\n?/g, "").replace(/```/g, "").trim();
    const result = JSON.parse(cleaned);
    if (result.milestones && Array.isArray(result.milestones) && result.milestones.length > 0) {
      const lines = result.milestones.map(
        (m: { label: string; description: string; step_budget: number }, i: number) =>
          `${i + 1}. ${m.label} — ${m.description} (~${m.step_budget} steps)`,
      );
      setMilestonesContext(
        `MILESTONES:\n${lines.join("\n")}\nWhen you reach a milestone, set progress to "milestone:<label>".`,
      );
      cognitiveTrail.add(0, "THINK",
        `Decomposed into ${result.milestones.length} milestones: ${result.milestones.map((m: { label: string }) => m.label).join(", ")}`);
      strategyTracker.register(result.milestones[0].label, "initial");
      setMilestone(result.milestones[0].label);
      return true;
    }
    return false;
  } catch {
    // decomposition LLM/parse failed — leave caller state untouched
    return false;
  }
}

/**
 * Tier 4: re-assess the goal entirely, re-decompose with failure context.
 * Notebook is NOT cleared — discovered data is preserved across reassessment.
 * Global strategy budget is reset ONLY when re-decomposition actually succeeded —
 * otherwise the tracker keeps its exhausted state so we don't grant free budget
 * to the already-failing old milestones.
 */
async function rerunMilestoneDecomposition(args: DecomposeArgs): Promise<boolean> {
  const failed = args.strategyTracker.getFailedStrategies();
  const additional = failed.length > 0
    ? `FAILED STRATEGIES across all milestones: ${failed.join("; ")}.\nRe-plan with a fundamentally different structure.`
    : undefined;
  const decomposed = await decomposeMilestones({ ...args, additionalContext: additional });
  if (decomposed) {
    args.strategyTracker.resetGlobalCounter();
    args.cognitiveTrail.add(0, "REPLAN",
      "Tier 4: milestones re-decomposed with failure context (global strategy budget reset)");
  } else {
    args.cognitiveTrail.add(0, "REPLAN",
      "Tier 4: re-decomposition failed — global strategy budget NOT reset");
  }
  return decomposed;
}

// ─── Main entry point ─────────────────────────────────────────────────────────
export async function runGoal(
  cel: Cel,
  config: GoalRunnerConfig,
  callbacks: GoalRunnerCallbacks,
  adapters?: AdapterRegistry,
  registry?: FormalAdapterRegistry,
): Promise<GoalResult> {
  const maxSteps = config.maxSteps ?? 30;
  const stepDelay = config.stepDelay ?? 500;
  const taskTimeout = config.taskTimeout ?? 120_000;
  const maxConsecutiveFailures = config.maxConsecutiveFailures ?? 8;
  const enableVision = config.enableVision ?? true;
  const selfHealEnabled = config.selfHeal ?? true;
  const selfHealMaxAttempts = config.selfHealMaxAttempts ?? 2;
  const variables = config.variables ?? {};
  const startTime = Date.now();

  // ── Replan-hardening rollout flags (defaults preserve pre-96f5db0 behavior) ─
  const enableTierReplan = config.enableTierReplan ?? false;
  const enableSemanticStall = config.enableSemanticStallEscalation ?? false;
  const enableTier4Reassessment = config.enableTier4Reassessment ?? false;
  const enableFeasibilityPreSteps = config.enableFeasibilityPreSteps ?? false;
  // Fingerprint-keyed cache of verifyGoal() to avoid calling it on every stall step.
  let lastVerifyGoalFp: string | null = null;
  let lastVerifyGoalResult = false;

  // ── CACHE LOOKUP ──────────────────────────────────────────────────────
  const initialFingerprint = callbacks.stateFingerprint?.() ?? "";
  if (config.actCache) {
    const cached = await config.actCache.lookup(config.goal, initialFingerprint, variables);
    if (cached) {
      const cacheResult = await replayCachedActions(
        cached.key, cached.actions, cel, callbacks, config, adapters,
      );
      if (cacheResult) return cacheResult;
    }
  }

  const contextLazy = config.enableContextLazy ?? false;
  const basePlannerModel = celConfig.llmPlannerModel;
  const escalationModel = celConfig.llmEscalationModel;
  const ESCALATION_THRESHOLD = 2;
  const history: PlannerStepRecord[] = [];
  const loopDetector = new LoopDetector();
  let loopWarning: string | null = null;
  let tentativePlan: PlannedStep[] = [];
  let pendingSpeculativePlan: Promise<PlannedStep | null> | null = null;
  let preemptiveContextPromise: Promise<ScreenContext> | null = null;
  let consecutiveFailures = 0;
  let lastStateFingerprint: string | undefined;
  let cachedContext: ScreenContext | null = null;
  let previousContext: ScreenContext | null = null;
  let lastActionWasTransition = false;
  let requestedContextTier: import("./types.js").ContextTier = contextLazy ? "none" : "full";
  const perStepTimeout = config.stepTimeout ?? 30_000;
  const maxStepsWithoutProgress = config.maxStepsWithoutProgress ?? 10;
  let stepsSinceProgress = 0;
  let lastProgressFingerprint: string | undefined;

  // Persistent LLM conversation thread — maintains context across steps.
  // Instead of rebuilding full context each step, sends diffs after step 0.
  // Enabled via config.persistentThread (default: true).
  const usePersistentThread = config.persistentThread ?? true;
  const conversation = usePersistentThread
    ? new PlannerConversation("You are a desktop automation agent. Observe UI elements and take actions to achieve goals. Respond with JSON containing: reasoning, actions array, expected_outcome, confidence.")
    : null;

  // Device baseline for blind planning
  let deviceBaselineJson: string | null = null;
  let deviceBaseline: import("./device-baseline.js").DeviceBaseline | null = null;
  if (contextLazy) {
    try {
      const { getOrScanBaseline } = await import("./device-baseline.js");
      deviceBaseline = getOrScanBaseline(cel);
      deviceBaselineJson = JSON.stringify(deviceBaseline);
    } catch { /* baseline unavailable */ }
  }

  // ── GOAL ROUTER: LLM-powered classification (~100ms with Gemini Flash) ──
  // Routes simple goals to deterministic execution, complex goals to full planner.
  // Skip the router when running through a browser adapter — the fast paths use
  // native macOS input (Spotlight, keyboard shortcuts) which would type into the
  // user's desktop instead of the headless browser.
  const skipRouter = config.skipRouter ?? false;
  const { route, actions: routedActions } = skipRouter
    ? { route: { route: "needs_planning", reason: "router skipped" } as GoalRoute, actions: null }
    : await routeGoal(cel, config.goal, deviceBaseline);
  if (routedActions) {
    const templateHistory: PlannerStepRecord[] = [];
    for (let i = 0; i < routedActions.length; i++) {
      const action = routedActions[i];
      try {
        if (action.type === "activate_app" && "app_name" in action) {
          (cel as any).activateApp?.(action.app_name);
        } else if (action.type === "key_combo") cel.keyCombo(action.keys);
        else if (action.type === "key") cel.keyPress(action.key);
        else if (action.type === "type" && "text" in action) cel.typeText(action.text);
        else if (action.type === "wait" && "ms" in action) await sleep(action.ms);
        templateHistory.push({ step_index: i, action, success: true });
        if (i < routedActions.length - 1) await sleep(200);
      } catch (e) {
        templateHistory.push({ step_index: i, action, success: false, error: String(e) });
        break;
      }
    }
    if (templateHistory.every((h) => h.success)) {
      return {
        status: "achieved" as const,
        summary: `Goal routed as ${route.route}: ${config.goal}`,
        totalSteps: templateHistory.length,
        history: templateHistory,
        metrics: {
          totalMs: Date.now() - startTime,
          contextExtractionMs: 0,
          llmCalls: 1, // router call
          visionCalls: 0,
          errorCount: 0,
          stateChanges: 0,
          loopWarnings: 0,
        },
      };
    }
    // Route partially failed — fall through to full LLM planning
  }

  // ── READ_DATA FAST PATH: CDP extraction without planner ──────────────
  if (route.route === "read_data" && route.extraction) {
    try {
      const { extractFromPage, dismissCookieBanner } = await import("./cdp-extractor.js");

      // Navigate if URL provided
      if (route.url) {
        // Try CDP navigate first (if a browser target already exists)
        let navigated = false;
        try {
          const targets = await discoverCanonicalCdpTargets(cel);
          if (targets.length > 0) {
            await cel.cdpNavigate(route.url);
            await sleep(3000);
            navigated = true;
          }
        } catch { /* CDP not available */ }

        if (!navigated) {
          // Fallback: open browser via Spotlight, then use address bar
          const spotlightKeys = deviceBaseline?.shortcuts?.spotlight ?? ["Cmd", "Space"];
          cel.keyCombo(spotlightKeys);
          await sleep(500);
          cel.typeText("Chrome");
          await sleep(300);
          cel.keyPress("Enter");
          await sleep(1500);
          cel.keyCombo(["Cmd", "L"]);
          await sleep(200);
          cel.typeText(route.url);
          cel.keyPress("Enter");
          await sleep(3000);
        }
      }

      // Dismiss cookie banners
      await dismissCookieBanner(cel);
      await sleep(500);

      // Extract data
      const extracted = await extractFromPage(cel, route.extraction);
      if (extracted && extracted.trim().length > 10) {
        return {
          status: "achieved" as const,
          summary: extracted,
          totalSteps: 0,
          history: [],
          metrics: {
            totalMs: Date.now() - startTime,
            contextExtractionMs: 0,
            llmCalls: 2, // router + extractor LLM (if used)
            visionCalls: 0,
            errorCount: 0,
            stateChanges: 0,
            loopWarnings: 0,
          },
        };
      }
      // Extraction failed — fall through to planner
    } catch {
      // CDP not available — fall through to planner
    }
  }

  // ── MULTI-STEP FAST PATH: execute sequential sub-goals ──
  if (route.route === "multi_step" && route.steps && route.steps.length > 0) {
    try {
      const { extractFromPage, dismissCookieBanner } = await import("./cdp-extractor.js");
      const results: string[] = [];
      let totalLlmCalls = 1; // router call

      for (const step of route.steps) {
        if (step.route === "read_data" && step.url && step.extraction) {
          // Navigate
          try {
            const targets = await discoverCanonicalCdpTargets(cel);
            if (targets.length > 0) {
              await cel.cdpNavigate(step.url);
              await sleep(3000);
            } else {
              // Fallback: address bar
              cel.keyCombo(["Cmd", "L"]);
              await sleep(200);
              cel.typeText(step.url);
              cel.keyPress("Enter");
              await sleep(3000);
            }
          } catch {
            cel.keyCombo(["Cmd", "L"]);
            await sleep(200);
            cel.typeText(step.url);
            cel.keyPress("Enter");
            await sleep(3000);
          }

          // Dismiss cookies
          await dismissCookieBanner(cel);
          await sleep(500);

          // Extract
          const extracted = await extractFromPage(cel, step.extraction);
          if (extracted && extracted.trim().length > 5) {
            results.push(extracted);
            totalLlmCalls++;
          } else {
            results.push(`[Failed to extract from ${step.url}]`);
          }
        } else if (step.route === "navigate_url" && step.url) {
          try {
            await cel.cdpNavigate(step.url);
            await sleep(2000);
          } catch {
            cel.keyCombo(["Cmd", "L"]);
            await sleep(200);
            cel.typeText(step.url);
            cel.keyPress("Enter");
            await sleep(2000);
          }
        } else if (step.route === "search_web" && step.query) {
          const searchUrl = `https://www.google.com/search?q=${encodeURIComponent(step.query)}`;
          try {
            await cel.cdpNavigate(searchUrl);
            await sleep(3000);
          } catch {
            cel.keyCombo(["Cmd", "L"]);
            await sleep(200);
            cel.typeText(searchUrl);
            cel.keyPress("Enter");
            await sleep(3000);
          }

          // Google may redirect to consent page — dismiss and retry
          await dismissCookieBanner(cel);
          await sleep(1000);
          // Check if we're still on consent page and need to wait for redirect
          try {
            const currentUrl = await cel.cdpEvaluate("window.location.href") as string;
            if (currentUrl && typeof currentUrl === "string" && currentUrl.includes("consent")) {
              await sleep(2000); // Wait for redirect after consent
            }
          } catch { /* ignore */ }

          if (step.extraction) {
            const extracted = await extractFromPage(cel, step.extraction);
            if (extracted && extracted.trim().length > 5) {
              results.push(extracted);
            } else {
              // Fallback: try LLM-generated extraction
              results.push(`[Search results for: ${step.query} — extraction pending]`);
            }
            totalLlmCalls++;
          }
        }
      }

      if (results.length > 0) {
        return {
          status: "achieved" as const,
          summary: results.join("\n\n---\n\n"),
          totalSteps: route.steps.length,
          history: [],
          metrics: {
            totalMs: Date.now() - startTime,
            contextExtractionMs: 0,
            llmCalls: totalLlmCalls,
            visionCalls: 0,
            errorCount: 0,
            stateChanges: 0,
            loopWarnings: 0,
          },
        };
      }
    } catch {
      // multi_step failed — fall through to planner
    }
  }

  // Metrics
  const metrics: GoalMetrics = {
    totalMs: 0, contextExtractionMs: 0, llmCalls: 0, visionCalls: 0,
    errorCount: 0, stateChanges: 0, loopWarnings: 0,
  };

  function makeResult(status: GoalResult["status"], summary: string, totalSteps: number): GoalResult {
    metrics.totalMs = Date.now() - startTime;

    // Post-run analysis: extract observations + update working memory
    if (config.workflowName) {
      try {
        const { processPostRun } = require("./post-run.js") as typeof import("./post-run.js");
        const stepResults = history.map((h, i) => ({
          stepIndex: i,
          stepId: `step-${i}`,
          description: `${h.action.type}${h.error ? `: ${h.error}` : ""}`,
          success: h.success,
          confidence: 0.8,
        }));
        processPostRun(cel as any, config.workflowName, Date.now(), stepResults, undefined, cognitiveTrail);
      } catch (e) {
        // cel-store may genuinely be unavailable in some environments, but
        // silent swallow hid real bugs during development — log at warn level.
        console.warn(`[goal-runner] post-run failed: ${String(e).slice(0, 200)}`);
      }
    }

    return {
      status, summary, totalSteps, history, metrics: { ...metrics },
      conversationThread: conversation?.messages,
    };
  }

  // ═══════════════════════════════════════════════════════════════════════
  // COGNITIVE LOOP INITIALIZATION
  // ═══════════════════════════════════════════════════════════════════════

  const cognitiveTrail = new CognitiveTrail();
  const notebook = config.enableNotebook !== false ? new Notebook() : null;
  const strategyTracker = new StrategyTracker();
  const checkpointManager = new CheckpointManager();
  const cortexBridge = config.cortex ? new CortexBridge(config.cortex as any, cel) : null;
  // Per-goal replan event emitter. Opt-in via CELLAR_REPLAN_EVENTS=1.
  // Listeners can also subscribe programmatically via emitReplanEvent.subscribe(...)
  // for in-process consumers (UIs, tests).
  const goalId = `goal-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
  const emitReplanEvent = new ReplanEventEmitter(goalId);

  // Track batch_next signal from LLM
  let batchNextRequested = false;
  // Track current milestone for strategy scoping
  let currentMilestone = "default";
  // Populated by pre-flight milestone decomposition (also may be repopulated by Tier 4)
  let milestonesContext = "";
  // Last injected milestone content — allows Tier 4 re-decomposition to re-inject
  // without repeating the same content every step. null = inject on next opportunity.
  let milestonesInjectedFor: string | null = null;
  // Hard cap on Tier 4 re-assessments. Goal fails on the second Tier 4 to prevent
  // wasting remaining step budget on futile re-decomposition LLM calls.
  let tier4Count = 0;

  // Simple-goal heuristic: extraction/read-only requests complete in 1-2 steps
  // and don't benefit from feasibility check or milestone decomposition.
  // Matches goals that either START with an extraction verb OR embed one
  // prominently (e.g. "You are on Hacker News. Extract the titles…") while
  // lacking multi-step interaction verbs.
  const trimmedGoal = config.goal.trim();
  const startsExtract = /^(extract|read|get|show|list|what is|how many)/i.test(trimmedGoal);
  const embedsExtract = /\b(extract|scrape|return (?:as )?(?:a )?(?:structured )?list)\b/i.test(trimmedGoal);
  const hasMultiStep = /\bclick\b|\bnavigate\b|\bgo to\b|\bfill\b|\bsubmit\b|\bthen\b|\bafter\b|\bnext\b/i.test(trimmedGoal);
  const isSimpleGoal = (startsExtract || embedsExtract) && !hasMultiStep;

  // ═══════════════════════════════════════════════════════════════════════
  // PRE-FLIGHT — runs in order: history → feasibility (+ pre_steps) → decompose
  // Each stage consumes the previous stage's output. Early-exits on infeasibility.
  // ═══════════════════════════════════════════════════════════════════════

  // STAGE 1: History advice — query cel-store for past experience
  let historyAdvice: string | null = null;
  if (config.workflowName) {
    try {
      historyAdvice = await HistoryAdvisor.query(cel, config.goal, config.workflowName);
    } catch { /* cel-store not available — proceed without advice */ }
  }

  // Shared initial context — pre-flight stages 2 & 3 plus step 0 all read the
  // same page state, so extract once (large pages like HN can cost seconds per
  // call) and thread the snapshot through.
  let preflightCtx: ScreenContext | null = null;
  async function getPreflightCtx(): Promise<ScreenContext> {
    if (preflightCtx) return preflightCtx;
    const pfStart = Date.now();
    preflightCtx = await callbacks.getContext();
    metrics.contextExtractionMs += Date.now() - pfStart;
    return preflightCtx;
  }

  // STAGE 2: Feasibility check — can we reach the goal from current state?
  // Runs BEFORE decomposition so an infeasible goal fails fast without wasting
  // the decomposition LLM call. History is fed in so past failures inform this.
  let feasibilityNote: string | null = null;
  if (config.enableFeasibilityCheck && maxSteps > 10 && !isSimpleGoal) {
    try {
      const initialCtx = await getPreflightCtx();
      const elementLabels = initialCtx.elements
        .slice(0, 20)
        .map(e => e.label || e.element_type)
        .join(", ");
      // Cap history snippet so the feasibility prompt (256-token budget) can't
      // blow past its input window — advice is best-effort context, not required.
      const historyForFeasibility = historyAdvice ? historyAdvice.slice(0, 600) : null;
      const feasibilityPrompt =
        `Is this goal feasible from the current state? Answer JSON only.\n` +
        `Goal: "${config.goal}"\n` +
        `Current: App=${initialCtx.app}, Window=${initialCtx.window}\n` +
        `Elements: ${elementLabels}\n` +
        (historyForFeasibility ? `Past experience:\n${historyForFeasibility}\n` : "") +
        `pre_steps must be SHORT app-open commands only (e.g. "open Chrome", "launch Safari").\n` +
        `{"feasible": true/false, "reason": "...", "pre_steps": ["open Chrome"]}\n` +
        `JSON only:`;
      const raw = await cel.llmComplete(feasibilityPrompt, config.goal, 256);
      metrics.llmCalls++; // pre-flight feasibility counts as an LLM call
      const cleaned = raw.replace(/```json?\n?/g, "").replace(/```/g, "").trim();
      try {
        const result = JSON.parse(cleaned);
        if (result.feasible === false) {
          if (result.pre_steps && Array.isArray(result.pre_steps) && result.pre_steps.length > 0) {
          if (!enableFeasibilityPreSteps) {
            // Log-only default — safer because auto-exec types into the real
            // desktop via Spotlight. Enable with enableFeasibilityPreSteps.
            cognitiveTrail.add(0, "THINK", `Feasibility: not ready — ${result.reason}. Pre-steps suggested (auto-exec disabled): ${result.pre_steps.join(", ")}`);
            feasibilityNote = `Pre-steps suggested (not executed): ${result.pre_steps.join(", ")}.`;
          } else {
            // Pre-step execution goes through filterPreSteps which enforces
            // the full safety checklist: allowlist (browsers only), blocklist
            // (Passwords/Keychain/Mail/Terminal), per-goal cap (1), array
            // length cap (3), strict regex. See pre-step-safety.ts.
            const { allowed, rejected } = filterPreSteps(result.pre_steps);
            if (rejected.length > 0) {
              cognitiveTrail.add(0, "THINK",
                `Feasibility pre_steps REJECTED by safety filter: ${rejected.map(r => `"${r.raw}" (${r.reason})`).join("; ")}`);
            }
            let anyExecuted = false;
            if (allowed.length > 0) {
              cognitiveTrail.add(0, "THINK",
                `Feasibility: not ready — ${result.reason}. Running safe pre-steps: ${allowed.map(a => a.appName).join(", ")}`);
              for (const step of allowed) {
                const preActions = openAppActions(step.appName, deviceBaseline?.shortcuts?.spotlight);
                for (const a of preActions) {
                  try {
                    if (a.type === "activate_app" && "app_name" in a) (cel as any).activateApp?.(a.app_name);
                    else if (a.type === "key_combo") cel.keyCombo(a.keys);
                    else if (a.type === "key") cel.keyPress(a.key);
                    else if (a.type === "type" && "text" in a) cel.typeText(a.text);
                    else if (a.type === "wait" && "ms" in a) await sleep(a.ms);
                    anyExecuted = true;
                  } catch { /* pre-step action failed — best effort */ }
                }
              }
            } else {
              cognitiveTrail.add(0, "THINK",
                `Feasibility: not ready — ${result.reason}. No pre_steps passed safety filter.`);
            }
            feasibilityNote = anyExecuted
              ? `Pre-steps ran: ${allowed.map(a => a.appName).join(", ")}.`
              : `Pre-steps suggested but none passed safety filter (${rejected.length} rejected).`;
          } // end enableFeasibilityPreSteps
          } else {
            // No pre_steps — goal is truly blocked from here, fail fast.
            const failResult = makeResult("failed", `Goal not feasible: ${result.reason}`, 0);
            callbacks.onComplete?.(failResult);
            return failResult;
          }
        } else {
          cognitiveTrail.add(0, "THINK", `Feasibility: OK — ${result.reason || "goal achievable from current state"}`);
          feasibilityNote = typeof result.reason === "string" ? result.reason : null;
        }
      } catch { /* Parse failed — proceed anyway */ }
    } catch { /* LLM call failed — proceed without feasibility check */ }
  }

  // STAGE 3: Milestone decomposition — advisory checkpoints for progress tracking.
  // Receives both history and feasibility context so milestones account for any
  // pre-step state change or known blockers. Reuses the pre-flight context
  // snapshot so we don't re-extract from a potentially large page.
  if (config.enableDecomposition && maxSteps > 15 && !isSimpleGoal) {
    await decomposeMilestones({
      cel, callbacks, config, maxSteps, historyAdvice,
      cognitiveTrail, strategyTracker,
      setMilestone: (m: string) => { currentMilestone = m; },
      setMilestonesContext: (s: string) => { milestonesContext = s; },
      additionalContext: feasibilityNote ? `Feasibility context: ${feasibilityNote}` : undefined,
      initialCtx: preflightCtx ?? undefined,
      metrics,
    });
  }

  // Seed the step-0 context cache with the pre-flight extraction (if any) so
  // the main loop doesn't re-extract on its first iteration.
  if (preflightCtx) cachedContext = preflightCtx;

  // Track extract/done attempts for auto-promotion when verifyGoal keeps failing
  let extractAttempts = 0;
  let lastExtractData = "";
  let doneAttempts = 0;
  let lastDoneSummary = "";

  // Fix 1: Track repeated clicks on same target for click-loop detection
  let lastClickTarget = "";
  let sameClickCount = 0;

  // Track consecutive scrolls for scroll-loop breaker
  let consecutiveScrolls = 0;

  // Fix 3: Track consecutive notebook_writes for notebook-loop detection
  let consecutiveNotebookWrites = 0;

  // ═══════════════════════════════════════════════════════════════════════
  // MAIN LOOP — Phase-based: Prepare → Plan → Execute → Validate
  // ═══════════════════════════════════════════════════════════════════════

  for (let stepIndex = 0; stepIndex < maxSteps; stepIndex++) {
    const profiler = PhaseProfiler.start(stepIndex);
    profiler.mark("limits");
    logger.debug("run-goal phase", { stepIndex, phase: "limits" });
    // ── PHASE 0: CHECK limits ─────────────────────────────────────────
    if (Date.now() - startTime > taskTimeout) {
      const result = makeResult("timeout", `Timeout after ${taskTimeout}ms`, stepIndex);
      callbacks.onComplete?.(result);
      return result;
    }
    // Escalation check: signal orchestrator to replan before auto-failing
    if (config.validator) {
      const escalation = getFailureEscalation(
        consecutiveFailures, loopDetector.shouldAutoFail(), loopDetector.loopCount ?? 0,
      );
      if (escalation.level === "replan") {
        const result = makeResult("failed", escalation.message, stepIndex);
        result.escalation = "replan";
        callbacks.onComplete?.(result);
        return result;
      }
      if (escalation.level === "abort") {
        const result = makeResult("failed", escalation.message, stepIndex);
        result.escalation = "abort";
        callbacks.onComplete?.(result);
        return result;
      }
    }

    if (consecutiveFailures >= maxConsecutiveFailures) {
      const result = makeResult("failed", `Too many consecutive failures (${maxConsecutiveFailures})`, stepIndex);
      callbacks.onComplete?.(result);
      return result;
    }
    if (loopDetector.shouldAutoFail()) {
      const result = makeResult("failed", "Stuck in action loop", stepIndex);
      callbacks.onComplete?.(result);
      return result;
    }
    if (stepsSinceProgress >= maxStepsWithoutProgress) {
      const result = makeResult("failed", `No progress detected after ${maxStepsWithoutProgress} steps`, stepIndex);
      callbacks.onComplete?.(result);
      return result;
    }

    // ── PERCEIVE: cortex interrupts + batch gating ─────────────────────
    if (cortexBridge) {
      // Handle interrupts inline (dismiss dialogs, wait for loading)
      const handled = await cortexBridge.handleInterrupts();
      for (const desc of handled) {
        cognitiveTrail.add(stepIndex, "INTERRUPT", desc);
      }
      // Gate batch_next: if page is still changing, force context refresh
      if (batchNextRequested && !cortexBridge.isSettled()) {
        batchNextRequested = false; // Override — page still changing
      }
    }

    // ── PHASE 1: PREPARE (observe context) ────────────────────────────
    profiler.mark("prepare");
    logger.debug("run-goal phase", { stepIndex, phase: "prepare:start" });
    let context: ScreenContext = cachedContext ?? { app: "", window: "", elements: [], timestamp_ms: Date.now() } as ScreenContext;

    // batch_next optimization: skip context re-gathering when LLM is confident
    if (batchNextRequested && cachedContext && consecutiveFailures === 0) {
      context = cachedContext;
      batchNextRequested = false; // Consume the signal
    }
    // Cortex path: mental model is already current — instant read.
    // The cortex accepts a pluggable context provider, so it works with any
    // adapter (desktop a11y, browser CDP, etc.).
    else if (config.cortex?.isRunning() && (config.cortex as any).model?.currentContext) {
      const freshness = cortexBridge?.readFreshness() ?? (config.cortex as any).model?.freshness ?? null;
      if (freshness?.state === "hard-stale") {
        const ctxStart = Date.now();
        context = await callbacks.getContext();
        metrics.contextExtractionMs += Date.now() - ctxStart;
        cachedContext = context;
        loopWarning = (loopWarning ?? "") + `\nSTATE REFRESH: cortex model was hard-stale (${freshness.causes.join(", ") || "unknown"}).`;
        console.info("[runtime-route] hard-stale model forced refresh", {
          causes: freshness.causes,
          ageMs: freshness.ageMs,
          confidence: freshness.confidence,
        });
      } else {
        context = (config.cortex as any).model.currentContext;
        cachedContext = context;
      }
    } else if (contextLazy && requestedContextTier === "none" && deviceBaselineJson) {
      context = cachedContext ?? { app: "", window: "", elements: [], timestamp_ms: Date.now() };
    } else if (contextLazy && requestedContextTier === "minimal") {
      const ctxStart = Date.now();
      // Prefer adapter-provided tiered context; fall back to CEL quick context
      if (callbacks.getContextTier2) {
        context = await callbacks.getContextTier2();
      } else if (callbacks.getContextTier1) {
        context = await callbacks.getContextTier1();
      } else {
        context = cel.getQuickContext();
      }
      metrics.contextExtractionMs += Date.now() - ctxStart;
      cachedContext = context;
    } else {
      let needsFreshContext = cachedContext === null;

      // Use fingerprint-based caching (existing mechanism) — skip re-extraction
      // only when the page URL hasn't changed. Action-type-based caching is unsafe
      // because type/key/scroll can trigger DOM changes (autocomplete, form submit, lazy load).
      if (callbacks.stateFingerprint && cachedContext !== null) {
        const currentFP = callbacks.stateFingerprint();
        if (currentFP !== lastStateFingerprint) {
          needsFreshContext = true;
          lastStateFingerprint = currentFP;
        }
      } else {
        needsFreshContext = true;
      }

      if (needsFreshContext) {
        const ctxStart = Date.now();
        // Use preemptive context if available (started during previous settle wait)
        if (preemptiveContextPromise) {
          context = await preemptiveContextPromise;
          preemptiveContextPromise = null;
          // Still need screenshot separately if vision is enabled
          if (callbacks.screenshot && enableVision) {
            (callbacks as any)._cachedScreenshot = await callbacks.screenshot().catch(() => null);
          }
        } else if (callbacks.screenshot && enableVision) {
          // Run DOM extraction and screenshot capture in parallel.
          // Use Promise.allSettled to avoid screenshot failure killing context read.
          const [ctxResult, screenshotResult] = await Promise.allSettled([
            callbacks.getContext(),
            callbacks.screenshot(),
          ]);
          context = ctxResult.status === "fulfilled" ? ctxResult.value : await callbacks.getContext();
          const screenshot = screenshotResult.status === "fulfilled" ? screenshotResult.value : null;
          // Cap screenshot at 2MB to prevent OOM on 4K+ displays
          if (screenshot && screenshot.length <= 2 * 1024 * 1024) {
            (callbacks as any)._cachedScreenshot = screenshot;
          } else if (screenshot) {
            // Oversized screenshot — discard to prevent memory issues
            (callbacks as any)._cachedScreenshot = null;
          }
        } else {
          context = await callbacks.getContext();
        }
        metrics.contextExtractionMs += Date.now() - ctxStart;

        // Retry once if context is empty — catches timing issues where dynamic
        // content hasn't rendered yet (e.g., SPA hydration, lazy-loaded results).
        if (context.elements.length === 0 && callbacks.getContext) {
          await sleep(1500);
          const ctxRetryStart = Date.now();
          context = await callbacks.getContext();
          metrics.contextExtractionMs += Date.now() - ctxRetryStart;
        }

        cachedContext = context;
      } else {
        context = cachedContext!;
      }

      // Skeleton detection
      if (stepIndex === 0 || needsFreshContext) {
        const waitMs = skeletonWaitMs(context);
        if (waitMs > 0) {
          await sleep(waitMs);
          const ctxStart2 = Date.now();
          context = await callbacks.getContext();
          metrics.contextExtractionMs += Date.now() - ctxStart2;
          cachedContext = context;
        }
      }
    }

    // Defensive: ensure elements is always an array
    context.elements = context.elements ?? [];
    logger.debug("run-goal prepare complete", {
      stepIndex,
      phase: "prepare:done",
      elementCount: context.elements.length,
      app: context.app,
      focusedId: context.elements.find(e => e.state?.focused)?.id ?? null,
      labels: context.elements
        .slice(0, 6)
        .map((el) => ({
          id: el.id,
          label: el.label,
          type: el.element_type,
          value: el.value ? el.value.slice(0, 30) : undefined,
          focused: el.state?.focused ? true : undefined,
        })),
    });

    // Decide: blind vs full planning
    const isBlindMode = contextLazy && requestedContextTier === "none"
      && context.elements.length === 0 && deviceBaselineJson;

    // Vision mode — also check CortexBridge recommendation
    const visionMode: VisionMode = !enableVision ? "never" : (config.visionMode ?? "auto");
    const cortexWantsVision = cortexBridge?.isVisionNeeded() ?? false;
    const useVision = isBlindMode ? false : (cortexWantsVision || shouldUseVision(
      visionMode, stepIndex, context, consecutiveFailures, !!callbacks.screenshot, config.goal,
    ));

    // DOM distillation — always use goal-aware filtering when we have a goal
    if ((config.distillContext ?? true) && !isBlindMode && context.elements.length > 20) {
      context = distillContextByGoal(context, config.goal, route.extraction);
    }

    // Extract-first hint: disabled — was causing premature "done" claims.
    // The planner prompt now has verification rules (rule 17) that handle this properly.
    const extractFirstHint = "";

    // Two-step diff for compound actions
    let diffGoalSuffix = "";
    if (!isBlindMode && previousContext && lastActionWasTransition) {
      const diff = diffContexts(previousContext, context);
      if (isDiffSignificant(diff)) {
        diffGoalSuffix = "\n\n" + formatDiffForPrompt(diff);
      }
    }
    previousContext = context;

    // ── Cortex intelligence (supplements any context source) ──────────
    // Use CortexBridge for structured signal injection when available,
    // fall back to raw cortex access for backward compatibility.
    let cortexPromptSignals = "";
    if (cortexBridge) {
      const signals = cortexBridge.poll();
      const signalText = cortexBridge.getPromptSignals(signals);
      if (signalText) cortexPromptSignals = signalText;
      // Action-required signals also go into loopWarning for stronger emphasis
      const urgent = signals.filter(s => s.actionRequired);
      if (urgent.length > 0) {
        loopWarning = (loopWarning ?? "") + "\n" + urgent.map(s => `URGENT: ${s.description}`).join("\n");
      }
    } else if (config.cortex?.isRunning()) {
      // Legacy path: direct cortex access without CortexBridge
      const anomalies = config.cortex.consumeAnomalies();
      if (anomalies.length > 0) {
        const anomalyHints = anomalies.map((a) => a.description).join("; ");
        loopWarning = (loopWarning ?? "") + `\nANOMALY: ${anomalyHints}`;
      }
      const temporal = (config.cortex as any).model?.temporal;
      if (temporal?.loading?.detected && (temporal.loading.durationMs ?? temporal.loading.duration_ms ?? 0) > 2000) {
        const loadMs = temporal.loading.durationMs ?? temporal.loading.duration_ms ?? 0;
        loopWarning = (loopWarning ?? "") + `\nLoading state detected for ${loadMs}ms — consider waiting.`;
      }
      if (temporal?.errorPersisting?.detected) {
        loopWarning = (loopWarning ?? "") + `\nPersistent error: "${temporal.errorPersisting.message ?? "unknown"}"`;
      }
    }

    // ── PHASE 2: PLAN ─────────────────────────────────────────────────
    profiler.mark("plan");
    logger.debug("run-goal phase", { stepIndex, phase: "plan:start" });
    let step: PlannedStep;
    let effectiveGoal = config.goal;
    if (extractFirstHint) effectiveGoal += extractFirstHint;
    if (diffGoalSuffix) effectiveGoal += diffGoalSuffix;
    // Inject notebook data as compact one-liner
    if (notebook && !notebook.isEmpty) {
      effectiveGoal += `\n${notebook.toPromptContext()}`;
    }
    // Inject history advice on step 0 only
    if (stepIndex === 0 && historyAdvice) {
      effectiveGoal += `\n\n${historyAdvice}`;
    }
    // Inject milestones (from decomposition) once per unique content. Covers
    // both step 0 (initial decomp) and Tier 4 re-decomp (milestonesInjectedFor
    // is reset to null in the Tier 4 branch, forcing re-injection).
    if (milestonesContext && milestonesContext !== milestonesInjectedFor) {
      effectiveGoal += `\n\n${milestonesContext}`;
      milestonesInjectedFor = milestonesContext;
    }
    const failureNudge = getFailureNudge(consecutiveFailures);
    if (failureNudge) effectiveGoal += failureNudge;
    // Inject cortex signals (page state awareness: loading, idle, errors, spinners)
    if (cortexPromptSignals) {
      effectiveGoal += `\n\nPAGE STATE:\n${cortexPromptSignals}`;
    }
    // Batch hint: detect form-like patterns and generate a form map for the planner
    if (!isBlindMode && consecutiveFailures === 0) {
      const textInputs = context.elements.filter(
        (el) => el.element_type === "textfield" || el.element_type === "input"
          || el.element_type === "textarea" || el.element_type === "combobox"
          || el.element_type === "select",
      );
      const radioButtons = context.elements.filter((el) => el.element_type === "radio_button");
      const checkboxes = context.elements.filter((el) => el.element_type === "checkbox");
      const submitButtons = context.elements.filter(
        (el) => el.element_type === "button" && /submit|calculate|search|send|go|order/i.test(el.label || ""),
      );
      const totalFormElements = textInputs.length + radioButtons.length + checkboxes.length;
      if (totalFormElements >= 2) {
        let hint = `\n\nFORM DETECTED (${totalFormElements} fields). Batch ALL actions in ONE response:`;
        if (textInputs.length > 0) hint += `\n- ${textInputs.length} text inputs → use set_value or type`;
        if (radioButtons.length > 0) hint += `\n- ${radioButtons.length} radio buttons → use CLICK (not type)`;
        if (checkboxes.length > 0) hint += `\n- ${checkboxes.length} checkboxes → use CLICK (not type)`;
        if (submitButtons.length > 0) hint += `\n- Submit button: "${submitButtons[0].label}" → CLICK it last`;
        hint += `\nAfter filling all fields, submit with click or key_combo ["Enter"].`;
        effectiveGoal += hint;
      }
    }

    // Feed conversation with context diff (persistent thread)
    if (conversation && !isBlindMode) {
      const lastHist = history.length > 0 ? history[history.length - 1] : null;
      const lastActionStr = lastHist ? actionSignature(lastHist.action) : undefined;
      const lastErr = lastHist?.error;
      const userMsg = conversation.buildUserMessage(stepIndex, effectiveGoal, context, lastActionStr, lastErr);
      conversation.addUserMessage(userMsg);
    }

    try {
      // Resolve pending speculative plan from previous iteration
      if (pendingSpeculativePlan && tentativePlan.length === 0) {
        const specResult = await pendingSpeculativePlan;
        pendingSpeculativePlan = null;
        if (specResult) {
          tentativePlan.push(specResult);
        }
      } else if (pendingSpeculativePlan) {
        pendingSpeculativePlan = null;
      }

      if (isBlindMode) {
        step = await cel.planStepBlind(effectiveGoal, history, deviceBaselineJson!, {
          maxSteps, loopWarning: loopWarning ?? undefined,
        });
        metrics.llmCalls++;
      } else if (tentativePlan.length > 0) {
        const cached = tentativePlan[0];
        if (cachedStepMatchesContext(cached, context)) {
          step = tentativePlan.shift()!;
        } else {
          tentativePlan = [];
          const escalate = escalationModel && consecutiveFailures >= ESCALATION_THRESHOLD ? escalationModel : basePlannerModel;
          step = await planStep(cel, effectiveGoal, context, history, loopWarning, maxSteps, useVision, callbacks, deviceBaselineJson, escalate, conversation, stepIndex);
          metrics.llmCalls++;
          if (useVision) metrics.visionCalls++;
        }
      } else {
        const escalate = escalationModel && consecutiveFailures >= ESCALATION_THRESHOLD ? escalationModel : basePlannerModel;
        step = await planStep(cel, effectiveGoal, context, history, loopWarning, maxSteps, useVision, callbacks, deviceBaselineJson, escalate, conversation, stepIndex);
        metrics.llmCalls++;
        if (useVision) metrics.visionCalls++;
      }
    logger.debug("run-goal plan complete", {
      stepIndex,
      phase: "plan:done",
      action: step.action.type,
      payload: step.action,
    });
    } catch (planError) {
      // Record failed plan in conversation thread
      conversation?.addAssistantMessage(`{"type":"fail","reason":"planning error"}`);

      const errMsg = String(planError).slice(0, 120);
      history.push({
        step_index: stepIndex,
        action: { type: "fail", reason: `Plan failed: ${errMsg}` } as PlannedAction,
        success: false, error: errMsg,
      });
      callbacks.onStepExecuted?.({ reasoning: "Planning failed", action: { type: "fail", reason: errMsg } as PlannedAction, expected_outcome: "", confidence: 0 }, stepIndex, false, errMsg);
      metrics.errorCount++;
      // Don't count planner/API errors as consecutive action failures —
      // they're transient (network, rate limit, parse) and shouldn't
      // trigger the "too many failures" exit.
      cachedContext = null;
      await sleep(1000); // Longer backoff for API errors
      continue;
    }

    callbacks.onStepPlanned?.(step, stepIndex);

    // Record successful plan in conversation thread
    if (conversation && step) {
      const actionDesc = actionSignature(step.action);
      conversation.addAssistantMessage(JSON.stringify({ action: actionDesc, reasoning: step.reasoning?.slice(0, 100) }));
    }

    // Grounding validation
    let pageOrigin: string | null = null;
    if (callbacks.stateFingerprint) {
      try { pageOrigin = new URL(callbacks.stateFingerprint()).origin; } catch {}
    }
    const groundingError = validateGrounding(step, context, pageOrigin);
    if (groundingError) {
      history.push({ step_index: stepIndex, action: step.action, success: false, error: `Grounding: ${groundingError}` });
      callbacks.onStepExecuted?.(step, stepIndex, false, groundingError);
      metrics.errorCount++;
      consecutiveFailures++;
      cachedContext = null;

      // Tier-replan engagement on grounding failure: previously this `continue`
      // skipped the GATE phase entirely, meaning tier-replan never saw repeated
      // target-not-found errors no matter how many occurred. Trigger the shared
      // helper here so the LLM receives a replan signal on the next step.
      if (enableTierReplan && consecutiveFailures >= 3) {
        const outcome = await triggerReplan({
          reason: "reactive_failure", stepIndex,
          consecutiveFailures, currentMilestone,
          strategyTracker, loopDetector, checkpointManager, notebook, cognitiveTrail,
          historyAdvisor: HistoryAdvisor, cel, goal: config.goal,
          workflowName: config.workflowName,
          failureDetail: `Grounding: ${groundingError}`,
          metrics,
        });
        if (outcome.tier >= 2) {
          emitReplanEvent.tier({
            tier: outcome.tier,
            reason: "reactive_failure",
            step_index: stepIndex,
            milestone: currentMilestone,
            consecutive_failures: consecutiveFailures,
            failed_strategies_count: strategyTracker.getFailedStrategies(currentMilestone).length,
            backtracked: outcome.backtracked,
            needs_redecompose: outcome.needsRedecompose,
          });
          consecutiveFailures = 0;
          sameClickCount = 0;
          lastClickTarget = "";
          consecutiveScrolls = 0;
          consecutiveNotebookWrites = 0;
          if (outcome.loopWarning) loopWarning = outcome.loopWarning;
        }
      }

      await sleep(200);
      continue;
    }

    // Extract action — read-only data extraction.
    // Must still verify the goal — the agent may have extracted partial data
    // without completing the full task (e.g., multi-step goals that require navigation).
    if (step.action.type === "extract") {
      extractAttempts++;
      if (step.action.data) lastExtractData = step.action.data;

      // Pre-done verification: catch vague summaries, error pages, wrong domains
      const goalUrl = config.goal.match(/https?:\/\/[^\s"'<>]+/)?.[0];
      const extractVerification = verifyDone(step.action, context, config.goal, goalUrl);
      if (!extractVerification.verified) {
        loopWarning = `VERIFICATION FAILED: ${extractVerification.reason}`;
        history.push({ step_index: stepIndex, action: step.action, success: false, error: extractVerification.reason });
        callbacks.onStepExecuted?.(step, stepIndex, false, extractVerification.reason);
        metrics.errorCount++;
        consecutiveFailures++; // Trigger vision escalation after repeated failures
        continue;
      }
      if (callbacks.verifyGoal) {
        let verified = false;
        try { verified = await callbacks.verifyGoal(); } catch {}
        if (verified) {
          const result = makeResult("achieved", `Extracted: ${step.action.data}`, stepIndex);
          callbacks.onComplete?.(result);
          return result;
        }
        // Auto-promote: if we've extracted 3+ times and verifyGoal keeps failing,
        // accept the best extraction we have. Repeated extract attempts won't help
        // if the verify function is checking something unrelated to extraction quality.
        if (extractAttempts >= 3 && lastExtractData.length > 20) {
          const result = makeResult("achieved", `Extracted: ${lastExtractData}`, stepIndex);
          callbacks.onComplete?.(result);
          return result;
        }
        // Verification failed — the extract was premature (e.g., multi-step goal not complete).
        // Provide explicit feedback so the planner knows more steps are needed.
        loopWarning = "IMPORTANT: Data was extracted but the overall goal is NOT complete yet. Do NOT repeat the extraction — instead, perform the NEXT action in the goal (e.g., click a link, navigate to another page, or complete a different step). Re-read the original goal carefully.";
        history.push({ step_index: stepIndex, action: step.action, success: true, error: "Goal verification failed — more steps needed" });
        callbacks.onStepExecuted?.(step, stepIndex, true);
        continue;
      }
      // No verifyGoal callback — accept the extract as-is
      const result = makeResult("achieved", `Extracted: ${step.action.data}`, stepIndex);
      callbacks.onComplete?.(result);
      return result;
    }

    // Done validation
    if (step.action.type === "done") {
      doneAttempts++;
      if (step.action.summary) lastDoneSummary = step.action.summary;

      // Pre-done verification: catch vague summaries, error pages, wrong domains
      const doneGoalUrl = config.goal.match(/https?:\/\/[^\s"'<>]+/)?.[0];
      const doneVerification = verifyDone(step.action, context, config.goal, doneGoalUrl);
      if (!doneVerification.verified) {
        loopWarning = `VERIFICATION FAILED: ${doneVerification.reason}`;
        history.push({ step_index: stepIndex, action: step.action, success: false, error: doneVerification.reason });
        callbacks.onStepExecuted?.(step, stepIndex, false, doneVerification.reason);
        metrics.errorCount++;
        consecutiveFailures++;
        continue;
      }
      if (callbacks.verifyGoal) {
        let verified = false;
        try { verified = await callbacks.verifyGoal(); } catch {}
        if (!verified) {
          // Auto-accept after 3 failed verifyGoal attempts on "done" with real data
          if (doneAttempts >= 3 && lastDoneSummary.length > 30) {
            // The agent has tried 3+ times with data — accept it
          } else {
            history.push({ step_index: stepIndex, action: step.action, success: false, error: "Goal verification failed" });
            callbacks.onStepExecuted?.(step, stepIndex, false, "Goal verification failed");
            metrics.errorCount++;
            consecutiveFailures++;
            continue;
          }
        }
      }
      if (config.actCache) {
        const successActions = history.filter((h) => h.success).map((h) => h.action);
        await config.actCache.store(config.goal, initialFingerprint, variables, successActions).catch(() => {});
      }
      // Store learnings on success
      if (config.workflowName) {
        try {
          await HistoryAdvisor.storeOutcome(
            cel, config.goal, cognitiveTrail, notebook ?? new Notebook(),
            config.workflowName, true,
          );
        } catch { /* cel-store not available */ }
      }
      let summary = step.action.summary;
      if (notebook && !notebook.isEmpty) {
        summary += `\n\nDiscovered data:\n${notebook.toSummary()}`;
      }
      const result = makeResult("achieved", summary, stepIndex);
      callbacks.onComplete?.(result);
      return result;
    }

    if (step.action.type === "fail") {
      // Override false fail: if it's step 0 and page-text has data,
      // the LLM said "fail" but the data is right there. Extract it.
      const pageText = context.elements.find(e => e.id === "page-text");
      if (stepIndex === 0 && pageText?.value && pageText.value.length > 50) {
        const result = makeResult("achieved", `Extracted from page: ${pageText.value.slice(0, 500)}`, stepIndex);
        callbacks.onComplete?.(result);
        return result;
      }
      // Reject premature "fail" — if we haven't tried enough steps, the LLM is giving up
      // too early. Force it to try navigation/alternative approaches first.
      const genuineFailReasons = /not found|404|access denied|forbidden|blocked|login required|authentication|captcha/i;
      if (stepIndex < 5 && !genuineFailReasons.test(step.action.reason)) {
        const nudge = `You gave up after only ${stepIndex + 1} steps. Try navigating to the right page, scrolling, or clicking relevant links. Don't give up yet.`;
        history.push({ step_index: stepIndex, action: step.action, success: false, error: nudge });
        callbacks.onStepExecuted?.(step, stepIndex, false, nudge);
        loopWarning = nudge;
        cognitiveTrail.add(stepIndex, "REASSESS", `Premature fail rejected: "${step.action.reason}"`);
        continue;
      }
      const result = makeResult("failed", step.action.reason, stepIndex);
      callbacks.onComplete?.(result);
      return result;
    }

    // ── ACTION DEDUP GUARD ─────────────────────────────────────────────
    // Block exact duplicate actions (except scroll/wait which are valid to repeat).
    // This catches the LLM re-issuing the same click/fill before the loop detector
    // threshold (5 repeats) kicks in — preventing wasted steps.
    const actionSig = actionSignature(step.action);
    let isRepeatableAction = step.action.type === "scroll" || step.action.type === "wait"
      || step.action.type === "key" || step.action.type === "key_combo"
      || step.action.type === "activate_app";
    // Exempt tab clicks — tab navigation requires revisiting tabs
    if (!isRepeatableAction && step.action.type === "click") {
      const targetId = getActionTargetId(step.action);
      if (targetId) {
        const targetEl = context?.elements?.find(e => e.id === targetId);
        if (targetEl && /tab/i.test(targetEl.element_type || "")) {
          isRepeatableAction = true;
        }
      }
    }
    if (!isRepeatableAction) {
      const recentSigs = history.slice(-6).map(h => actionSignature(h.action));
      const dupCount = recentSigs.filter(s => s === actionSig).length;
      if (dupCount >= 2) {
        // Already tried this action twice recently — skip it and force re-plan
        history.push({ step_index: stepIndex, action: step.action, success: false, error: `Duplicate action skipped: ${actionSig}` });
        callbacks.onStepExecuted?.(step, stepIndex, false, "Duplicate action — try a different approach");
        loopWarning = `You already tried "${actionSig}" ${dupCount} times recently. Use a DIFFERENT action.`;
        cachedContext = null; // force fresh context
        continue;
      }
    }

    // ── FIX 1: Click-loop detection ────────────────────────────────────
    // Track consecutive clicks on the same target_id. If 3+ same-target clicks
    // happen without state changes, force an alternative approach.
    if (step.action.type === "click") {
      const targetId = getActionTargetId(step.action) ?? "";
      if (targetId === lastClickTarget) {
        sameClickCount++;
      } else {
        lastClickTarget = targetId;
        sameClickCount = 1;
      }
      if (sameClickCount >= 3) {
        loopWarning = `STUCK: You clicked "${targetId}" ${sameClickCount} times but the page didn't change. The click is NOT working — try a completely DIFFERENT approach: use "act" with natural language, scroll the element into view first, or use "navigate" to go directly to the URL.`;
        sameClickCount = 0; // reset to give new approach a chance
        cachedContext = null;
      }
    } else {
      // Non-click action resets the click tracker
      if (step.action.type !== "scroll" && step.action.type !== "wait") {
        sameClickCount = 0;
      }
    }

    // ── FIX 3: Notebook-loop detection ──────────────────────────────────
    // If the agent writes to notebook 5+ times without any real action,
    // inject a warning to take action instead of just recording data.
    if (step.action.type === "notebook_writes" || (step.action as any).type === "notebook_write") {
      consecutiveNotebookWrites++;
      if (consecutiveNotebookWrites >= 4) {
        loopWarning = "STUCK: You've written to the notebook " + consecutiveNotebookWrites + " times without taking any action. STOP recording and take an ACTION now: use 'click' to navigate, 'done' to finish with the data you have, or 'extract' to read the page.";
      }
    } else {
      consecutiveNotebookWrites = 0;
    }

    // ── Scroll-loop breaker ───────────────────────────────────────────
    // If the agent scrolls 3+ times, it's stuck. After 4 scrolls, OVERRIDE
    // the action: skip the scroll entirely and force a fresh context read
    // with page-text so the next planning step has all visible data.
    if (step.action.type === "scroll") {
      consecutiveScrolls++;
      if (consecutiveScrolls >= 4) {
        // Override: skip the scroll, force extract from page-text on next step
        loopWarning = "SCROLL OVERRIDE: Your scroll was BLOCKED because you scrolled " + consecutiveScrolls + " times. The data IS on the page. Look at the PAGE TEXT and element labels. Use 'done' with the data you see, or 'extract' to read. Scrolling is DISABLED.";
        cachedContext = null;
        history.push({ step_index: stepIndex, action: step.action, success: false, error: "Scroll blocked — too many consecutive scrolls" });
        callbacks.onStepExecuted?.(step, stepIndex, false, "Scroll blocked");
        consecutiveFailures++;
        continue; // Skip execution, go straight to next planning step
      } else if (consecutiveScrolls >= 3) {
        loopWarning = "WARNING: You have scrolled " + consecutiveScrolls + " times. The data is likely already visible. Use 'done' or 'extract' instead of scrolling again. Next scroll will be BLOCKED.";
        cachedContext = null;
      }
    } else {
      consecutiveScrolls = 0;
    }

    // ── PHASE 3: EXECUTE ──────────────────────────────────────────────
    profiler.mark("execute");
    logger.debug("run-goal phase", {
      stepIndex,
      phase: "execute:start",
      action: step.action.type,
    });

    // Element stability pre-check: if cortex is running and target is volatile,
    // wait briefly for it to stabilize before acting (prevents stale-element failures)
    if (config.cortex?.isRunning()) {
      const freshness = cortexBridge?.readFreshness() ?? (config.cortex as any).model?.freshness ?? null;
      if (freshness?.state === "hard-stale") {
        cachedContext = await callbacks.getContext();
        context = cachedContext;
        loopWarning = (loopWarning ?? "") + `\nSTATE REFRESH: hard-stale before execute (${freshness.causes.join(", ") || "unknown"}).`;
        console.info("[runtime-route] pre-execute refresh", {
          action: step.action.type,
          causes: freshness.causes,
        });
      }
      const targetId = getActionTargetId(step.action);
      const volatileSet = (config.cortex as any).model?.stability?.volatile;
      if (targetId && volatileSet?.has?.(targetId)) {
        let stabilized = false;
        for (let attempt = 0; attempt < 3; attempt++) {
          await new Promise(r => setTimeout(r, 150));
          const currentVolatile = (config.cortex as any).model?.stability?.volatile;
          if (!currentVolatile?.has?.(targetId)) {
            stabilized = true;
            break;
          }
        }
        if (!stabilized) {
          loopWarning = (loopWarning ?? "") + `\nWARNING: Target "${targetId}" is still animating.`;
        }
      }
    }

    // ── PRE-EXECUTE FOCUS/VALUE GUARDS ────────────────────────────────
    // Defensive checks that don't rely on the LLM noticing AXValue/AXFocused
    // fields in the serialized context. Catches two classes of bugs:
    //   1. Duplicate typing: planner re-issues `type X` when X is already in
    //      the field — result: field ends up with `XX` and action fails.
    //   2. Focus-drift keystrokes: planner issues `key Return` but keyboard
    //      focus has shifted to a different app/element — keystroke goes
    //      to the wrong place silently.
    {
      const a = step.action;
      const findEl = (id: string) => context.elements.find(e => e.id === id);

      // Skip redundant type: target already holds exactly the desired text.
      // Strict equality avoids false positives where the agent wants to append.
      if (a.type === "type" && "target_id" in a && a.target_id && typeof a.text === "string") {
        const target = findEl(a.target_id);
        if (target && target.value === a.text) {
          cognitiveTrail.add(stepIndex, "SKIP",
            `type skipped — ${a.target_id} already has value="${a.text.slice(0, 40)}"`);
          callbacks.onStepExecuted?.(step, stepIndex, true, undefined);
          // Advance without re-executing; leave cachedContext stale so the
          // next prepare re-reads — the field value is authoritative.
          continue;
        }
      }

      // Pre-key focus assertion: a key/key_combo without target_id goes to
      // whatever has keyboard focus. If nothing reports AXFocused, warn loudly
      // so the planner can re-focus or the loop-detector can catch thrashing.
      if ((a.type === "key" || a.type === "key_combo") && !("target_id" in a && (a as { target_id?: string }).target_id)) {
        const focusedEls = context.elements.filter(e => e.state?.focused);
        if (focusedEls.length === 0) {
          const keyDesc = a.type === "key" ? a.key : a.keys.join("+");
          loopWarning = (loopWarning ?? "") +
            `\nFOCUS WARNING: about to send ${a.type} "${keyDesc}" but no element reports AXFocused. ` +
            `Keystroke may land in the wrong app. If a target field should be focused, click into it first.`;
          cognitiveTrail.add(stepIndex, "FOCUS_WARN",
            `${a.type} "${keyDesc}" — no focused element in context`);
        }
      }
    }

    const preActionFP = callbacks.stateFingerprint?.();
    let success = false;
    let error: string | undefined;
    let executedAction: PlannedAction = step.action;
    const usedCallbackExecutor = !!callbacks.executeAction;

    // Per-step timeout with proper cleanup to prevent unhandled rejections.
    // The previous Promise.race() pattern left dangling rejected promises when
    // the action completed before the timeout fired.
    let stepTimeoutId: ReturnType<typeof setTimeout> | undefined;
    const stepTimeoutPromise = new Promise<never>((_, reject) => {
      stepTimeoutId = setTimeout(() => reject(new Error("Step timeout")), perStepTimeout);
    });

    // ── SUB-REGION VISION ZOOM: refine coordinates when vision was used
    // and the action targets raw coordinates (select, drag). This gives ~4x
    // pixel resolution by cropping a 200px region and re-asking the LLM.
    if (useVision && callbacks.screenshot) {
      const a = step.action;
      const isCoordAction = a.type === "select" || (a.type === "drag" && "from_x" in a);
      if (isCoordAction && "from_x" in a) {
        try {
          const screenshotBuf = (callbacks as any)._cachedScreenshot ?? await callbacks.screenshot();
          // Approximate screen dimensions from context or fallback
          const screenW = context.elements.reduce((max, el) =>
            el.bounds ? Math.max(max, el.bounds.x + el.bounds.width) : max, 1280);
          const screenH = context.elements.reduce((max, el) =>
            el.bounds ? Math.max(max, el.bounds.y + el.bounds.height) : max, 800);

          const refined = await refineWithZoom(
            a.from_x, a.from_y, screenshotBuf, screenW, screenH,
            `Refine click target for: ${step.reasoning}`,
            async (image, prompt) => {
              const base64 = image.toString("base64");
              return cel.llmCompleteWithImage("You are a precise UI element locator.", base64, prompt);
            },
          );
          if (refined.confidence !== "low") {
            (a as any).from_x = refined.x;
            (a as any).from_y = refined.y;
            cognitiveTrail.add(stepIndex, "NOTE",
              `Vision zoom refined from_xy to (${refined.x},${refined.y}) [${refined.confidence}]`);
          }
        } catch {
          // Refinement failed — proceed with original coordinates
        }
      }
    }

    try {
      // ── ADAPTER PATH: when executeAction callback is provided, ALL actions
      // route through the adapter. This is critical for browser benchmarks where
      // native macOS input (cel.keyCombo, cel.typeText) would type into the
      // user's desktop instead of the headless browser. ──
      if (callbacks.executeAction) {
        logger.debug("run-goal execute dispatch", {
          stepIndex,
          phase: "execute:dispatch",
          action: step.action.type,
          payload: step.action,
        });
        success = await Promise.race([
          callbacks.executeAction(step.action, context),
          stepTimeoutPromise,
        ]);
        clearTimeout(stepTimeoutId); // Cancel timeout on success
        logger.debug("run-goal execute complete", {
          stepIndex,
          phase: "execute:done",
          success,
        });
      }
      // ── NATIVE PATH: when no adapter callback, use CEL native input directly ──
      else if (step.action.type === "batch" && "actions" in step.action) {
        const batchActions = (step.action as { type: "batch"; actions: PlannedAction[] }).actions;
        for (let bi = 0; bi < batchActions.length; bi++) {
          const subAction = batchActions[bi];
          if (subAction.type === "click" && subAction.target_id) {
            const el = context.elements.find((e) => e.id === subAction.target_id);
            if (el?.bounds) {
              cel.click(el.bounds.x + Math.floor(el.bounds.width / 2), el.bounds.y + Math.floor(el.bounds.height / 2));
            }
          } else if (subAction.type === "key_combo") {
            cel.keyCombo(subAction.keys);
          } else if (subAction.type === "key") {
            cel.keyPress(subAction.key);
          } else if (subAction.type === "type" && (!("target_id" in subAction) || !subAction.target_id)) {
            cel.typeText(subAction.text);
          } else if (subAction.type === "type" && "target_id" in subAction && subAction.target_id) {
            // Type with target: click to focus, then type
            const el = context.elements.find((e) => e.id === subAction.target_id);
            if (el?.bounds) {
              cel.click(el.bounds.x + Math.floor(el.bounds.width / 2), el.bounds.y + Math.floor(el.bounds.height / 2));
              await sleep(100);
            }
            cel.typeText(subAction.text);
          } else if (subAction.type === "set_value") {
            cel.axSetValue(subAction.target_id, subAction.value);
          } else if (subAction.type === "ax_action") {
            cel.axPerformAction(subAction.target_id, subAction.action);
          } else if (subAction.type === "drag") {
            cel.drag(subAction.from_x, subAction.from_y, subAction.to_x, subAction.to_y);
          } else if (subAction.type === "activate_app") {
            (cel as any).activateApp?.(subAction.app_name);
          } else if (subAction.type === "wait") {
            await sleep(subAction.ms);
          } else if (subAction.type === "scroll") {
            cel.scroll(subAction.dx, subAction.dy);
          }
          if (bi < batchActions.length - 1) {
            // Shorter delays for non-UI-modifying actions in batch
            const needsSettleTime = subAction.type === "click" || subAction.type === "scroll"
              || subAction.type === "ax_action" || subAction.type === "drag";
            await sleep(needsSettleTime ? 200 : 50);
          }
        }
        success = true;
      } else if (step.action.type === "custom" && step.action.adapter === "browser" && step.action.action === "navigate") {
        // Browser navigate — use CDP if available, otherwise Cmd+L → type URL → Enter
        const url = (step.action.params as { url?: string })?.url;
        if (url) {
          try {
            await cel.cdpNavigate(url);
            success = true;
            await sleep(3000);
            cachedContext = null;

            // Auto-dismiss cookie/consent dialogs after navigation
            try {
              await cel.cdpEvaluate(`
                (function() {
                  const selectors = [
                    'button[id*="accept"]', 'button[id*="agree"]', 'button[id*="consent"]',
                    '[class*="accept"]', '[class*="agree"]', '[class*="consent"]',
                    'button[aria-label*="Accept"]', 'button[aria-label*="Agree"]',
                    'button', 'a[role="button"]'
                  ];
                  for (const sel of selectors.slice(0, -2)) {
                    const btn = document.querySelector(sel);
                    if (btn && btn.offsetParent !== null) { btn.click(); return 'clicked:' + sel; }
                  }
                  const buttons = document.querySelectorAll('button, a[role="button"], [role="button"]');
                  const acceptWords = ['accept', 'agree', 'ok', 'got it', 'i agree', 'consent', 'allow', 'συμφωνώ', 'αποδοχή'];
                  for (const btn of buttons) {
                    const text = (btn.textContent || '').toLowerCase().trim();
                    if (acceptWords.some(w => text.includes(w)) && btn.offsetParent !== null) {
                      btn.click();
                      return 'clicked:' + text;
                    }
                  }
                  const iframes = document.querySelectorAll('iframe[src*="consent"], iframe[src*="cookie"]');
                  for (const iframe of iframes) {
                    try {
                      const doc = iframe.contentDocument;
                      if (doc) {
                        const btns = doc.querySelectorAll('button');
                        for (const btn of btns) {
                          const t = (btn.textContent || '').toLowerCase();
                          if (acceptWords.some(w => t.includes(w))) { btn.click(); return 'iframe-clicked:' + t; }
                        }
                      }
                    } catch(e) { /* cross-origin iframe */ }
                  }
                  return 'no-consent-found';
                })()
              `);
            } catch {
              // Cookie dismissal failed — not critical, continue
            }
          } catch {
            // CDP not available — fallback to address bar
            cel.keyCombo(["Cmd", "L"]);
            await sleep(200);
            cel.typeText(url);
            await sleep(100);
            cel.keyPress("Enter");
            success = true;
            await sleep(2000);
            cachedContext = null;
          }
        }
      } else if (step.action.type === "activate_app") {
        // Direct app activation via open -a (most reliable macOS app switching)
        success = (cel as any).activateApp?.(step.action.app_name) ?? false;
        if (success) {
          await sleep(1000); // Wait for app to come to front
          cachedContext = null; // Force context refresh
        }
      } else if (step.action.type === "type") {
        // Smart type routing: use axSetValue for settable elements (more reliable)
        const typeAction = step.action;
        if (typeAction.target_id && typeAction.target_id !== "" && typeAction.target_id !== "0") {
          const targetEl = context.elements.find((el) => el.id === typeAction.target_id);
          if (targetEl?.properties?.settable === "true") {
            success = cel.axSetValue(typeAction.target_id, typeAction.text);
          } else {
            const wfStep: WorkflowStep = {
              id: `planned-${stepIndex}`,
              description: step.reasoning,
              action: plannedToWorkflowAction(step.action)!,
            };
            success = await executeAction(cel, wfStep, context, adapters);
          }
        } else {
          // No target — type directly into the currently focused element
          cel.typeText(typeAction.text);
          success = true;
        }
      } else {
        // Unified path: route all other actions through executeAction
        const workflowAction = plannedToWorkflowAction(step.action);
        if (!workflowAction) continue;
        const wfStep: WorkflowStep = {
          id: `planned-${stepIndex}`,
          description: step.reasoning,
          action: workflowAction,
        };
        success = await executeAction(cel, wfStep, context, adapters);
      }
      clearTimeout(stepTimeoutId); // Cancel timeout on success
      // Bump on non-throwing failure (executeAction returned false). The
      // success-path reset is done LATER, in the state-change detection block,
      // so that "successful action that made no progress" (semantic stall)
      // doesn't silently zero the tier-replan signal.
      if (!success) {
        consecutiveFailures++;
      }

      // Execute additional actions (multi-action output — up to 4 more)
      if (success && step.additional_actions && step.additional_actions.length > 0) {
        for (const extraAction of step.additional_actions) {
          try {
            if (callbacks.executeAction) {
              // Route through adapter (safe for headless browsers)
              await callbacks.executeAction(extraAction, context);
            } else {
              // Native CEL input (desktop automation)
              if (extraAction.type === "key_combo") cel.keyCombo(extraAction.keys);
              else if (extraAction.type === "key") cel.keyPress(extraAction.key);
              else if (extraAction.type === "type" && (!("target_id" in extraAction) || !extraAction.target_id)) cel.typeText(extraAction.text);
              else if (extraAction.type === "wait") await sleep(extraAction.ms);
              else if (extraAction.type === "scroll") cel.scroll(extraAction.dx, extraAction.dy);
              else if (extraAction.type === "click" && extraAction.target_id) {
                const el = context.elements.find((e) => e.id === extraAction.target_id);
                if (el?.bounds) {
                  cel.click(el.bounds.x + Math.floor(el.bounds.width / 2), el.bounds.y + Math.floor(el.bounds.height / 2));
                }
              }
            }
            const needsSettle = extraAction.type === "click" || extraAction.type === "scroll";
            await sleep(needsSettle ? 200 : 50);
          } catch (extraErr) {
            error = `Additional action ${extraAction.type} failed: ${String(extraErr)}`;
            break;
          }
        }
      }
    } catch (e) {
      clearTimeout(stepTimeoutId); // Always clear timeout in catch path
      error = String(e);
      metrics.errorCount++;

      // Per-step timeout — skip self-healing, just count as failure
      if (error.includes("Step timeout")) {
        consecutiveFailures++;
        callbacks.onStepExecuted?.(step, stepIndex, false, "Step timed out");
        continue;
      }

      // Self-healing
      if (selfHealEnabled) {
        const preHealFP = contextFingerprint(context);
        const healResult = await selfHeal(
          step.action, error, callbacks, cel, config.goal, history,
          {
            maxAttempts: selfHealMaxAttempts,
            enableVision,
            knowledgeStore: cel,
            workflowName: config.workflowName,
            originalContextFingerprint: preHealFP,
          },
        );
        if (healResult) {
          metrics.llmCalls++;
          let healSuccess = false;
          try {
            if (callbacks.executeAction) {
              healSuccess = await callbacks.executeAction(healResult.repairedAction, healResult.newContext);
            } else {
              const wfAction = plannedToWorkflowAction(healResult.repairedAction);
              if (wfAction) {
                healSuccess = await executeAction(cel, {
                  id: `healed-${stepIndex}`, description: `Self-healed`, action: wfAction,
                }, healResult.newContext, adapters);
              }
            }
          } catch { healSuccess = false; }

          if (healSuccess) {
            success = true; error = undefined;
            executedAction = healResult.repairedAction;
            context = healResult.newContext;
            cachedContext = healResult.newContext;
            consecutiveFailures = 0;
            metrics.selfHealSuccesses = (metrics.selfHealSuccesses ?? 0) + 1;

            // Record healing event in cognitive trail
            const failedDesc = describeAction(step.action);
            const repairedDesc = describeAction(healResult.repairedAction);
            const shiftTag = healResult.healingContext.contextShifted ? " [context shifted]" : "";
            cognitiveTrail.add(stepIndex, "HEAL",
              `"${failedDesc}" failed (${healResult.healingContext.failureReason}) → repaired to "${repairedDesc}"${shiftTag}`);

            // Track context shifts and trigger reassessment
            if (healResult.healingContext.contextShifted) {
              metrics.healContextShifts = (metrics.healContextShifts ?? 0) + 1;
              loopWarning = (loopWarning ?? "") +
                "\nCONTEXT SHIFT: The previous action required self-healing because the screen changed. " +
                "Re-evaluate your current approach before continuing.";
              if (currentMilestone) {
                cognitiveTrail.add(stepIndex, "REASSESS",
                  `Context shifted during heal at milestone "${currentMilestone}" — planner should reassess`);
              }
            }
          } else {
            consecutiveFailures++;
            metrics.selfHealFailures = (metrics.selfHealFailures ?? 0) + 1;
            cognitiveTrail.add(stepIndex, "ACT_FAIL",
              `Heal attempted but repair also failed: ${describeAction(healResult.repairedAction)}`);
          }
        } else {
          consecutiveFailures++;
          metrics.selfHealFailures = (metrics.selfHealFailures ?? 0) + 1;
        }
      } else {
        consecutiveFailures++;
      }
    }

    callbacks.onStepExecuted?.(step, stepIndex, success, error);
    if (cortexBridge && !usedCallbackExecutor) {
      cortexBridge.ingestActionOutcome({
        action: executedAction.type,
        route: "structured",
        success,
        verified: success,
        contradiction: false,
        sideEffectSummary: error,
      });
    }

    // ── POST-ACTION: Cortex interrupt handling ─────────────────────────
    // After action execution, check for and dismiss any dialogs/overlays
    // that may have appeared (e.g., cookie consent after navigation).
    if (cortexBridge && success) {
      try {
        const handled = await cortexBridge.handleInterrupts();
        for (const desc of handled) {
          cognitiveTrail.add(stepIndex, "INTERRUPT", `Post-action: ${desc}`);
        }
      } catch { /* best effort */ }
    }

    // Adapter-executed actions can change page state without tripping the
    // lightweight fingerprint gate. Refresh the cached snapshot here so the
    // next planning step sees the post-action UI instead of the pre-action one.
    if (success && !isBlindMode && usedCallbackExecutor) {
      try {
        cachedContext = await callbacks.getContext();
      } catch {
        cachedContext = null;
      }
    }

    // ── PHASE 4: VALIDATE ─────────────────────────────────────────────
    lastActionWasTransition = isTransitionAction(executedAction);

    // Post-action validation: dedicated validator or inline fallback.
    // Validator runs heuristic + diff checks (no LLM call), so it's cheap
    // to run on every step. Only cost is a getContext() call which is usually cached.
    if (success && !isBlindMode && config.validator) {
      const postFP = callbacks.stateFingerprint?.();
      // Force fresh context — cachedContext is still the pre-action snapshot
      const postCtx = await callbacks.getContext();
      // Cache validation context for reuse in next iteration's Phase 1.
      // The fingerprint check at Phase 1 will detect if it's stale.
      cachedContext = postCtx;
      const validationResult = await validateAction(
        {
          goal: config.goal, step, preContext: previousContext ?? context,
          postContext: postCtx, preFingerprint: preActionFP, postFingerprint: postFP,
          pageOrigin, executionError: error,
        },
        config.validator,
      );
      callbacks.onValidation?.(validationResult, stepIndex);
      if (validationResult.verdict === "failure" && !error) {
        error = validationResult.reasoning;
        if (cortexBridge && !usedCallbackExecutor) {
          cortexBridge.ingestActionOutcome({
            action: executedAction.type,
            route: "structured",
            success: false,
            verified: false,
            contradiction: true,
            sideEffectSummary: validationResult.reasoning,
          });
        }
        // cachedContext stays as postCtx — fingerprint gate handles staleness
      }
    } else if (success && !isBlindMode && isTransitionAction(executedAction)) {
      // Original inline validation (backward compat when no validator config)
      const postFP = callbacks.stateFingerprint?.();
      const validationError = validatePostAction(preActionFP, postFP);
      if (validationError && !error) {
        error = validationError;
        if (cortexBridge && !usedCallbackExecutor) {
          cortexBridge.ingestActionOutcome({
            action: executedAction.type,
            route: "structured",
            success: false,
            verified: false,
            contradiction: true,
            sideEffectSummary: validationError,
          });
        }
        // fingerprint gate in Phase 1 handles staleness
      }
    }

    // Fast-path completion: some tasks become complete immediately after a click/type
    // even if the post-action context is still noisy. If the caller can verify the
    // goal directly, stop here instead of forcing the planner to rediscover success.
    if (success && callbacks.verifyGoal) {
      let verified = false;
      try { verified = await callbacks.verifyGoal(); } catch {}
      if (verified) {
        error = undefined;
        callbacks.onStepExecuted?.(step, stepIndex, true);
        const result = makeResult(
          "achieved",
          step.expected_outcome || `Goal verified after ${describeAction(executedAction)}`,
          stepIndex,
        );
        callbacks.onComplete?.(result);
        return result;
      }
    }

    // Settle + speculative parallel planning
    // While waiting for UI to settle, start planning the next step in background
    if (success && isTransitionAction(step.action)) {
      const settlePromise = callbacks.waitForSettle
        ? callbacks.waitForSettle(step.action.type)
        : sleep(DEFAULT_SETTLE_MS[step.action.type] ?? stepDelay);

      // Speculative planning: plan next step in parallel with settle wait.
      // Widened speculative planning: also speculate for predictable actions
      // (type, key, key_combo, wait) where context rarely changes meaningfully.
      const actionType = step.action.type as string;
      const isPredictableAction = actionType === "type" || actionType === "key"
        || actionType === "key_combo" || actionType === "wait" || actionType === "scroll";
      const canSpeculate = tentativePlan.length === 0
        && actionType !== "done" && actionType !== "fail"
        && !isBlindMode
        && (
          (contextLazy && step.context_tier === "none")  // original condition
          || isPredictableAction                          // new: predictable actions
          || (step.confidence >= 0.8 && consecutiveFailures === 0) // new: high confidence + no failures
        );
      if (canSpeculate) {
        // Store the speculative plan promise — resolve it in Phase 2 of the next
        // iteration instead of racing against a 100ms timeout. This way the LLM
        // call runs in the background through settle + context extraction, and
        // we only await it when we'd need an LLM call anyway.
        pendingSpeculativePlan = planStep(
          cel, effectiveGoal, context, history, loopWarning, maxSteps, false, callbacks, deviceBaselineJson
        ).catch(() => null);
      } else {
        pendingSpeculativePlan = null;
      }

      await settlePromise;

      // Pre-fetch context for next iteration if action was predictable
      if (isPredictableAction && callbacks.getContext) {
        preemptiveContextPromise = callbacks.getContext();
      }
    }

    // State change detection + no-progress tracking
    if (success) {
      if (callbacks.stateFingerprint && preActionFP !== undefined) {
        const postActionFP = callbacks.stateFingerprint();
        if (postActionFP !== preActionFP) {
          tentativePlan = [];
          pendingSpeculativePlan = null;
          preemptiveContextPromise = null;
          cachedContext = null;
          lastStateFingerprint = postActionFP;
          metrics.stateChanges++;
          // Reset no-progress counter on real state change
          stepsSinceProgress = 0;
          lastProgressFingerprint = postActionFP;
          // Real progress — clear the failure counter so the tier system
          // doesn't fire from stale accumulated failures.
          consecutiveFailures = 0;
        } else {
          stepsSinceProgress++;
          // NOTE: we deliberately do NOT reset consecutiveFailures here. A
          // "successful" action that produced no observable state change is
          // exactly the signal the semantic-stall check needs — zeroing it
          // would defeat tier-replan escalation on stalls.
        }
      } else {
        // No fingerprint available — assume progress (legacy adapters)
        stepsSinceProgress = 0;
        consecutiveFailures = 0;
      }
      if (isTransitionAction(step.action)) {
        cachedContext = null;
      }
    }

    // Record history — resolve element label for richer planner context
    const targetId = getActionTargetId(executedAction);
    const element_label = targetId && context
      ? context.elements.find(e => e.id === targetId)?.label || undefined
      : undefined;
    history.push({ step_index: stepIndex, action: executedAction, success, error, element_label });

    // Message compaction
    compactHistoryIfNeeded(history, stepIndex);

    // ── REFLECT: cognitive trail + notebook + assessment ──────────────

    // 1. Record thinking in cognitive trail
    if (step.thinking) {
      cognitiveTrail.add(stepIndex, "THINK", step.thinking);
    }

    // 2. Record action result
    const actionDesc = actionSignature(executedAction);
    cognitiveTrail.add(stepIndex, success ? "ACT_OK" : "ACT_FAIL",
      success ? actionDesc : `${actionDesc} — ${error ?? "failed"}`);

    // 3. Process notebook writes
    if (notebook && step.notebook_writes) {
      for (const nw of step.notebook_writes) {
        notebook.write(nw.key, nw.value, `step-${stepIndex}`, nw.category as "data" | "url" | "observation" | "error");
        cognitiveTrail.add(stepIndex, "NOTE", `${nw.key}: ${nw.value}`);
      }
    }

    // 4. Process progress assessment
    if (step.progress) {
      const progress = step.progress;

      if (progress.startsWith("milestone:")) {
        // Milestone reached — capture checkpoint
        const milestoneLabel = progress.slice("milestone:".length);
        currentMilestone = milestoneLabel;
        cognitiveTrail.add(stepIndex, "MILESTONE", milestoneLabel);
        const fp = callbacks.stateFingerprint?.() ?? String(contextFingerprint(context));
        checkpointManager.capture(
          milestoneLabel, stepIndex, fp,
          callbacks.stateFingerprint?.() ?? null,
          `${context.app ?? ""} — ${context.window ?? ""}`,
          notebook?.snapshot() ?? [],
        );
      } else if (progress === "wrong_approach") {
        // Proactive reassessment — trigger Tier 2+ replan via shared helper.
        // Flag-gated so operators can roll out gradually.
        if (!enableTierReplan) {
          cognitiveTrail.add(stepIndex, "REASSESS", "LLM flagged wrong_approach — tier-replan disabled, proceeding");
        } else {
        cognitiveTrail.add(stepIndex, "REASSESS", "LLM flagged wrong_approach — triggering replan");
        const outcome = await triggerReplan({
          reason: "wrong_approach", stepIndex, consecutiveFailures, currentMilestone,
          strategyTracker, loopDetector, checkpointManager, notebook, cognitiveTrail,
          historyAdvisor: HistoryAdvisor, cel, goal: config.goal,
          workflowName: config.workflowName, metrics,
        });
        if (outcome.tier >= 2) {
          emitReplanEvent.tier({
            tier: outcome.tier,
            reason: "wrong_approach",
            step_index: stepIndex,
            milestone: currentMilestone,
            consecutive_failures: consecutiveFailures,
            failed_strategies_count: strategyTracker.getFailedStrategies(currentMilestone).length,
            backtracked: outcome.backtracked,
            needs_redecompose: outcome.needsRedecompose,
          });
          // Reset ephemeral counters (anti-loop state leaks if left alone)
          consecutiveFailures = 0;
          sameClickCount = 0;
          lastClickTarget = "";
          consecutiveScrolls = 0;
          consecutiveNotebookWrites = 0;
          if (outcome.loopWarning) loopWarning = outcome.loopWarning;
          cachedContext = null; // Force fresh context

          if (outcome.needsRedecompose && enableTier4Reassessment) {
            tier4Count++;
            if (tier4Count > 1) {
              // Already re-decomposed once and still failing — abort to avoid
              // burning every remaining step on futile LLM re-decomposition.
              const result = makeResult(
                "failed",
                `Strategy exhausted: Tier 4 re-assessment did not recover after ${tier4Count} attempts`,
                stepIndex,
              );
              callbacks.onComplete?.(result);
              return result;
            }
            await rerunMilestoneDecomposition({
              cel, callbacks, config, maxSteps, historyAdvice,
              cognitiveTrail, strategyTracker,
              setMilestone: (m: string) => { currentMilestone = m; },
              setMilestonesContext: (s: string) => { milestonesContext = s; },
            });
            // Force re-injection of the new milestones on the next plan step
            // (the step-0 gate on milestonesContext would otherwise skip them).
            milestonesInjectedFor = null;
          }
        }
        } // end enableTierReplan (proactive)
      } else if (progress === "stalled") {
        // Mild nudge — not a replan, just awareness
        if (!loopWarning) {
          loopWarning = "Progress appears stalled. Consider a different approach.";
        }
      }
    }

    // 5. Update batch_next for next iteration
    batchNextRequested = step.batch_next ?? false;

    // Update context tier
    if (contextLazy) {
      requestedContextTier = !success ? "full" : (step.context_tier ?? "full");
    }

    profiler.mark("gate");
    // ── GATE: loop detection → failure escalation → replan decision ─────
    // Loop detection runs FIRST so its failure increment feeds into the tier
    // decision below (spec: loop state is part of the GATE, not REFLECT).
    const ctxHash = contextFingerprint(context);
    const signal = loopDetector.check(executedAction, ctxHash);
    if (signal.type !== "none") {
      loopWarning = loopDetector.getWarning(signal);
      metrics.loopWarnings++;
      loopDetector.startGrace();
      // Treat sustained loops as consecutive failures — even though the action
      // "succeeded", nothing changed. This activates the failure nudges which
      // escalate to "try completely different approach" and eventually "done/fail".
      consecutiveFailures++;
      // Force context refresh so the planner doesn't see stale state
      cachedContext = null;
    } else {
      loopWarning = null;
      loopDetector.resetGrace();
    }

    // ── GATE: semantic stall escalation ─────────────────────────────────
    // The hash-based loop detector misses cases where the LLM alternates
    // action types (e.g. navigate ↔ key_combo+type) that all target the same
    // URL — each action hash is different so no "repeat" fires. When actions
    // keep succeeding but state isn't advancing and verifyGoal still fails,
    // bump consecutiveFailures so tier-replan can engage. Without this the
    // runner burns its entire step budget on confident but useless actions.
    // Flag-gated: requires both enableTierReplan AND enableSemanticStallEscalation.
    let stallTriggeredThisStep = false;
    if (enableTierReplan && enableSemanticStall && success && stepsSinceProgress >= 3) {
      let verifyGoalFailing = true;
      if (callbacks.verifyGoal) {
        // Cache verifyGoal result to avoid calling it every stall step —
        // fingerprint-keyed so we re-check only when the page actually changes.
        const fp = callbacks.stateFingerprint?.() ?? `${stepIndex}`;
        if (lastVerifyGoalFp !== fp) {
          try { lastVerifyGoalResult = !!(await callbacks.verifyGoal()); }
          catch { lastVerifyGoalResult = false; }
          lastVerifyGoalFp = fp;
        }
        verifyGoalFailing = !lastVerifyGoalResult;
      }
      if (verifyGoalFailing) {
        consecutiveFailures++;
        stallTriggeredThisStep = true;
        cognitiveTrail.add(stepIndex, "ACT_FAIL",
          `Semantic stall: ${stepsSinceProgress} successful steps without state change or goal verification — counting as failure for tier escalation`);
        emitReplanEvent.stall({ step_index: stepIndex, steps_since_progress: stepsSinceProgress });
      }
    }

    // GATE: reactive replan on consecutive failures (Tier 2+) — shared helper.
    // Runs AFTER loop detection so a loop-induced failure can trigger replan
    // in the same step rather than waiting for the next iteration.
    // Flag-gated: requires enableTierReplan. Tier 4 requires enableTier4Reassessment.
    // Triggers on either action failure OR semantic stall (where success=true
    // but no progress is being made) — both are signals that the current
    // strategy is dead.
    if (enableTierReplan && (!success || stallTriggeredThisStep) && consecutiveFailures >= 3) {
      const outcome = await triggerReplan({
        reason: "reactive_failure", stepIndex, consecutiveFailures, currentMilestone,
        strategyTracker, loopDetector, checkpointManager, notebook, cognitiveTrail,
        historyAdvisor: HistoryAdvisor, cel, goal: config.goal,
        workflowName: config.workflowName, failureDetail: error, metrics,
      });
      if (outcome.tier >= 2) {
        emitReplanEvent.tier({
          tier: outcome.tier,
          reason: "reactive_failure",
          step_index: stepIndex,
          milestone: currentMilestone,
          consecutive_failures: consecutiveFailures,
          failed_strategies_count: strategyTracker.getFailedStrategies(currentMilestone).length,
          backtracked: outcome.backtracked,
          needs_redecompose: outcome.needsRedecompose,
        });
        // Reset ephemeral counters (anti-loop state leaks if left alone)
        consecutiveFailures = 0;
        sameClickCount = 0;
        lastClickTarget = "";
        consecutiveScrolls = 0;
        consecutiveNotebookWrites = 0;
        if (outcome.loopWarning) loopWarning = outcome.loopWarning;
        cachedContext = null;

        if (outcome.needsRedecompose && enableTier4Reassessment) {
          tier4Count++;
          if (tier4Count > 1) {
            emitReplanEvent.tier4Cap({ step_index: stepIndex, attempts: tier4Count });
            const result = makeResult(
              "failed",
              `Strategy exhausted: Tier 4 re-assessment did not recover after ${tier4Count} attempts`,
              stepIndex,
            );
            callbacks.onComplete?.(result);
            return result;
          }
          await rerunMilestoneDecomposition({
            cel, callbacks, config, maxSteps, historyAdvice,
            cognitiveTrail, strategyTracker,
            setMilestone: (m: string) => { currentMilestone = m; },
            setMilestonesContext: (s: string) => { milestonesContext = s; },
          });
          milestonesInjectedFor = null;
        }
      }
    }

    // Increment no-progress counter on failed steps too
    if (!success) stepsSinceProgress++;

    if (!success && stepDelay > 0) await sleep(stepDelay);

    profiler.end();
  }

  // ── POST-EXECUTION: store learnings ──────────────────────────────────
  if (config.workflowName) {
    try {
      await HistoryAdvisor.storeOutcome(
        cel, config.goal, cognitiveTrail, notebook ?? new Notebook(),
        config.workflowName, false,
      );
    } catch { /* cel-store not available */ }
  }

  const result = makeResult("max_steps", `Exceeded ${maxSteps} steps without achieving goal`, maxSteps);
  // Append notebook and trail to result summary
  if (notebook && !notebook.isEmpty) {
    result.summary += `\n\nDiscovered data:\n${notebook.toSummary()}`;
  }
  callbacks.onComplete?.(result);
  return result;
}

// ─── Cache replay ─────────────────────────────────────────────────────────────

async function replayCachedActions(
  cacheKey: string,
  actions: PlannedAction[],
  cel: Cel,
  callbacks: GoalRunnerCallbacks,
  config: GoalRunnerConfig,
  adapters?: AdapterRegistry,
): Promise<GoalResult | null> {
  const startTime = Date.now();
  const selfHealEnabled = config.selfHeal ?? true;

  for (let i = 0; i < actions.length; i++) {
    const action = actions[i];
    if (action.type === "done" || action.type === "fail") continue;

    const context = await callbacks.getContext();
    let success = false;

    try {
      if (callbacks.executeAction) {
        success = await callbacks.executeAction(action, context);
      } else {
        const wfAction = plannedToWorkflowAction(action);
        if (!wfAction) continue;
        success = await executeAction(cel, {
          id: `cached-${i}`, description: `Replaying ${i + 1}/${actions.length}`, action: wfAction,
        }, context, adapters);
      }
    } catch (e) {
      if (selfHealEnabled) {
        const preHealFP = contextFingerprint(context);
        const healResult = await selfHeal(action, String(e), callbacks, cel, config.goal, [], {
          maxAttempts: config.selfHealMaxAttempts ?? 2,
          knowledgeStore: cel,
          workflowName: config.workflowName,
          originalContextFingerprint: preHealFP,
        });
        if (healResult) {
          try {
            if (callbacks.executeAction) {
              success = await callbacks.executeAction(healResult.repairedAction, healResult.newContext);
            } else {
              const wfAction = plannedToWorkflowAction(healResult.repairedAction);
              if (wfAction) {
                success = await executeAction(cel, {
                  id: `cache-healed-${i}`, description: `Self-healed ${i + 1}`, action: wfAction,
                }, healResult.newContext, adapters);
              }
            }
            if (success) {
              await config.actCache?.repair(cacheKey, i, healResult.repairedAction, config.variables ?? {}).catch(() => {});
            }
          } catch { success = false; }
        }
      }
      if (!success) return null;
    }

    if (!success) return null;

    if (callbacks.waitForSettle) {
      await callbacks.waitForSettle(action.type);
    } else {
      const settleMs = DEFAULT_SETTLE_MS[action.type] ?? 500;
      if (settleMs > 0) await sleep(settleMs);
    }
  }

  return {
    status: "achieved",
    summary: "Goal achieved (replayed from cache)",
    totalSteps: actions.length,
    history: actions.map((a, i) => ({ step_index: i, action: a, success: true })),
    metrics: {
      totalMs: Date.now() - startTime, contextExtractionMs: 0, llmCalls: 0,
      visionCalls: 0, errorCount: 0, stateChanges: 0, loopWarnings: 0,
      cacheHits: actions.length,
    },
  };
}
