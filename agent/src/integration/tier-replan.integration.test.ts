/**
 * Tier-replan integration tests.
 *
 * Drives `runGoal()` end-to-end with a mocked `Cel` and scripted callbacks
 * so we can force specific failure signatures (wrong_approach, consecutive
 * action failures, semantic stalls) and assert the tier-replan system
 * actually activates — something the bench tasks haven't exercised yet.
 *
 * Mock surface is intentionally minimal: only the methods runGoal hits on
 * the happy-path + tier-replan paths. New runGoal code paths should extend
 * these mocks rather than mock the full Cel class.
 */
import { describe, it, expect } from "vitest";

// Integration tests drive the real runGoal — give them more time than the 5s default.
const TEST_TIMEOUT = 30_000;
import { runGoal } from "../goal-runner.js";
import type {
  GoalRunnerConfig,
  GoalRunnerCallbacks,
} from "../goal-runner/config.js";
import type {
  ScreenContext,
  PlannedStep,
  PlannedAction,
  PlannerStepRecord,
} from "../types.js";

// ─── Mock Cel ────────────────────────────────────────────────────────────────

interface ScriptedStep {
  step: PlannedStep;
  /** Result of executeAction for this step (default: true). */
  executionSuccess?: boolean;
  /** Error string when executionSuccess=false. */
  error?: string;
}

function emptyContext(): ScreenContext {
  // Non-empty elements avoid the 1.5s "context retry" path in goal-runner.
  // Provide several distinct targets so the duplicate-action detector
  // (2x same signature in last 6 entries → skip) doesn't intercept.
  const makeBtn = (id: string, label: string) => ({
    id,
    element_type: "button" as const,
    label,
    state: { visible: true, focused: false, enabled: true },
  });
  return {
    app: "TestApp",
    window: "TestWindow",
    elements: [
      makeBtn("btn-a", "Button A"),
      makeBtn("btn-b", "Button B"),
      makeBtn("btn-c", "Button C"),
      makeBtn("btn-d", "Button D"),
      makeBtn("btn-e", "Button E"),
      makeBtn("btn-f", "Button F"),
      makeBtn("btn-g", "Button G"),
    ] as never[],
    timestamp_ms: Date.now(),
  };
}

/**
 * Minimal Cel stub covering just the methods runGoal hits on the tier-replan
 * paths. Stage a PlannedStep script, optionally stub llmComplete / searchKnowledge.
 */
function makeCel(script: ScriptedStep[], trace?: string[]) {
  let stepCursor = 0;
  return {
    // Planner
    planStep: async (
      _goal: string,
      _context: ScreenContext,
      _history: PlannerStepRecord[],
    ): Promise<PlannedStep> => {
      const entry = script[Math.min(stepCursor, script.length - 1)];
      stepCursor++;
      if (trace) trace.push(`plan[${stepCursor - 1}]:${entry.step.action.type}`);
      return entry.step;
    },
    planStepWithVision: async (): Promise<PlannedStep> => {
      const entry = script[Math.min(stepCursor, script.length - 1)];
      stepCursor++;
      if (trace) trace.push(`planV[${stepCursor - 1}]:${entry.step.action.type}`);
      return entry.step;
    },
    buildPlanPrompt: () => ({ system: "sys", user: "usr", index_map: [] }),
    llmComplete: async (prompt: string): Promise<string> => {
      // Pre-flight uses llmComplete for feasibility + decomposition.
      // Return null-ish results so pre-flight is a no-op and we focus on the loop.
      if (prompt.includes("feasible")) return '{"feasible": true, "reason": "ok"}';
      if (prompt.includes("Decompose")) return '{"milestones": []}';
      return "{}";
    },
    llmCompleteWithMessages: async () => "{}",
    llmCompleteWithImage: async () => "{}",
    // KnowledgeStore
    searchKnowledge: () => [],
    addScopedKnowledge: () => {},
    storeObservation: () => {},
    getWorkingMemory: () => ({}),
    setWorkingMemory: () => {},
    // Input (action execution usually goes through callbacks.executeAction instead)
    keyPress: () => {},
    keyCombo: () => {},
    typeText: () => {},
    // CDP stubs (not used by integration tests)
    discoverCdpTargets: () => [],
    cdpNavigate: async () => {},
    cdpEvaluate: async () => null,
    // Display
    getContext: () => emptyContext(),
    getQuickContext: () => emptyContext(),
    // Misc
    isNativeAvailable: false,
    get stepCursor() { return stepCursor; },
  };
}

