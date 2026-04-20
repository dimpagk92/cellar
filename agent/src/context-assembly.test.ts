import { describe, it, expect, vi } from "vitest";
import { assembleContext, formatContextSummary, findActionTarget, validateAction, type StepResult } from "./context-assembly.js";
import type { Workflow, ScreenContext, ContextElement } from "./types.js";

function makeCel(overrides: Record<string, unknown> = {}) {
  return {
    getWorkingMemory: vi.fn().mockReturnValue("# Mappings\n- Vendor X → 10045"),
    getObservations: vi.fn().mockReturnValue([
      { id: 1, content: "Vendor X always maps to code 10045", priority: "high", workflow_name: "daily-po" },
      { id: 2, content: "SAP takes ~3s after Submit", priority: "medium", workflow_name: "daily-po" },
    ]),
    searchKnowledge: vi.fn().mockReturnValue([
      { id: 1, content: "PO field requires 8-digit code", score: 0.85 },
    ]),
    ...overrides,
  } as any;
}

function makeWorkflow(): Workflow {
  return {
    name: "daily-po",
    description: "Daily purchase order entry",
    app: "SAP",
    version: "1.0.0",
    steps: [
      { id: "s1", description: "Open PO form", action: { type: "click", target: "po-btn" } },
      { id: "s2", description: "Enter vendor code", action: { type: "type", target: "vendor-field", text: "10045" } },
      { id: "s3", description: "Submit", action: { type: "click", target: "submit-btn" } },
    ],
    created_at: "2024-01-01",
    updated_at: "2024-01-01",
  };
}

function makeScreen(): ScreenContext {
  return {
    app: "SAP",
    window: "Create PO",
    elements: [
      { id: "po-btn", element_type: "button", confidence: 0.95, source: "accessibility_tree", state: { focused: false, enabled: true, visible: true, selected: false } },
    ],
    timestamp_ms: Date.now(),
  };
}

describe("assembleContext", () => {
  it("should assemble all context layers", () => {
    const cel = makeCel();
    const ctx = assembleContext(cel, makeWorkflow(), 0, makeScreen(), []);

    expect(ctx.workflow.name).toBe("daily-po");
    expect(ctx.workflow.currentStep).toBe(0);
    expect(ctx.workflow.totalSteps).toBe(3);
    expect(ctx.workingMemory).toContain("Vendor X");
    expect(ctx.observations).toHaveLength(2);
    expect(ctx.knowledge).toHaveLength(1);
    expect(ctx.screen.app).toBe("SAP");
    expect(ctx.currentStep.id).toBe("s1");
  });

  it("should pass workflow name to memory lookups", () => {
    const cel = makeCel();
    assembleContext(cel, makeWorkflow(), 1, makeScreen(), []);

    expect(cel.getWorkingMemory).toHaveBeenCalledWith("daily-po");
    expect(cel.getObservations).toHaveBeenCalledWith("daily-po", 50);
    expect(cel.searchKnowledge).toHaveBeenCalledWith("Enter vendor code", "daily-po", 5);
  });

  it("should limit recent steps", () => {
    const cel = makeCel();
    const steps: StepResult[] = Array.from({ length: 20 }, (_, i) => ({
      stepIndex: i,
      stepId: `s${i}`,
      description: `Step ${i}`,
      success: true,
      confidence: 0.9,
    }));

    const ctx = assembleContext(cel, makeWorkflow(), 2, makeScreen(), steps, {
      maxRecentSteps: 5,
    });
    expect(ctx.recentSteps).toHaveLength(5);
    expect(ctx.recentSteps[0].stepIndex).toBe(15); // last 5
  });

  it("should use custom config", () => {
    const cel = makeCel();
    assembleContext(cel, makeWorkflow(), 0, makeScreen(), [], {
      maxObservations: 10,
      maxKnowledge: 3,
    });

    expect(cel.getObservations).toHaveBeenCalledWith("daily-po", 10);
    expect(cel.searchKnowledge).toHaveBeenCalledWith("Open PO form", "daily-po", 3);
  });

  it("should handle empty memory gracefully", () => {
    const cel = makeCel({
      getWorkingMemory: vi.fn().mockReturnValue(""),
      getObservations: vi.fn().mockReturnValue([]),
      searchKnowledge: vi.fn().mockReturnValue([]),
    });

    const ctx = assembleContext(cel, makeWorkflow(), 0, makeScreen(), []);
    expect(ctx.workingMemory).toBe("");
    expect(ctx.observations).toHaveLength(0);
    expect(ctx.knowledge).toHaveLength(0);
  });
});

