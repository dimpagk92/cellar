import { describe, it, expect, vi } from "vitest";
import { WorkflowEngine, type EngineCallbacks } from "./engine.js";
import type { Workflow, ScreenContext, WorkflowStep } from "./types.js";
import type { AssembledContext, StepResult } from "./context-assembly.js";

function makeWorkflow(steps: number): Workflow {
  return {
    name: "test-workflow",
    description: "A test workflow",
    app: "test-app",
    version: "1.0.0",
    steps: Array.from({ length: steps }, (_, i) => ({
      id: `step-${i}`,
      description: `Step ${i}`,
      action: { type: "click" as const, target: `target-${i}` },
    })),
    created_at: new Date().toISOString(),
    updated_at: new Date().toISOString(),
  };
}

function makeContext(confidence = 0.9): ScreenContext {
  return {
    app: "test-app",
    window: "Main",
    elements: [
      {
        id: "elem-1",
        label: "Button",
        element_type: "button",
        confidence,
        source: "accessibility_tree",
        state: { focused: false, enabled: true, visible: true, selected: false },
      },
    ],
    timestamp_ms: Date.now(),
  };
}

function makeCallbacks(overrides: Partial<EngineCallbacks> = {}): EngineCallbacks {
  return {
    getContext: vi.fn(async () => makeContext()),
    executeAction: vi.fn(async (_step: WorkflowStep, _ctx: AssembledContext) => true),
    onPause: vi.fn(async (_step: WorkflowStep, _ctx: AssembledContext) => {}),
    onStepComplete: vi.fn((_step: WorkflowStep, _idx: number, _ctx: AssembledContext) => {}),
    onComplete: vi.fn((_wf: Workflow, _status: string, _steps: StepResult[]) => {}),
    onLog: vi.fn(),
    ...overrides,
  };
}

describe("WorkflowEngine", () => {
  it("should submit workflows and return an ID", () => {
    const callbacks = makeCallbacks();
    const engine = new WorkflowEngine(callbacks);
    const id = engine.submit(makeWorkflow(2));
    expect(id).toMatch(/^wf-/);
  });

  it("should execute all steps of a workflow", async () => {
    const callbacks = makeCallbacks();
    const engine = new WorkflowEngine(callbacks);
    const wf = makeWorkflow(3);
    engine.submit(wf);

    // Start engine briefly then stop
    const startPromise = engine.start();
    // Give it time to process
    await new Promise((r) => setTimeout(r, 100));
    engine.stop();
    await startPromise;

    expect(callbacks.executeAction).toHaveBeenCalledTimes(3);
    expect(callbacks.onStepComplete).toHaveBeenCalledTimes(3);
    expect(callbacks.onComplete).toHaveBeenCalledWith(wf, "completed", expect.any(Array));
  });

  it("should stop on step failure", async () => {
    const callbacks = makeCallbacks({
      executeAction: vi.fn(async (step: WorkflowStep, _ctx: AssembledContext) => {
        // Fail on step-1
        return step.id !== "step-1";
      }),
    });
    const engine = new WorkflowEngine(callbacks);
    engine.submit(makeWorkflow(3));

    const startPromise = engine.start();
    await new Promise((r) => setTimeout(r, 100));
    engine.stop();
    await startPromise;

    expect(callbacks.onComplete).toHaveBeenCalledWith(
      expect.anything(),
      "failed",
      expect.any(Array),
    );
  });

  it("should stop on step exception", async () => {
    const callbacks = makeCallbacks({
      executeAction: vi.fn(async () => {
        throw new Error("Boom");
      }),
    });
    const engine = new WorkflowEngine(callbacks);
    engine.submit(makeWorkflow(2));

    const startPromise = engine.start();
    await new Promise((r) => setTimeout(r, 100));
    engine.stop();
    await startPromise;

    expect(callbacks.onLog).toHaveBeenCalledWith(
      "error",
      expect.stringContaining("Boom")
    );
    expect(callbacks.onComplete).toHaveBeenCalledWith(
      expect.anything(),
      "failed",
      expect.any(Array),
    );
  });

  it("should call onPause when confidence is too low", async () => {
    const callbacks = makeCallbacks({
      getContext: vi.fn(async () => makeContext(0.1)), // Very low confidence
    });
    const engine = new WorkflowEngine(callbacks);
    engine.submit(makeWorkflow(1));

    const startPromise = engine.start();
    await new Promise((r) => setTimeout(r, 100));
    engine.stop();
    await startPromise;

    expect(callbacks.onPause).toHaveBeenCalled();
  });

  it("should not start twice", async () => {
    const callbacks = makeCallbacks();
    const engine = new WorkflowEngine(callbacks);

    const p1 = engine.start();
    const p2 = engine.start(); // Should return immediately
    engine.stop();
    await Promise.all([p1, p2]);
  });

  it("should handle priority in submission", () => {
    const callbacks = makeCallbacks();
    const engine = new WorkflowEngine(callbacks);
    const id1 = engine.submit(makeWorkflow(1), "low");
    const id2 = engine.submit(makeWorkflow(1), "critical");
    expect(id1).not.toBe(id2);
  });

  it("should pass assembled context to callbacks", async () => {
    let receivedContext: AssembledContext | undefined;
    const callbacks = makeCallbacks({
      executeAction: vi.fn(async (_step: WorkflowStep, ctx: AssembledContext) => {
        receivedContext = ctx;
        return true;
      }),
    });
    const engine = new WorkflowEngine(callbacks);
    engine.submit(makeWorkflow(1));

    const startPromise = engine.start();
    await new Promise((r) => setTimeout(r, 100));
    engine.stop();
    await startPromise;

    expect(receivedContext).toBeDefined();
    expect(receivedContext!.workflow.name).toBe("test-workflow");
    expect(receivedContext!.screen.app).toBe("test-app");
    expect(receivedContext!.currentStep.id).toBe("step-0");
  });

  it("should track completed steps across the run", async () => {
    let lastSteps: StepResult[] = [];
    const callbacks = makeCallbacks({
      onComplete: vi.fn((_wf, _status, steps) => {
        lastSteps = steps;
      }),
    });
    const engine = new WorkflowEngine(callbacks);
    engine.submit(makeWorkflow(3));

    const startPromise = engine.start();
    await new Promise((r) => setTimeout(r, 100));
    engine.stop();
    await startPromise;

    expect(lastSteps).toHaveLength(3);
    expect(lastSteps[0].stepId).toBe("step-0");
    expect(lastSteps[0].success).toBe(true);
    expect(lastSteps[2].stepIndex).toBe(2);
  });
});
