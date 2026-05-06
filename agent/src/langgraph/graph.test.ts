import { describe, expect, it } from "vitest";

import { createCellarGraph } from "./graph.js";
import { createInitialCellarGraphState } from "./state.js";
import type {
  CanonicalStep,
  CellarLangGraphDriver,
  CellarLangGraphPlanner,
  DoneVerdict,
  NextMove,
  PerceptionFrame,
  PlanningView,
} from "./index.js";

function makeStubPlanningView(goal: string): PlanningView {
  return {
    goal,
    budget: {
      max_tokens: 8000,
      max_elements: 80,
      max_memories: 8,
      max_adapter_facts: 12,
    },
    screen: { active_app: "TestApp" },
    elements: [],
    adapter_facts: [],
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

describe("createCellarGraph", () => {
  it("loops through perceive, plan, execute, and verify_done", async () => {
    let perceiveCalls = 0;
    let executeCalls = 0;
    let decideCalls = 0;

    const frame: PerceptionFrame = {
      perception: {
        app: "Test App",
        window: "Main",
        elements: [],
        timestamp_ms: Date.now(),
      },
      screenshot_base64: null,
      caps: {
        cdp_bound: false,
        native_input: true,
        steps_used: 0,
        max_steps: 0,
      },
    };

    const step: CanonicalStep = {
      purpose: "click the only button",
      kind: "deterministic",
      action: {
        type: "click",
        target_id: "ax:test-button",
      },
    };

    const driver: CellarLangGraphDriver = {
      async perceive() {
        perceiveCalls += 1;
        return frame;
      },
      async executeStep() {
        executeCalls += 1;
        return {
          status: "ok",
          data: { clicked: true },
        };
      },
      async buildPlanningView(goal) {
        return makeStubPlanningView(goal);
      },
    };

    const planner: CellarLangGraphPlanner = {
      async decideNext(): Promise<NextMove> {
        decideCalls += 1;
        if (decideCalls === 1) {
          return {
            kind: "batch",
            purpose: "complete the one-step action",
            steps: [step],
          };
        }
        return {
          kind: "done",
          summary: "Clicked the only button",
          extracted_data: { clicked: true },
        };
      },
      async verifyDone(): Promise<DoneVerdict> {
        return {
          verified: true,
          reason: "",
        };
      },
    };

    const graph = createCellarGraph({
      driver,
      planner,
      policy: {
        captureScreenshot: () => false,
        interruptBeforeStep: () => false,
        perceiveAfterStep: () => true,
      },
    });

    const result = await graph.invoke(
      createInitialCellarGraphState("click the button", "test-run"),
      { configurable: { thread_id: "test-run" } },
    );

    expect(result.outcome).toEqual({
      status: "succeeded",
      summary: "Clicked the only button",
      extracted_data: { clicked: true },
    });
    expect(result.history).toHaveLength(1);
    expect(result.sharedMemory).toEqual({
      "click the only button": { clicked: true },
    });
    expect(perceiveCalls).toBeGreaterThanOrEqual(2);
    expect(decideCalls).toBe(2);
    expect(executeCalls).toBe(1);
  });
});
