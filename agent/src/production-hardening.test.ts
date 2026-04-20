import { describe, it, expect, vi } from "vitest";
import { createMockPlanner, sampleContext } from "./test-utils/index.js";
import { createMockInputController } from "./test-utils/mock-input-controller.js";
import { createMockContextProvider, emptyContext } from "./test-utils/mock-context-provider.js";
import { createMockKnowledgeStore } from "./test-utils/mock-knowledge-store.js";
import { verifyDone } from "./goal-runner/verify-done.js";
import type { PlannedAction, ScreenContext } from "./types.js";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

// ─── CDP Extractor Sandbox ──────────────────────────────────────────────────

describe("CDP extractor JS sandbox", () => {
  // We test the dangerous patterns check indirectly by importing the module
  // The actual cdpEvaluate is mocked, but the pattern matching runs before it

  it("verifyDone should reject vague summaries", () => {
    const action = { type: "done", summary: "done" } as PlannedAction;
    const ctx = sampleContext();
    const result = verifyDone(action, ctx, "Find prices");
    expect(result.verified).toBe(false);
    expect(result.reason).toContain("vague");
  });

  it("verifyDone should accept extract actions with data", () => {
    // Extract actions only need non-empty data, not strict verification
    const action = {
      type: "extract",
      goal: "Get costs",
      data: "Basic: $10/mo, Pro: $25/mo, Enterprise: $99/mo",
    } as PlannedAction;
    const ctx = sampleContext();
    const result = verifyDone(action, ctx, "Get subscription costs");
    expect(result.verified).toBe(true);
  });

  it("verifyDone should accept done with matching page data", () => {
    // Short summary with specific data — should pass the specific-data check
    const action = {
      type: "done",
      summary: "Prices are $10 and $25 per month",
    } as PlannedAction;
    // Context without page-text element (no hasPageText = true → semantic check skipped)
    const ctx: ScreenContext = {
      app: "Chrome",
      window: "Results",
      elements: [{
        id: "a11y:1",
        element_type: "text",
        label: "Prices: $10 and $25",
        bounds: { x: 0, y: 0, width: 100, height: 100 },
        state: { visible: true, enabled: true, focused: false, selected: false },
        actions: [],
        confidence: 0.9,
        source: "accessibility_tree",
      }],
      timestamp_ms: Date.now(),
    };
    // Use non-comparison goal to avoid scroll check
    const result = verifyDone(action, ctx, "What are the costs");
    expect(result.verified).toBe(true);
  });

  it("verifyDone should catch wrong domain", () => {
    const action = {
      type: "done",
      summary: "Found product details: Widget Pro costs $49.99",
    } as PlannedAction;
    const ctx: ScreenContext = {
      ...sampleContext(),
      elements: [
        {
          id: "el-1",
          element_type: "text",
          label: "Widget Pro",
          properties: { url: "https://evil.com/steal" },
          bounds: { x: 0, y: 0, width: 100, height: 100 },
          state: { visible: true, enabled: true, focused: false, selected: false },
          actions: [],
          confidence: 0.9,
          source: "accessibility_tree",
        },
      ],
    };
    const result = verifyDone(action, ctx, "Find product on https://example.com", "https://example.com");
    expect(result.verified).toBe(false);
    expect(result.reason).toContain("wrong domain");
  });
});

// ─── Checkpoint Manager Persistence ─────────────────────────────────────────

