import { describe, expect, it, vi } from "vitest";

import { CelLlmPlanner, type PlannerSurface } from "./llm-planner.js";
import type { NextMove, PlanningView } from "./canonical.js";

function makeFakeView(goal: string): PlanningView {
  return {
    goal,
    budget: {
      max_tokens: 8000,
      max_elements: 80,
      max_memories: 8,
      max_adapter_facts: 12,
    },
    screen: { active_app: "Test App" },
    elements: [],
    adapter_facts: [],
    adapter_actions: [],
    capabilities: [],
    run_progress: { steps_used: 0, max_steps: 80 },
    memories: [],
    knowledge: [],
    recent_events: [],
    blockers: [],
    anomalies: [],
    evidence: [],
    omitted_counts: {
      elements: 0,
      memories: 0,
      knowledge: 0,
      adapter_facts: 0,
      recent_events: 0,
    },
  };
}

describe("CelLlmPlanner", () => {
  it("delegates decideNext to the canonical Rust planner via N-API", async () => {
    const expected: NextMove = {
      kind: "batch",
      purpose: "click the button",
      steps: [
        {
          purpose: "click submit",
          kind: "deterministic",
          action: { type: "click", target_id: "ax:submit" },
        },
      ],
    };
    const buildView = vi.fn(async (goal: string) => makeFakeView(goal));
    const decideNext = vi.fn(async () => expected);
    const verifyDone = vi.fn(async () => ({ verified: true, reason: "" }));
    const cel: PlannerSurface = {
      canonicalBuildPlanningView: buildView,
      canonicalDecideNext: decideNext,
      canonicalVerifyDone: verifyDone,
    };
    const planner = new CelLlmPlanner(cel);

    const move = await planner.decideNext({
      goal: "click submit",
      history: [],
      sharedMemory: {},
      frame: {
        perception: {
          app: "Test App",
          window: "Dialog",
          timestamp_ms: Date.now(),
          elements: [],
        },
        screenshot_base64: null,
        caps: {
          cdp_bound: false,
          native_input: true,
          steps_used: 0,
          max_steps: 0,
        },
      },
    });

    expect(buildView).toHaveBeenCalledTimes(1);
    expect(decideNext).toHaveBeenCalledTimes(1);
    expect(move).toEqual(expected);
  });

  it("short-circuits with Fail when history exceeds maxSteps", async () => {
    const buildView = vi.fn();
    const decideNext = vi.fn();
    const cel: PlannerSurface = {
      canonicalBuildPlanningView: buildView,
      canonicalDecideNext: decideNext,
      canonicalVerifyDone: vi.fn(),
    };
    const planner = new CelLlmPlanner(cel, { maxSteps: 2 });

    const move = await planner.decideNext({
      goal: "anything",
      history: [
        {
          step_purpose: "a",
          action: { type: "wait", ms: 1 },
          succeeded: true,
          data: null,
        },
        {
          step_purpose: "b",
          action: { type: "wait", ms: 1 },
          succeeded: true,
          data: null,
        },
      ],
      sharedMemory: {},
      frame: {
        perception: {
          app: "x",
          window: "y",
          timestamp_ms: 0,
          elements: [],
        },
        screenshot_base64: null,
        caps: {
          cdp_bound: false,
          native_input: false,
          steps_used: 0,
          max_steps: 0,
        },
      },
    });

    expect(move.kind).toBe("fail");
    // Did NOT touch the cortex when the budget is exhausted.
    expect(buildView).not.toHaveBeenCalled();
    expect(decideNext).not.toHaveBeenCalled();
  });

  it("delegates verifyDone to the canonical Rust planner via N-API", async () => {
    const expected = { verified: false, reason: "missing evidence" };
    const buildView = vi.fn(async (goal: string) => makeFakeView(goal));
    const verifyDone = vi.fn(async () => expected);
    const cel: PlannerSurface = {
      canonicalBuildPlanningView: buildView,
      canonicalDecideNext: vi.fn(),
      canonicalVerifyDone: verifyDone,
    };
    const planner = new CelLlmPlanner(cel);

    const verdict = await planner.verifyDone({
      goal: "any",
      summary: "claimed done",
      sharedMemory: {},
      frame: {
        perception: {
          app: "x",
          window: "y",
          timestamp_ms: 0,
          elements: [],
        },
        screenshot_base64: null,
        caps: {
          cdp_bound: false,
          native_input: false,
          steps_used: 0,
          max_steps: 0,
        },
      },
    });

    expect(buildView).toHaveBeenCalledTimes(1);
    expect(verifyDone).toHaveBeenCalledTimes(1);
    expect(verdict).toEqual(expected);
  });
});
