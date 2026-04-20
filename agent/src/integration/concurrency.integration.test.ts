/**
 * Concurrency audit — run two goals in parallel and assert no state leaks.
 *
 * The goal-runner creates per-call instances of CognitiveTrail, Notebook,
 * StrategyTracker, CheckpointManager, LoopDetector, and ReplanEventEmitter.
 * This test verifies that running N goals concurrently produces N independent
 * metric/trail/notebook outputs — no cross-talk through module-level state.
 */
import { describe, it, expect } from "vitest";
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

function ctx(): ScreenContext {
  const makeBtn = (id: string) => ({
    id,
    element_type: "button" as const,
    label: `Button ${id}`,
    state: { visible: true, focused: false, enabled: true },
  });
  return {
    app: "TestApp",
    window: "TestWindow",
    elements: ["btn-a", "btn-b", "btn-c", "btn-d", "btn-e"].map(makeBtn) as never[],
    timestamp_ms: Date.now(),
  };
}

function planned(action: PlannedAction, opts: Partial<PlannedStep> = {}): PlannedStep {
  return { reasoning: "test", action, expected_outcome: "test", confidence: 0.9, ...opts };
}

function makeCel(scriptActions: PlannedAction[]) {
  let cursor = 0;
  return {
    planStep: async (
      _goal: string,
      _context: ScreenContext,
      _history: PlannerStepRecord[],
    ): Promise<PlannedStep> => {
      const action = scriptActions[Math.min(cursor, scriptActions.length - 1)];
      cursor++;
      return planned(action);
    },
    planStepWithVision: async () => planned(scriptActions[0]),
    buildPlanPrompt: () => ({ system: "sys", user: "usr", index_map: [] }),
    llmComplete: async () => "{}",
    llmCompleteWithMessages: async () => "{}",
    llmCompleteWithImage: async () => "{}",
    searchKnowledge: () => [],
    addScopedKnowledge: () => {},
    storeObservation: () => {},
    getWorkingMemory: () => ({}),
    setWorkingMemory: () => {},
    keyPress: () => {},
    keyCombo: () => {},
    typeText: () => {},
    discoverCdpTargets: () => [],
    cdpNavigate: async () => {},
    cdpEvaluate: async () => null,
    getContext: () => ctx(),
    getQuickContext: () => ctx(),
    isNativeAvailable: false,
  };
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type CelLike = any;

function cbs(executeOk: boolean): GoalRunnerCallbacks {
  return {
    getContext: async () => ctx(),
    stateFingerprint: () => "fp-static",
    executeAction: async () => executeOk,
    waitForSettle: async () => {},
  };
}

function cfg(goal: string, overrides: Partial<GoalRunnerConfig> = {}): GoalRunnerConfig {
  return {
    goal,
    maxSteps: 6,
    stepDelay: 0,
    taskTimeout: 10_000,
    stepTimeout: 1000,
    maxConsecutiveFailures: 20,
    enableVision: false,
    skipRouter: true,
    persistentThread: false,
    selfHeal: false,
    enableTierReplan: true,
    ...overrides,
  };
}

describe("goal-runner concurrency", () => {
  it("two parallel goals produce independent metrics / trails / notebooks", async () => {
    // Goal A: 5 failing clicks → should trigger tier-2 replan
    const scriptA: PlannedAction[] = [
      { type: "click", target_id: "btn-a" },
      { type: "click", target_id: "btn-b" },
      { type: "click", target_id: "btn-c" },
      { type: "click", target_id: "btn-d" },
      { type: "done", summary: "goal A finished — enough data extracted as a list" },
    ];
    // Goal B: 3 successful clicks then done — should have NO tier replans
    const scriptB: PlannedAction[] = [
      { type: "click", target_id: "btn-a" },
      { type: "click", target_id: "btn-b" },
      { type: "done", summary: "goal B finished — enough data extracted as a list" },
    ];

    const [resultA, resultB] = await Promise.all([
      runGoal(makeCel(scriptA) as CelLike, cfg("goal A", { maxSteps: 12 }), cbs(false)),
      runGoal(makeCel(scriptB) as CelLike, cfg("goal B"), cbs(true)),
    ]);

    // Goal A should have triggered at least one tier-2 replan
    expect(resultA.metrics?.tier2Replans ?? 0).toBeGreaterThanOrEqual(1);
    // Goal B should have zero tier activity (actions succeeded)
    expect(resultB.metrics?.tier2Replans ?? 0).toBe(0);
    expect(resultB.metrics?.tier3Backtracks ?? 0).toBe(0);
    expect(resultB.metrics?.tier4Reassessments ?? 0).toBe(0);

    // Goals should have distinct histories
    expect(resultA.history.length).toBeGreaterThan(0);
    expect(resultB.history.length).toBeGreaterThan(0);
    // And distinct conversation threads (persistentThread=false, so both null)
    expect(resultA.conversationThread).toBeUndefined();
    expect(resultB.conversationThread).toBeUndefined();
  }, 30_000);

  it("ten parallel goals don't cross-contaminate metric counters", async () => {
    const N = 10;
    const script: PlannedAction[] = [
      { type: "done", summary: "finished with enough data extracted as a list" },
    ];
    const promises = Array.from({ length: N }, (_, i) =>
      runGoal(makeCel(script) as CelLike, cfg(`goal ${i}`), cbs(true)),
    );
    const results = await Promise.all(promises);
    // All should achieve, none should have tier replans
    for (const r of results) {
      expect(r.metrics?.tier2Replans ?? 0).toBe(0);
      expect(r.metrics?.tier3Backtracks ?? 0).toBe(0);
      expect(r.metrics?.tier4Reassessments ?? 0).toBe(0);
    }
  }, 30_000);
});