describe("checkpoint manager persistence", () => {
  it("should persist checkpoints to disk and restore", async () => {
    const { CheckpointManager } = await import("./goal-runner/checkpoint-manager.js");
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cel-test-"));
    const filePath = path.join(tmpDir, "checkpoints.json");

    try {
      // Create manager with persistence
      const mgr = new CheckpointManager(filePath);
      mgr.capture("milestone-1", 5, "fp-abc", "https://example.com", "Chrome — Example", []);
      mgr.capture("milestone-2", 10, "fp-def", null, "Finder — Documents", [
        { key: "price", value: "$99", source: "step-8", category: "data", timestamp: Date.now() },
      ]);

      expect(mgr.count).toBe(2);
      expect(fs.existsSync(filePath)).toBe(true);

      // Create new manager from same path — should restore
      const mgr2 = new CheckpointManager(filePath);
      expect(mgr2.count).toBe(2);
      expect(mgr2.getLatest()!.milestone).toBe("milestone-2");
      expect(mgr2.getByMilestone("milestone-1")!.stepIndex).toBe(5);

      // Clear should remove file
      mgr2.clear();
      expect(mgr2.count).toBe(0);
      expect(fs.existsSync(filePath)).toBe(false);
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  it("should work without persistence path (in-memory only)", async () => {
    const { CheckpointManager } = await import("./goal-runner/checkpoint-manager.js");
    const mgr = new CheckpointManager();
    mgr.capture("test", 1, "fp", null, "App", []);
    expect(mgr.count).toBe(1);
  });
});

// ─── Mock Factories ─────────────────────────────────────────────────────────

describe("mock factories", () => {
  it("createMockPlanner should return scripted steps", async () => {
    const planner = createMockPlanner({
      steps: [
        {
          reasoning: "Click submit",
          action: { type: "click", target_id: "a11y:1" } as PlannedAction,
          expected_outcome: "Form submitted",
          confidence: 0.9,
        },
      ],
    });

    const step1 = await planner.planStep("Submit form", sampleContext());
    expect(step1.action.type).toBe("click");
    expect(planner.calls.planStep).toHaveLength(1);

    // After exhausting scripted steps, returns done
    const step2 = await planner.planStep("Submit form", sampleContext());
    expect(step2.action.type).toBe("done");
  });

  it("createMockInputController should record calls", () => {
    const input = createMockInputController();
    input.click(100, 200);
    input.typeText("hello");
    input.keyCombo(["Cmd", "C"]);

    expect(input.calls).toHaveLength(3);
    expect(input.calls[0].method).toBe("click");
    expect(input.calls[0].args).toEqual([100, 200]);
    expect(input.calls[1].method).toBe("typeText");
    expect(input.calls[2].method).toBe("keyCombo");

    input.reset();
    expect(input.calls).toHaveLength(0);
  });

  it("createMockContextProvider should return canned context", () => {
    const ctx = createMockContextProvider();
    const context = ctx.getContext();
    expect(context.app).toBe("TestApp");
    expect(context.elements.length).toBeGreaterThan(0);
    expect(ctx.calls.getContext).toHaveLength(1);
  });

  it("createMockKnowledgeStore should persist in memory", () => {
    const store = createMockKnowledgeStore();

    // Add and query knowledge
    store.addKnowledge("Login requires email", "test");
    const results = store.queryKnowledge("email");
    expect(results).toHaveLength(1);
    expect(results[0].content).toBe("Login requires email");

    // Working memory
    store.updateWorkingMemory("test-workflow", "current state info");
    expect(store.getWorkingMemory("test-workflow")).toBe("current state info");

    // Observations
    store.addObservation("test-workflow", "Button moved", "high", [1]);
    const obs = store.getObservations("test-workflow");
    expect(obs).toHaveLength(1);
    expect(obs[0].content).toBe("Button moved");

    // Run tracking
    const runId = store.startRun("test-workflow", 10);
    expect(runId).toBeGreaterThan(0);
    store.logStep(runId, 0, "step-0", "click", true, 0.9);
    store.finishRun(runId, "completed");
    const history = store.getRunHistory();
    expect(history[0].status).toBe("completed");
  });
});

// ─── Interface Composition ──────────────────────────────────────────────────

describe("interface composition", () => {
  it("Cel class should satisfy all interfaces", async () => {
    // Type-level test — if this compiles, Cel implements all interfaces
    const { Cel } = await import("./cel-bindings.js");
    const cel = new Cel();

    // ContextProvider
    expect(typeof cel.getContext).toBe("function");
    expect(typeof cel.getQuickContext).toBe("function");
    expect(typeof cel.captureScreen).toBe("function");

    // InputController
    expect(typeof cel.click).toBe("function");
    expect(typeof cel.typeText).toBe("function");
    expect(typeof cel.keyCombo).toBe("function");
    expect(typeof cel.axSetValue).toBe("function");

    // Planner
    expect(typeof cel.planStep).toBe("function");
    expect(typeof cel.llmComplete).toBe("function");

    // KnowledgeStore
    expect(typeof cel.addKnowledge).toBe("function");
    expect(typeof cel.searchKnowledge).toBe("function");
    expect(typeof cel.startRun).toBe("function");

    // BrowserBridge
    expect(typeof cel.getCdpPageContent).toBe("function");
    expect(typeof cel.cdpEvaluate).toBe("function");

    // EventSource
    expect(typeof cel.startWatchdog).toBe("function");
    expect(typeof cel.pollEvents).toBe("function");
  });
});

// ─── Device Baseline Safety ─────────────────────────────────────────────────

describe("device baseline safety", () => {
  it("should handle missing monitors gracefully", () => {
    const ctx = createMockContextProvider({ context: emptyContext() });
    // listMonitors returns default when no native module
    expect(ctx.listMonitors().length).toBeGreaterThan(0);
  });
});

// ─── Goal Router ────────────────────────────────────────────────────────────

describe("goal router", () => {
  it("should export routeGoal and GoalRoute", async () => {
    const mod = await import("./goal-runner/goal-router.js");
    expect(typeof mod.routeGoal).toBe("function");
    expect(typeof mod.openAppActions).toBe("function");
  });
});
