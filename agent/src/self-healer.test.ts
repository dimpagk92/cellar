import { describe, it, expect, vi } from "vitest";
import { selfHeal } from "./self-healer.js";
import type { ScreenContext, PlannedAction, PlannedStep, PlannerStepRecord } from "./types.js";
import type { GoalRunnerCallbacks } from "./goal-runner.js";
import type { Planner } from "./interfaces/planner.js";

function makeContext(elements: { id: string; visible?: boolean; enabled?: boolean }[] = []): ScreenContext {
  return {
    app: "TestApp",
    window: "Main",
    timestamp_ms: Date.now(),
    elements: elements.map(e => ({
      id: e.id,
      element_type: "button",
      label: e.id,
      state: { visible: e.visible ?? true, enabled: e.enabled ?? true },
      actions: ["click"],
      bounds: { x: 0, y: 0, width: 100, height: 30 },
    })),
  } as ScreenContext;
}

function makeCallbacks(freshContext: ScreenContext): GoalRunnerCallbacks {
  return {
    getContext: vi.fn().mockResolvedValue(freshContext),
    screenshot: undefined,
  } as any;
}

function makePlanner(repairedAction: PlannedAction, reasoning = "Try alternative button"): Planner {
  return {
    planStep: vi.fn().mockResolvedValue({
      action: repairedAction,
      reasoning,
      expected_outcome: "element clicked",
      confidence: 0.8,
    } as PlannedStep),
    planStepWithVision: vi.fn().mockResolvedValue({
      action: repairedAction,
      reasoning,
      expected_outcome: "element clicked",
      confidence: 0.8,
    } as PlannedStep),
  } as any;
}

describe("selfHeal", () => {
  const failedAction: PlannedAction = { type: "click", target_id: "btn-1" };
  const repairedAction: PlannedAction = { type: "click", target_id: "btn-2" };

  it("should return healingContext with metadata on successful heal", async () => {
    const freshContext = makeContext([{ id: "btn-2" }]);
    const callbacks = makeCallbacks(freshContext);
    const planner = makePlanner(repairedAction, "Clicked alternative submit button");

    const result = await selfHeal(
      failedAction, "element not found", callbacks, planner,
      "Submit the form", [],
    );

    expect(result).not.toBeNull();
    expect(result!.repairedAction).toEqual(repairedAction);
    expect(result!.healingContext).toEqual({
      failedAction,
      failureReason: "element not found",
      contextShifted: false,
      repairDescription: "Clicked alternative submit button",
    });
  });

  it("should detect context shift when fingerprint changes", async () => {
    const freshContext = makeContext([{ id: "btn-2" }, { id: "btn-3" }]);
    const callbacks = makeCallbacks(freshContext);
    const planner = makePlanner(repairedAction);

    const result = await selfHeal(
      failedAction, "click intercepted", callbacks, planner,
      "Submit the form", [],
      { originalContextFingerprint: 99999 }, // Different from fresh context
    );

    expect(result).not.toBeNull();
    expect(result!.healingContext.contextShifted).toBe(true);
  });

  it("should not detect context shift when fingerprint matches", async () => {
    const freshContext = makeContext([{ id: "btn-2" }]);
    const callbacks = makeCallbacks(freshContext);
    const planner = makePlanner(repairedAction);

    // Import contextFingerprint to get the actual fingerprint
    const { contextFingerprint } = await import("./goal-runner/helpers.js");
    const fp = contextFingerprint(freshContext);

    const result = await selfHeal(
      failedAction, "click intercepted", callbacks, planner,
      "Submit the form", [],
      { originalContextFingerprint: fp },
    );

    expect(result).not.toBeNull();
    expect(result!.healingContext.contextShifted).toBe(false);
  });

  it("should query HistoryAdvisor when knowledgeStore is provided", async () => {
    const freshContext = makeContext([{ id: "btn-2" }]);
    const callbacks = makeCallbacks(freshContext);
    const planner = makePlanner(repairedAction);

    const knowledgeStore = {
      searchKnowledge: vi.fn().mockReturnValue([
        { id: 1, content: "Button moves after animation", source: "past-run", score: 0.9 },
      ]),
    } as any;

    const result = await selfHeal(
      failedAction, "element not found", callbacks, planner,
      "Submit the form", [],
      { knowledgeStore, workflowName: "test-wf" },
    );

    expect(result).not.toBeNull();
    // Planner should have been called with a repair goal that includes past advice
    const planCall = (planner.planStep as any).mock.calls[0];
    expect(planCall[0]).toContain("REPAIR NEEDED");
  });

  it("should return null when all attempts fail", async () => {
    const freshContext = makeContext([{ id: "btn-2" }]);
    const callbacks = makeCallbacks(freshContext);
    // Planner returns same action as failed — will be rejected
    const planner = makePlanner(failedAction);

    const result = await selfHeal(
      failedAction, "error", callbacks, planner,
      "test goal", [],
      { maxAttempts: 2 },
    );

    expect(result).toBeNull();
  });

  it("should reject done/fail responses from planner", async () => {
    const freshContext = makeContext([{ id: "btn-2" }]);
    const callbacks = makeCallbacks(freshContext);
    const doneAction: PlannedAction = { type: "done", summary: "already done" };
    const planner = makePlanner(doneAction);

    const result = await selfHeal(
      failedAction, "error", callbacks, planner,
      "test goal", [],
      { maxAttempts: 1 },
    );

    expect(result).toBeNull();
  });

  it("should reject ungrounded actions", async () => {
    // Fresh context has btn-3, but planner suggests btn-2 (not in context)
    const freshContext = makeContext([{ id: "btn-3" }]);
    const callbacks = makeCallbacks(freshContext);
    const planner = makePlanner(repairedAction); // btn-2

    const result = await selfHeal(
      failedAction, "error", callbacks, planner,
      "test goal", [],
      { maxAttempts: 1 },
    );

    expect(result).toBeNull();
  });
});
