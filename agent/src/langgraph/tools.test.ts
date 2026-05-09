import { describe, expect, it, vi } from "vitest";

import { createCortexTools } from "./tools.js";
import type {
  CellarLangGraphDriver,
  PerceptionFrame,
  PlanningView,
} from "./index.js";

const baseFrame: PerceptionFrame = {
  perception: {
    app: "Test App",
    window: "Dialog",
    timestamp_ms: 1_700_000_000_000,
    elements: [
      {
        id: "ax:submit",
        label: "Submit",
        element_type: "button",
        state: { focused: false, enabled: true, visible: true, selected: false },
        confidence: 1,
        source: "accessibility_tree",
        actions: ["click"],
      },
    ],
  },
  screenshot_base64: null,
  caps: {
    cdp_bound: false,
    native_input: true,
    steps_used: 0,
    max_steps: 8,
  },
};

const baseView: PlanningView = {
  goal: "click submit",
  budget: {
    max_tokens: 12_000,
    max_elements: 30,
    max_memories: 5,
    max_adapter_facts: 5,
  },
  screen: {
    active_app: "Test App",
    window: "Dialog",
    summary: null,
    url: null,
  },
  elements: [
    {
      id: "ax:submit",
      element_type: "button",
      label: "Submit",
      value: null,
      state: {
        focused: false,
        selected: false,
        enabled: true,
        checked: false,
        expanded: false,
      },
      clickable: true,
      settable: false,
    },
  ],
  adapter_facts: [],
  capabilities: [{ id: "native_input", detail: null }],
  run_progress: { steps_used: 0, max_steps: 8 },
  memories: [
    {
      id: 99,
      kind: "outcome",
      summary: "Submitted invoice via Concur successfully",
      content: { goal: "submit invoice" },
      created_at: "2026-04-01T00:00:00Z",
    },
  ],
  knowledge: [],
  recent_events: [],
  blockers: [],
  anomalies: [],
  evidence: [],
  selection_rationale:
    "Selected 1 element(s) from 1 candidate(s). Hydrated 1 workflow memory (dropped 0 below threshold).",
  omitted_counts: {
    elements: 0,
    memories: 0,
    knowledge: 0,
    adapter_facts: 0,
    recent_events: 0,
  },
};

describe("createCortexTools — WK3 PlanningView migration", () => {
  it("uses driver.buildPlanningView when goal + builder are provided", async () => {
    const buildPlanningView = vi.fn(async () => baseView);
    const driver: CellarLangGraphDriver = {
      perceive: vi.fn(async () => baseFrame),
      executeStep: vi.fn(async () => ({ status: "ok" as const })),
      buildPlanningView,
    };

    const { see, session } = createCortexTools({
      driver,
      goal: "click submit",
    });

    const raw = await see.invoke({});
    const result = JSON.parse(raw as string);

    expect(buildPlanningView).toHaveBeenCalledTimes(1);
    expect(buildPlanningView).toHaveBeenCalledWith(
      "click submit",
      expect.objectContaining({
        perception: baseFrame.perception,
        caps: baseFrame.caps,
      }),
    );

    // Serialized context comes from the PlanningView path, not the
    // legacy compressContext one — pin on a marker that only the new
    // path produces (the formatted [N] element_type "label" id=... line).
    expect(result.context).toContain("[1] button \"Submit\" id=ax:submit");
    expect(result.context).toContain("clickable");
    expect(result.element_count).toBe(1);

    // PlanningView-only diagnostics surfaced.
    expect(result.selection_rationale).toBe(baseView.selection_rationale);
    expect(result.omitted_counts).toEqual(baseView.omitted_counts);
    expect(result.memories).toEqual([
      { id: 99, kind: "outcome", summary: "Submitted invoice via Concur successfully" },
    ]);

    // indexMap is built from view.elements: numeric "1" maps to "ax:submit".
    expect(session.lastIndexMap.get(1)).toBe("ax:submit");
  });

  it("falls back to compressContext path when driver lacks buildPlanningView", async () => {
    const driver: CellarLangGraphDriver = {
      perceive: vi.fn(async () => baseFrame),
      executeStep: vi.fn(async () => ({ status: "ok" as const })),
      // no buildPlanningView
    };

    const { see } = createCortexTools({
      driver,
      goal: "click submit",
    });

    const raw = await see.invoke({});
    const result = JSON.parse(raw as string);

    // Legacy path doesn't surface PlanningView-only fields.
    expect(result.selection_rationale).toBeUndefined();
    expect(result.omitted_counts).toBeUndefined();
    expect(result.memories).toBeUndefined();
  });

  it("falls back to compressContext path when goal is not provided", async () => {
    const buildPlanningView = vi.fn(async () => baseView);
    const driver: CellarLangGraphDriver = {
      perceive: vi.fn(async () => baseFrame),
      executeStep: vi.fn(async () => ({ status: "ok" as const })),
      buildPlanningView,
    };

    const { see } = createCortexTools({
      driver,
      // no goal — selector has nothing to score against
    });

    await see.invoke({});

    // The new path requires a goal; without one we don't even attempt
    // the builder call.
    expect(buildPlanningView).not.toHaveBeenCalled();
  });

  it("falls back to compressContext path when buildPlanningView throws", async () => {
    const buildPlanningView = vi.fn(async () => {
      throw new Error("simulated cortex builder failure");
    });
    const driver: CellarLangGraphDriver = {
      perceive: vi.fn(async () => baseFrame),
      executeStep: vi.fn(async () => ({ status: "ok" as const })),
      buildPlanningView,
    };

    const { see } = createCortexTools({
      driver,
      goal: "click submit",
    });

    // Suppress the warn from the fallback so it doesn't pollute test output.
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const raw = await see.invoke({});
      const result = JSON.parse(raw as string);
      expect(buildPlanningView).toHaveBeenCalledTimes(1);
      // Builder threw → legacy path used → no PlanningView-only fields.
      expect(result.selection_rationale).toBeUndefined();
      // But the rest of the see() output is still well-formed.
      expect(result.app).toBe("Test App");
    } finally {
      warnSpy.mockRestore();
    }
  });
});
