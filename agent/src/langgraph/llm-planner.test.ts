import { describe, expect, it, vi } from "vitest";

import { CelLlmPlanner } from "./llm-planner.js";

describe("CelLlmPlanner", () => {
  it("resolves indexed target ids into real context ids", async () => {
    const planner = new CelLlmPlanner({
      llmCompleteWithRole: vi.fn(async () => JSON.stringify({
        kind: "batch",
        purpose: "click the button",
        steps: [
          {
            purpose: "click submit",
            kind: "deterministic",
            action: {
              type: "click",
              target_id: "1",
            },
          },
        ],
      })),
      llmCompleteWithImage: vi.fn(async () => {
        throw new Error("unexpected image call");
      }),
    }, { maxSteps: 10 });

    const move = await planner.decideNext({
      goal: "click submit",
      history: [],
      sharedMemory: {},
      frame: {
        perception: {
          app: "Test App",
          window: "Dialog",
          timestamp_ms: Date.now(),
          elements: [
            {
              id: "ax:submit",
              label: "Submit",
              element_type: "button",
              state: {
                focused: false,
                enabled: true,
                visible: true,
                selected: false,
              },
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
          max_steps: 0,
        },
      },
    });

    expect(move.kind).toBe("batch");
    if (move.kind !== "batch") {
      throw new Error("expected batch");
    }
    expect(move.steps[0].action).toEqual({
      type: "click",
      target_id: "ax:submit",
    });
  });
});
