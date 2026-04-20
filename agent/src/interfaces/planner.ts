/**
 * Planner — LLM step planning and completion.
 *
 * Abstracts all LLM-facing operations from the Cel god class.
 * Consumers that need to plan or call LLMs should depend on this
 * interface, not the full Cel class.
 */

import type { PlannedStep, PlannerStepRecord, ScreenContext } from "../types.js";

export interface Planner {
  /** Plan a single step given a goal, current context, and step history. */
  planStep(
    goal: string,
    context: ScreenContext,
    history?: PlannerStepRecord[],
    options?: {
      maxSteps?: number;
      loopWarning?: string;
      deviceBaselineJson?: string;
      model?: string;
    },
  ): Promise<PlannedStep>;

  /** Plan a step WITHOUT screen context (blind mode). */
  planStepBlind(
    goal: string,
    history: PlannerStepRecord[],
    deviceBaselineJson: string,
    options?: { maxSteps?: number; loopWarning?: string },
  ): Promise<PlannedStep>;

  /** Plan a step with vision: structured context + screenshot. */
  planStepWithVision(
    goal: string,
    context: ScreenContext,
    screenshotBase64: string,
    history?: PlannerStepRecord[],
    options?: { maxSteps?: number; loopWarning?: string },
  ): Promise<PlannedStep>;

  /** Build prompts without calling the LLM. */
  buildPlanPrompt(
    goal: string,
    context: ScreenContext,
    history?: PlannerStepRecord[],
    options?: { maxSteps?: number; loopWarning?: string },
  ): { system: string; user: string; index_map: string[] };

  /** Send a text-only LLM completion. */
  llmComplete(
    systemPrompt: string,
    userPrompt: string,
    maxTokens?: number,
  ): Promise<string>;

  /** Send a role-aware LLM completion. */
  llmCompleteWithRole(
    systemPrompt: string,
    userPrompt: string,
    role: string,
    maxTokens?: number,
  ): Promise<string>;

  /** Send an LLM completion with an attached image. */
  llmCompleteWithImage(
    systemPrompt: string,
    imageBase64: string,
    userPrompt: string,
    maxTokens?: number,
  ): Promise<string>;

  /** Send a multi-turn LLM completion with a conversation thread. */
  llmCompleteWithMessages(
    messages: Array<{ role: string; content: string }>,
    maxTokens?: number,
  ): Promise<string>;
}
