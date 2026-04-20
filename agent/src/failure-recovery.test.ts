import { describe, it, expect } from "vitest";
import {
  getReplanTier,
  replanRouter,
  triggerReplan,
} from "./goal-runner/failure-recovery.js";
import { StrategyTracker } from "./goal-runner/strategy-tracker.js";
import { LoopDetector } from "./goal-runner/loop-detector.js";
import { CognitiveTrail } from "./goal-runner/cognitive-trail.js";
import { Notebook } from "./goal-runner/notebook.js";
import { CheckpointManager } from "./goal-runner/checkpoint-manager.js";
import { HistoryAdvisor } from "./goal-runner/history-advisor.js";

describe("replanRouter", () => {
  it("returns 'nudge' below the failure threshold", () => {
    const st = new StrategyTracker();
    st.register("m", "init");
    const ld = new LoopDetector();
    expect(replanRouter(1, st, ld, "m")).toBe("nudge");
  });

  it("returns 'new_strategy' at tier 2 (3+ failures)", () => {
    const st = new StrategyTracker();
    st.register("m", "init");
    const ld = new LoopDetector();
    expect(replanRouter(3, st, ld, "m")).toBe("new_strategy");
  });

  it("returns 'backtrack' when current milestone has exhausted strategies", () => {
    const st = new StrategyTracker();
    for (let i = 0; i < 3; i++) {
      const id = st.register("m", `s${i}`);
      st.recordOutcome(id, "failed");
    }
    const ld = new LoopDetector();
    expect(replanRouter(3, st, ld, "m")).toBe("backtrack");
  });

  it("returns 'reassess' when global cap is exhausted", () => {
    const st = new StrategyTracker();
    for (let i = 0; i < 5; i++) st.register(`m${i}`, `s${i}`);
    const ld = new LoopDetector();
    expect(replanRouter(3, st, ld, "m0")).toBe("reassess");
  });

  it("is one-to-one with getReplanTier", () => {
    const st = new StrategyTracker();
    st.register("m", "init");
    const ld = new LoopDetector();
    expect(getReplanTier(3, st, ld, "m")).toBe(2);
    expect(replanRouter(3, st, ld, "m")).toBe("new_strategy");
  });
});

