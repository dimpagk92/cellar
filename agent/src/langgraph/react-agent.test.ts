import { describe, expect, it, vi } from "vitest";

import { createCellarReactAgent, extractFinalAgentText } from "./react-agent.js";
import type {
  CellarLangGraphDriver,
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

describe("createCellarReactAgent", () => {
  it("runs the LangGraph agent through see, act, and final answer", async () => {
    const llmCompleteWithRole = vi.fn()
      .mockResolvedValueOnce(JSON.stringify({
        kind: "tool",
        name: "see",
        args: {},
        thought: "inspect first",
      }))
      .mockResolvedValueOnce(JSON.stringify({
        kind: "tool",
        name: "act",
        args: {
          purpose: "click submit",
          action: {
            type: "click",
            target_id: "1",
          },
        },
        thought: "click the button",
      }))
      .mockResolvedValueOnce(JSON.stringify({
        kind: "tool",
        name: "see",
        args: {},
        thought: "verify completion",
      }))
      .mockResolvedValueOnce(JSON.stringify({
        kind: "tool",
        name: "done_check",
        args: {
          draft_answer: "Clicked submit successfully.",
        },
        thought: "validate before finishing",
      }))
      .mockResolvedValueOnce(JSON.stringify({
        kind: "final",
        content: "Clicked submit successfully.",
      }));

    const perceive = vi.fn(async (): Promise<PerceptionFrame> => ({
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
        max_steps: 8,
      },
    }));

    const executeStep = vi.fn(async () => ({
      status: "ok" as const,
      data: { clicked: true },
    }));

    const buildPlanningView = vi.fn(async (goal: string) => makeStubPlanningView(goal));
    const driver: CellarLangGraphDriver = {
      perceive,
      executeStep,
      buildPlanningView,
    };

    const { agent, session } = createCellarReactAgent({
      driver,
      llm: {
        llmCompleteWithRole,
      },
      maxActions: 8,
    });

    const result = await agent.invoke({
      messages: [
        {
          role: "user",
          content: "Click submit",
        },
      ],
    }, {
      configurable: {
        thread_id: "react-agent-test",
      },
      recursionLimit: 20,
    });

    expect(perceive).toHaveBeenCalledTimes(2);
    expect(executeStep).toHaveBeenCalledTimes(1);
    expect(executeStep).toHaveBeenCalledWith({
      purpose: "click submit",
      kind: "llm_assisted",
      action: {
        type: "click",
        target_id: "ax:submit",
      },
    });
    expect(session.executedSteps).toBe(1);
    expect(extractFinalAgentText(result.messages)).toBe("Clicked submit successfully.");
    expect(llmCompleteWithRole).toHaveBeenCalledTimes(5);
  });
});