describe("findActionTarget", () => {
  function makeEl(overrides: Partial<ContextElement> = {}): ContextElement {
    return {
      id: "btn-1",
      label: "OK",
      element_type: "button",
      confidence: 0.90,
      source: "accessibility_tree",
      bounds: { x: 100, y: 100, width: 80, height: 30 },
      state: { focused: false, enabled: true, visible: true, selected: false },
      ...overrides,
    };
  }

  function screenWith(...elements: ContextElement[]): ScreenContext {
    return { app: "Test", window: "Test", elements, timestamp_ms: Date.now() };
  }

  it("finds element by ID", () => {
    const screen = screenWith(makeEl({ id: "btn-1" }));
    const found = findActionTarget(screen, "btn-1");
    expect(found).toBeDefined();
    expect(found!.id).toBe("btn-1");
  });

  it("finds element by label", () => {
    const screen = screenWith(makeEl({ id: "btn-1", label: "Submit" }));
    const found = findActionTarget(screen, "Submit");
    expect(found).toBeDefined();
    expect(found!.id).toBe("btn-1");
  });

  it("skips disabled elements", () => {
    const screen = screenWith(
      makeEl({ id: "btn-1", state: { focused: false, enabled: false, visible: true, selected: false } }),
    );
    expect(findActionTarget(screen, "btn-1")).toBeUndefined();
  });

  it("skips invisible elements", () => {
    const screen = screenWith(
      makeEl({ id: "btn-1", state: { focused: false, enabled: true, visible: false, selected: false } }),
    );
    expect(findActionTarget(screen, "btn-1")).toBeUndefined();
  });

  it("returns undefined when no match", () => {
    const screen = screenWith(makeEl({ id: "btn-1" }));
    expect(findActionTarget(screen, "nonexistent")).toBeUndefined();
  });

  it("prefers element with actions when multiple match", () => {
    const screen = screenWith(
      makeEl({ id: "ok", label: "OK", confidence: 0.90, actions: [] }),
      makeEl({ id: "ok-2", label: "OK", confidence: 0.85, actions: ["click"] }),
    );
    const found = findActionTarget(screen, "OK");
    expect(found).toBeDefined();
    expect(found!.id).toBe("ok-2");
  });

  it("falls back to highest confidence when no actions", () => {
    const screen = screenWith(
      makeEl({ id: "ok-low", label: "OK", confidence: 0.70 }),
      makeEl({ id: "ok-high", label: "OK", confidence: 0.90 }),
    );
    const found = findActionTarget(screen, "OK");
    expect(found!.id).toBe("ok-high");
  });
});

describe("validateAction", () => {
  function screenWith(el: Partial<ContextElement>): ScreenContext {
    return {
      app: "Test", window: "Test", timestamp_ms: Date.now(),
      elements: [{
        id: "btn-1", label: "OK", element_type: "button",
        confidence: 0.90, source: "accessibility_tree" as const,
        bounds: { x: 100, y: 100, width: 80, height: 30 },
        state: { focused: false, enabled: true, visible: true, selected: false },
        ...el,
      }],
    };
  }

  it("returns null for valid target", () => {
    expect(validateAction(screenWith({}), "btn-1")).toBeNull();
  });

  it("returns error for missing target", () => {
    const err = validateAction(screenWith({}), "nonexistent");
    expect(err).toContain("not found");
  });

  it("returns error for target without bounds", () => {
    const err = validateAction(screenWith({ bounds: undefined }), "btn-1");
    expect(err).toContain("no bounds");
  });
});

describe("formatContextSummary", () => {
  it("should format a readable summary", () => {
    const cel = makeCel();
    const ctx = assembleContext(cel, makeWorkflow(), 0, makeScreen(), [
      { stepIndex: 0, stepId: "s0", description: "Prep", success: true, confidence: 0.9 },
    ]);

    const summary = formatContextSummary(ctx);
    expect(summary).toContain("step 1/3");
    expect(summary).toContain("daily-po");
    expect(summary).toContain("SAP");
    expect(summary).toContain("Working memory: 2 lines");
    expect(summary).toContain("1 high");
    expect(summary).toContain("1 relevant facts");
    expect(summary).toContain("1/1 succeeded");
  });

  it("should show actions count when elements have actions", () => {
    const cel = makeCel();
    const screen: ScreenContext = {
      app: "SAP",
      window: "Create PO",
      elements: [
        { id: "btn-1", element_type: "button", confidence: 0.90, source: "accessibility_tree",
          state: { focused: false, enabled: true, visible: true, selected: false },
          actions: ["click", "press"] },
        { id: "btn-2", element_type: "button", confidence: 0.90, source: "accessibility_tree",
          state: { focused: false, enabled: true, visible: true, selected: false },
          actions: ["click"] },
        { id: "txt-1", element_type: "text", confidence: 0.85, source: "accessibility_tree",
          state: { focused: false, enabled: true, visible: true, selected: false } },
      ],
      timestamp_ms: Date.now(),
    };
    const ctx = assembleContext(cel, makeWorkflow(), 0, screen, []);
    const summary = formatContextSummary(ctx);
    expect(summary).toContain("2 with actions");
    expect(summary).toContain("3 actionable");
  });

  it("should show focused element when present", () => {
    const cel = makeCel();
    const screen: ScreenContext = {
      app: "SAP",
      window: "Create PO",
      elements: [
        { id: "input-vendor", label: "Vendor Code", element_type: "input", confidence: 0.90,
          source: "accessibility_tree",
          state: { focused: true, enabled: true, visible: true, selected: false } },
      ],
      timestamp_ms: Date.now(),
    };
    const ctx = assembleContext(cel, makeWorkflow(), 0, screen, []);
    const summary = formatContextSummary(ctx);
    expect(summary).toContain("Focused:");
    expect(summary).toContain("input-vendor");
    expect(summary).toContain("Vendor Code");
  });

  it("should not show focused line when no focused element", () => {
    const cel = makeCel();
    const ctx = assembleContext(cel, makeWorkflow(), 0, makeScreen(), []);
    const summary = formatContextSummary(ctx);
    expect(summary).not.toContain("Focused:");
  });
});