// Force cast to `any` at the test boundary — the production Cel is much
// larger than what runGoal uses, and maintaining a full stub would be
// net-negative. TypeScript will still check the methods we actually call.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type CelLike = any;

/** Scripted-context callbacks that never change state. */
function makeCallbacks(opts: {
  executeOk?: boolean;
  stateChanges?: boolean;
  verifyGoal?: () => Promise<boolean>;
  trace?: string[];
} = {}): GoalRunnerCallbacks {
  const { executeOk = true, stateChanges = false, verifyGoal, trace } = opts;
  let tick = 0;
  return {
    getContext: async () => emptyContext(),
    stateFingerprint: () => (stateChanges ? `fp-${tick++}` : "fp-static"),
    executeAction: async (action: PlannedAction) => {
      if (trace) trace.push(`exec:${action.type}->${executeOk}`);
      return executeOk;
    },
    waitForSettle: async () => {},
    verifyGoal,
    onStepExecuted: trace
      ? (step, i, success, err) => {
          trace.push(`step${i}:${step.action.type}:${success}${err ? `:${err.slice(0, 30)}` : ""}`);
        }
      : undefined,
  };
}

function baseConfig(overrides: Partial<GoalRunnerConfig> = {}): GoalRunnerConfig {
  return {
    goal: "test goal",
    maxSteps: 10,
    stepDelay: 0,
    taskTimeout: 10_000,
    stepTimeout: 1000,
    maxConsecutiveFailures: 20, // don't let the built-in cap terminate before tier-replan fires
    enableVision: false,
    skipRouter: true,
    persistentThread: false,
    // Flags default off — each test opts in to what it needs.
    ...overrides,
  };
}

// ─── Action helpers ──────────────────────────────────────────────────────────

// Use distinct target_ids so the goal-runner's duplicate-action detection
// (skips-and-continues on 2x same signature in last 6 history entries)
// doesn't bypass the reactive replan gate. Distinct signatures force each
// failure through the full GATE phase.
function clickOn(target: string): PlannedAction {
  return { type: "click", target_id: target };
}
const DONE_ACTION: PlannedAction = { type: "done", summary: "finished test run with all expected data extracted as a structured list" };

function planned(action: PlannedAction, opts: Partial<PlannedStep> = {}): PlannedStep {
  return {
    reasoning: "test",
    action,
    expected_outcome: "test outcome",
    confidence: 0.9,
    ...opts,
  };
}

// ─── Tests ───────────────────────────────────────────────────────────────────

