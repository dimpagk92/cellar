import { describe, it, expect } from "vitest";
import type {
  Workflow,
  WorkflowStep,
  WorkflowAction,
  ScreenContext,
  ContextElement,
  Priority,
  WorkflowStatus,
} from "./types.js";

describe("Type definitions", () => {
  it("should create valid ContextElement", () => {
    const elem: ContextElement = {
      id: "btn-1",
      label: "Submit",
      element_type: "button",
      confidence: 0.95,
      source: "accessibility_tree",
      bounds: { x: 100, y: 200, width: 80, height: 30 },
      state: { focused: false, enabled: true, visible: true, selected: false },
    };
    expect(elem.id).toBe("btn-1");
    expect(elem.confidence).toBe(0.95);
    expect(elem.bounds?.width).toBe(80);
  });

  it("should create valid ScreenContext", () => {
    const ctx: ScreenContext = {
      app: "Excel",
      window: "Sheet1 - Excel",
      elements: [
        {
          id: "cell-a1",
          element_type: "table_cell",
          value: "Revenue",
          confidence: 0.98,
          source: "native_api",
          state: { focused: false, enabled: true, visible: true, selected: false },
        },
      ],
      timestamp_ms: Date.now(),
    };
    expect(ctx.app).toBe("Excel");
    expect(ctx.elements.length).toBe(1);
    expect(ctx.elements[0].source).toBe("native_api");
  });

  it("should support all action types", () => {
    const actions: WorkflowAction[] = [
      { type: "click", target: "btn", button: "left" },
      { type: "click", target: "btn", button: "right" },
      { type: "type", target: "input", text: "Hello" },
      { type: "key", key: "Enter" },
      { type: "key_combo", keys: ["Ctrl", "S"] },
      { type: "wait", ms: 1000 },
      { type: "scroll", dx: 0, dy: -3 },
      { type: "custom", adapter: "excel", action: "read_cell", params: { cell: "A1" } },
    ];
    expect(actions.length).toBe(8);
    expect(actions[0].type).toBe("click");
    expect(actions[7].type).toBe("custom");
  });

  it("should create valid WorkflowStep with optional fields", () => {
    const step: WorkflowStep = {
      id: "step-1",
      description: "Click submit button",
      action: { type: "click", target: "submit-btn" },
      min_confidence: 0.8,
      expected: { app: "Excel" },
    };
    expect(step.min_confidence).toBe(0.8);
    expect(step.expected?.app).toBe("Excel");
  });

  it("should create valid Workflow", () => {
    const wf: Workflow = {
      name: "monthly-report",
      description: "Generate monthly revenue report",
      app: "Excel",
      version: "1.0.0",
      steps: [
        {
          id: "open-file",
          description: "Open workbook",
          action: { type: "key_combo", keys: ["Ctrl", "O"] },
        },
      ],
      context_map: { sheet: "Revenue" },
      created_at: new Date().toISOString(),
      updated_at: new Date().toISOString(),
    };
    expect(wf.name).toBe("monthly-report");
    expect(wf.context_map).toBeDefined();
  });

  it("should accept all priority levels", () => {
    const priorities: Priority[] = ["low", "normal", "high", "critical"];
    expect(priorities.length).toBe(4);
  });

  it("should accept all workflow statuses", () => {
    const statuses: WorkflowStatus[] = [
      "idle", "running", "paused", "completed", "failed", "queued",
    ];
    expect(statuses.length).toBe(6);
  });

  it("should allow elements without optional fields", () => {
    const elem: ContextElement = {
      id: "minimal",
      element_type: "text",
      confidence: 0.5,
      source: "vision",
      state: { focused: false, enabled: true, visible: true, selected: false },
    };
    expect(elem.label).toBeUndefined();
    expect(elem.value).toBeUndefined();
    expect(elem.bounds).toBeUndefined();
  });
});
