/**
 * Mock Planner for testing.
 *
 * Returns scripted PlannedStep sequences. No LLM calls are made.
 */

import type { Planner } from "../interfaces/planner.js";
import type { PlannedStep, PlannerStepRecord, ScreenContext } from "../types.js";

/** Default "done" step returned when no scripted steps remain. */
const DONE_STEP: PlannedStep = {
  reasoning: "Goal achieved (mock)",
  action: { type: "done", summary: "Task completed" },
  expected_outcome: "Goal is achieved",
  confidence: 1.0,
};

export interface MockPlannerOptions {
  /** Scripted steps to return in order. After exhausting, returns DONE_STEP. */
  steps?: PlannedStep[];
  /** Custom LLM completion response. Default: "mock-llm-response". */
  llmResponse?: string;
}

/** Create a type-safe mock Planner. */
export function createMockPlanner(
  options?: MockPlannerOptions,
): Planner & { stepIndex: number; calls: Record<string, unknown[][]> } {
  const steps = [...(options?.steps ?? [])];
  const llmResponse = options?.llmResponse ?? "mock-llm-response";
  const calls: Record<string, unknown[][]> = {};
  let stepIndex = 0;

  function track(method: string, args: unknown[]) {
    if (!calls[method]) calls[method] = [];
    calls[method].push(args);
  }

  function nextStep(): PlannedStep {
    if (stepIndex < steps.length) {
      return steps[stepIndex++];
    }
    return DONE_STEP;
  }

  return {
    get stepIndex() { return stepIndex; },
    set stepIndex(v) { stepIndex = v; },
    calls,

    async planStep(goal, context, history, options) {
      track("planStep", [goal, context, history, options]);
      return nextStep();
    },
    async planStepBlind(goal, history, deviceBaselineJson, options) {
      track("planStepBlind", [goal, history, deviceBaselineJson, options]);
      return nextStep();
    },
    async planStepWithVision(goal, context, screenshotBase64, history, options) {
      track("planStepWithVision", [goal, context, screenshotBase64, history, options]);
      return nextStep();
    },
    buildPlanPrompt(goal, context, history, options) {
      track("buildPlanPrompt", [goal, context, history, options]);
      return { system: "mock-system", user: "mock-user", index_map: [] };
    },
    async llmComplete(systemPrompt, userPrompt, maxTokens) {
      track("llmComplete", [systemPrompt, userPrompt, maxTokens]);
      return llmResponse;
    },
    async llmCompleteWithRole(systemPrompt, userPrompt, role, maxTokens) {
      track("llmCompleteWithRole", [systemPrompt, userPrompt, role, maxTokens]);
      return llmResponse;
    },
    async llmCompleteWithImage(systemPrompt, imageBase64, userPrompt, maxTokens) {
      track("llmCompleteWithImage", [systemPrompt, imageBase64, userPrompt, maxTokens]);
      return llmResponse;
    },
    async llmCompleteWithMessages(messages, maxTokens) {
      track("llmCompleteWithMessages", [messages, maxTokens]);
      return llmResponse;
    },
  };
}