describe("runGoal integration — tier-replan paths", () => {
  it("reactive failure path: consecutive action failures trigger Tier 2 replan when flag is on", async () => {
    // Distinct target_ids so duplicate-action detection doesn't intercept
    const script: ScriptedStep[] = [
      { step: planned(clickOn("btn-a")) },
      { step: planned(clickOn("btn-b")) },
      { step: planned(clickOn("btn-c")) },
      { step: planned(clickOn("btn-d")) },
      { step: planned(clickOn("btn-e")) },
      { step: planned(DONE_ACTION) },
    ];
    const trace: string[] = [];
    const cel = makeCel(script, trace);
    const result = await runGoal(cel as CelLike, baseConfig({
      enableTierReplan: true,
      selfHeal: false,
    }), makeCallbacks({ executeOk: false, trace }));
    // eslint-disable-next-line no-console
    if ((result.metrics?.tier2Replans ?? 0) === 0) console.log("TRACE:", trace, "METRICS:", result.metrics);
    expect(result.metrics?.tier2Replans ?? 0).toBeGreaterThanOrEqual(1);
  }, TEST_TIMEOUT);

  it("flags-off mode: behavior is preserved (no tier-replan activity)", async () => {
    const script: ScriptedStep[] = [
      { step: planned(clickOn("btn-a")) },
      { step: planned(clickOn("btn-b")) },
      { step: planned(clickOn("btn-c")) },
      { step: planned(clickOn("btn-d")) },
      { step: planned(clickOn("btn-e")) },
    ];
    const cel = makeCel(script);
    const result = await runGoal(cel as CelLike, baseConfig({
      enableTierReplan: false,
      selfHeal: false,
    }), makeCallbacks({ executeOk: false }));
    expect(result.metrics?.tier2Replans ?? 0).toBe(0);
    expect(result.metrics?.tier3Backtracks ?? 0).toBe(0);
    expect(result.metrics?.tier4Reassessments ?? 0).toBe(0);
  }, TEST_TIMEOUT);

  it("proactive replan: LLM emitting progress=wrong_approach triggers tier-replan when flag is on", async () => {
    const script: ScriptedStep[] = [
      { step: planned(clickOn("btn-a"), { progress: "wrong_approach" }) },
      { step: planned(clickOn("btn-b"), { progress: "wrong_approach" }) },
      { step: planned(clickOn("btn-c"), { progress: "wrong_approach" }) },
      { step: planned(clickOn("btn-d"), { progress: "wrong_approach" }) },
      { step: planned(DONE_ACTION) },
    ];
    const cel = makeCel(script);
    const result = await runGoal(cel as CelLike, baseConfig({
      enableTierReplan: true,
      maxConsecutiveFailures: 30,
      selfHeal: false,
    }), makeCallbacks({ executeOk: true, stateChanges: true }));
    expect(result.metrics?.tier2Replans ?? 0).toBeGreaterThanOrEqual(1);
  }, TEST_TIMEOUT);

  it("semantic stall: actions succeed but state never changes and verifyGoal fails → tier-replan activates", async () => {
    const script: ScriptedStep[] = [
      { step: planned(clickOn("btn-a")) },
      { step: planned(clickOn("btn-b")) },
      { step: planned(clickOn("btn-c")) },
      { step: planned(clickOn("btn-d")) },
      { step: planned(clickOn("btn-x")), executionSuccess: true },
      { step: planned(clickOn("btn-e")) },
      { step: planned(clickOn("btn-f")) },
      { step: planned(clickOn("btn-g")) },
      { step: planned(DONE_ACTION) },
    ];
    const trace: string[] = [];
    const cel = makeCel(script, trace);
    const result = await runGoal(cel as CelLike, baseConfig({
      enableTierReplan: true,
      enableSemanticStallEscalation: true,
      maxConsecutiveFailures: 30,
      maxStepsWithoutProgress: 30,
      selfHeal: false,
    }), makeCallbacks({
      executeOk: true,
      stateChanges: false,
      verifyGoal: async () => false,
      trace,
    }));
    // eslint-disable-next-line no-console
    if ((result.metrics?.tier2Replans ?? 0) === 0) console.log("STALL TRACE:", trace, "METRICS:", result.metrics);
    // Stall detection should escalate to tier-replan
    expect(result.metrics?.tier2Replans ?? 0).toBeGreaterThanOrEqual(1);
  }, TEST_TIMEOUT);

  it("grounding-fail path: repeated target-not-found errors trigger tier 2 replan (bypass fix)", async () => {
    // Target IDs that do NOT exist in the context — grounding will reject each.
    // Previously the `continue` after grounding error bypassed the tier gate;
    // the inline triggerReplan call at the grounding site fixes this.
    const script: ScriptedStep[] = [
      { step: planned(clickOn("nonexistent-a")) },
      { step: planned(clickOn("nonexistent-b")) },
      { step: planned(clickOn("nonexistent-c")) },
      { step: planned(clickOn("nonexistent-d")) },
      { step: planned(clickOn("btn-a")) }, // this one exists — fresh strategy might use it
      { step: planned(DONE_ACTION) },
    ];
    const cel = makeCel(script);
    const result = await runGoal(cel as CelLike, baseConfig({
      enableTierReplan: true,
      selfHeal: false,
    }), makeCallbacks({ executeOk: true }));
    expect(result.metrics?.tier2Replans ?? 0).toBeGreaterThanOrEqual(1);
  }, TEST_TIMEOUT);

  it("semantic stall disabled: actions succeed without state change — NO tier-replan (flag gate holds)", async () => {
    const targets = ["btn-a", "btn-b", "btn-c", "btn-d", "btn-e", "btn-f", "btn-g"];
    const script: ScriptedStep[] = targets.map((t) => ({ step: planned(clickOn(t)) }));
    const cel = makeCel(script);
    const result = await runGoal(cel as CelLike, baseConfig({
      enableTierReplan: true,
      enableSemanticStallEscalation: false, // off
      maxStepsWithoutProgress: 30,
      selfHeal: false,
    }), makeCallbacks({
      executeOk: true,
      stateChanges: false,
      verifyGoal: async () => false,
    }));
    expect(result.metrics?.tier2Replans ?? 0).toBe(0);
  }, TEST_TIMEOUT);
});