describe("triggerReplan", () => {
  function setup() {
    const strategyTracker = new StrategyTracker();
    strategyTracker.register("m", "initial");
    return {
      strategyTracker,
      loopDetector: new LoopDetector(),
      notebook: new Notebook(),
      cognitiveTrail: new CognitiveTrail(),
      checkpointManager: new CheckpointManager(),
      metrics: {} as {
        tier2Replans?: number;
        tier3Backtracks?: number;
        tier4Reassessments?: number;
        strategyExhaustedEvents?: number;
      },
    };
  }

  it("increments tier2Replans on a new_strategy decision", async () => {
    const s = setup();
    const outcome = await triggerReplan({
      reason: "reactive_failure",
      stepIndex: 5,
      consecutiveFailures: 3,
      currentMilestone: "m",
      strategyTracker: s.strategyTracker,
      loopDetector: s.loopDetector,
      checkpointManager: s.checkpointManager,
      notebook: s.notebook,
      cognitiveTrail: s.cognitiveTrail,
      historyAdvisor: HistoryAdvisor,
      cel: {},
      goal: "test",
      metrics: s.metrics,
    });
    expect(outcome.tier).toBe(2);
    expect(s.metrics.tier2Replans).toBe(1);
    // Loop detector was reset
    expect(s.loopDetector.loopCount).toBe(0);
    // A failed strategy was recorded
    expect(s.strategyTracker.getFailedStrategies("m").length).toBeGreaterThan(0);
  });

  it("emits needsRedecompose when globally exhausted", async () => {
    const s = setup();
    // Burn through the global cap
    for (let i = 0; i < 5; i++) s.strategyTracker.register(`m${i}`, `s${i}`);
    const outcome = await triggerReplan({
      reason: "reactive_failure",
      stepIndex: 10,
      consecutiveFailures: 4,
      currentMilestone: "m0",
      strategyTracker: s.strategyTracker,
      loopDetector: s.loopDetector,
      checkpointManager: s.checkpointManager,
      notebook: s.notebook,
      cognitiveTrail: s.cognitiveTrail,
      historyAdvisor: HistoryAdvisor,
      cel: {},
      goal: "test",
      metrics: s.metrics,
    });
    expect(outcome.tier).toBe(4);
    expect(outcome.needsRedecompose).toBe(true);
    expect(s.metrics.tier4Reassessments).toBe(1);
    expect(s.metrics.strategyExhaustedEvents).toBe(1);
  });

  it("is a no-op at tier 1 (nudge handled by caller)", async () => {
    const s = setup();
    const outcome = await triggerReplan({
      reason: "reactive_failure",
      stepIndex: 2,
      consecutiveFailures: 1,
      currentMilestone: "m",
      strategyTracker: s.strategyTracker,
      loopDetector: s.loopDetector,
      checkpointManager: s.checkpointManager,
      notebook: s.notebook,
      cognitiveTrail: s.cognitiveTrail,
      historyAdvisor: HistoryAdvisor,
      cel: {},
      goal: "test",
      metrics: s.metrics,
    });
    expect(outcome.tier).toBe(1);
    expect(outcome.loopWarning).toBeNull();
    expect(outcome.backtracked).toBe(false);
    expect(outcome.needsRedecompose).toBe(false);
    // No counters should have been bumped
    expect(s.metrics.tier2Replans ?? 0).toBe(0);
    expect(s.metrics.tier3Backtracks ?? 0).toBe(0);
    expect(s.metrics.tier4Reassessments ?? 0).toBe(0);
    // Strategy tracker should be unchanged
    expect(s.strategyTracker.getFailedStrategies("m").length).toBe(0);
  });

  it("resetGlobalCounter restores the strategy budget without wiping history", () => {
    const st = new StrategyTracker();
    for (let i = 0; i < 5; i++) {
      const id = st.register(`m${i}`, `s${i}`);
      st.recordOutcome(id, "failed");
    }
    expect(st.canReplanGlobal()).toBe(false);
    st.resetGlobalCounter();
    expect(st.canReplanGlobal()).toBe(true);
    // Failed strategies should still be inspectable by the LLM prompt
    expect(st.getFailedStrategies("m0").length).toBe(1);
  });

  it("returns tier 4 consistently across repeat invocations (caller owns the cap)", async () => {
    const s = setup();
    for (let i = 0; i < 5; i++) s.strategyTracker.register(`m${i}`, `s${i}`);
    const first = await triggerReplan({
      reason: "reactive_failure",
      stepIndex: 10,
      consecutiveFailures: 4,
      currentMilestone: "m0",
      strategyTracker: s.strategyTracker,
      loopDetector: s.loopDetector,
      checkpointManager: s.checkpointManager,
      notebook: s.notebook,
      cognitiveTrail: s.cognitiveTrail,
      historyAdvisor: HistoryAdvisor,
      cel: {},
      goal: "test",
      metrics: s.metrics,
    });
    expect(first.tier).toBe(4);
    expect(first.needsRedecompose).toBe(true);
    // Second invocation (simulating post-re-decomposition failure) returns tier 4
    // again — the goal-runner is responsible for capping this, not the helper.
    const second = await triggerReplan({
      reason: "reactive_failure",
      stepIndex: 15,
      consecutiveFailures: 4,
      currentMilestone: "m0",
      strategyTracker: s.strategyTracker,
      loopDetector: s.loopDetector,
      checkpointManager: s.checkpointManager,
      notebook: s.notebook,
      cognitiveTrail: s.cognitiveTrail,
      historyAdvisor: HistoryAdvisor,
      cel: {},
      goal: "test",
      metrics: s.metrics,
    });
    expect(second.tier).toBe(4);
    expect(second.needsRedecompose).toBe(true);
    expect(s.metrics.tier4Reassessments).toBe(2);
  });

  it("backtracks and restores notebook on tier 3", async () => {
    const s = setup();
    // Capture a checkpoint with some notebook data
    s.notebook.write("saved_key", "saved_value", "step-2", "data");
    s.checkpointManager.capture("m", 2, "fp1", null, "app — win", s.notebook.snapshot());
    // Simulate later work that overwrote the notebook
    s.notebook.write("saved_key", "overwritten", "step-5", "data");
    s.notebook.write("new_key", "post_checkpoint", "step-5", "data");
    // Exhaust this milestone so tier escalates to 3
    for (let i = 0; i < 3; i++) {
      const id = s.strategyTracker.register("m", `s${i}`);
      s.strategyTracker.recordOutcome(id, "failed");
    }
    const outcome = await triggerReplan({
      reason: "reactive_failure",
      stepIndex: 8,
      consecutiveFailures: 3,
      currentMilestone: "m",
      strategyTracker: s.strategyTracker,
      loopDetector: s.loopDetector,
      checkpointManager: s.checkpointManager,
      notebook: s.notebook,
      cognitiveTrail: s.cognitiveTrail,
      historyAdvisor: HistoryAdvisor,
      cel: {},
      goal: "test",
      metrics: s.metrics,
    });
    expect(outcome.tier).toBe(3);
    expect(outcome.backtracked).toBe(true);
    expect(s.metrics.tier3Backtracks).toBe(1);
    // Notebook should be restored to the checkpoint's value
    expect(s.notebook.read("saved_key")).toBe("saved_value");
    // Post-checkpoint write should be cleared (clear-then-restore)
    expect(s.notebook.read("new_key")).toBeUndefined();
  });
});
