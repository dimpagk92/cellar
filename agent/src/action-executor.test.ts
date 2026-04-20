import { describe, it, expect, vi } from "vitest";
import { executeAction } from "./action-executor.js";
import type { WorkflowStep, ScreenContext } from "./types.js";

function makeCel() {
  return {
    click: vi.fn(),
    rightClick: vi.fn(),
    doubleClick: vi.fn(),
    typeText: vi.fn(),
    keyPress: vi.fn(),
    keyCombo: vi.fn(),
    scroll: vi.fn(),
    mouseMove: vi.fn(),
  } as any;
}

function makeContext(elements: ScreenContext["elements"] = []): ScreenContext {
  return {
    app: "TestApp",
    window: "Main",
    elements,
    timestamp_ms: Date.now(),
  };
}

describe("executeAction", () => {
  it("should click a target by ID", async () => {
    const cel = makeCel();
    const step: WorkflowStep = {
      id: "s1",
      description: "Click submit",
      action: { type: "click", target: "submit-btn" },
    };
    const ctx = makeContext([
      {
        id: "submit-btn",
        element_type: "button",
        confidence: 0.95,
        source: "accessibility_tree",
        bounds: { x: 100, y: 200, width: 80, height: 30 },
        state: { focused: false, enabled: true, visible: true, selected: false },
      },
    ]);

    const result = await executeAction(cel, step, ctx);
    expect(result).toBe(true);
    expect(cel.click).toHaveBeenCalledWith(140, 215);
  });

  it("should right-click when button is right", async () => {
    const cel = makeCel();
    const step: WorkflowStep = {
      id: "s1",
      description: "Right-click cell",
      action: { type: "click", target: "cell-a1", button: "right" },
    };
    const ctx = makeContext([
      {
        id: "cell-a1",
        element_type: "table_cell",
        confidence: 0.9,
        source: "native_api",
        bounds: { x: 50, y: 50, width: 100, height: 20 },
        state: { focused: false, enabled: true, visible: true, selected: false },
      },
    ]);

    await executeAction(cel, step, ctx);
    expect(cel.rightClick).toHaveBeenCalledWith(100, 60);
  });

  it("should throw when click target not found", async () => {
    const cel = makeCel();
    const step: WorkflowStep = {
      id: "s1",
      description: "Click missing",
      action: { type: "click", target: "nonexistent" },
    };
    const ctx = makeContext([]);

    await expect(executeAction(cel, step, ctx)).rejects.toThrow(
      "Target element not found",
    );
  });

  it("should type text into a target field", async () => {
    const cel = makeCel();
    const step: WorkflowStep = {
      id: "s1",
      description: "Type in search",
      action: { type: "type", target: "search-box", text: "hello world" },
    };
    const ctx = makeContext([
      {
        id: "search-box",
        element_type: "input",
        confidence: 0.88,
        source: "accessibility_tree",
        bounds: { x: 200, y: 100, width: 300, height: 30 },
        state: { focused: false, enabled: true, visible: true, selected: false },
      },
    ]);

    await executeAction(cel, step, ctx);
    expect(cel.click).toHaveBeenCalledWith(350, 115);
    expect(cel.typeText).toHaveBeenCalledWith("hello world");
  });

  it("should press a key", async () => {
    const cel = makeCel();
    const step: WorkflowStep = {
      id: "s1",
      description: "Press Enter",
      action: { type: "key", key: "Enter" },
    };
    const ctx = makeContext();

    await executeAction(cel, step, ctx);
    expect(cel.keyPress).toHaveBeenCalledWith("Enter");
  });

  it("should press a key combo", async () => {
    const cel = makeCel();
    const step: WorkflowStep = {
      id: "s1",
      description: "Copy",
      action: { type: "key_combo", keys: ["Ctrl", "C"] },
    };
    const ctx = makeContext();

    await executeAction(cel, step, ctx);
    expect(cel.keyCombo).toHaveBeenCalledWith(["Ctrl", "C"]);
  });

  it("should wait for specified duration", async () => {
    const cel = makeCel();
    const step: WorkflowStep = {
      id: "s1",
      description: "Wait 50ms",
      action: { type: "wait", ms: 50 },
    };
    const ctx = makeContext();
    const start = Date.now();
    await executeAction(cel, step, ctx);
    expect(Date.now() - start).toBeGreaterThanOrEqual(40);
  });

  it("should scroll", async () => {
    const cel = makeCel();
    const step: WorkflowStep = {
      id: "s1",
      description: "Scroll down",
      action: { type: "scroll", dx: 0, dy: -3 },
    };
    const ctx = makeContext();

    await executeAction(cel, step, ctx);
    expect(cel.scroll).toHaveBeenCalledWith(0, -3);
  });

  it("should handle custom actions gracefully", async () => {
    const cel = makeCel();
    const step: WorkflowStep = {
      id: "s1",
      description: "Custom Excel action",
      action: {
        type: "custom",
        adapter: "excel",
        action: "read_cell",
        params: { cell: "A1" },
      },
    };
    const ctx = makeContext();

    const result = await executeAction(cel, step, ctx);
    expect(result).toBe(true);
  });

  it("should resolve target by label (case-insensitive)", async () => {
    const cel = makeCel();
    const step: WorkflowStep = {
      id: "s1",
      description: "Click OK button",
      action: { type: "click", target: "ok" },
    };
    const ctx = makeContext([
      {
        id: "btn-123",
        label: "OK",
        element_type: "button",
        confidence: 0.9,
        source: "accessibility_tree",
        bounds: { x: 400, y: 300, width: 60, height: 30 },
        state: { focused: false, enabled: true, visible: true, selected: false },
      },
    ]);

    await executeAction(cel, step, ctx);
    expect(cel.click).toHaveBeenCalledWith(430, 315);
  });
});
