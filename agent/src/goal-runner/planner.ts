/**
 * Planning helper — routes between text-only and vision-based planning.
 * Handles retries and JSON extraction from LLM output.
 */

import type { Planner as PlannerInterface } from "../interfaces/planner.js";
import type { PlannedStep, PlannerStepRecord, ScreenContext } from "../types.js";
import type { GoalRunnerCallbacks } from "./config.js";
import { extractJsonObject, sleep } from "./helpers.js";
import { diffContexts, formatDiffForPrompt } from "../context-differ.js";
// Prompt templating is owned by the Rust planner (cel.buildPlanPrompt + cel.planStep).
// resolveStepIndices is the one TS-side post-processor that maps numbered indices
// returned by the LLM back to concrete element IDs — imported statically here so
// the dependency is explicit rather than a dynamic require.
import { resolveStepIndices } from "../cel-bindings.js";

const PLAN_MAX_RETRIES = 3;
/** Threshold step index at which we switch to conversation-aware planning. */
const CONVERSATION_THRESHOLD = 3;
const PLAN_CALL_TIMEOUT_MS = 30_000;

function withPlanTimeout<T>(promise: Promise<T>, label: string): Promise<T> {
  return Promise.race([
    promise,
    new Promise<T>((_, reject) =>
      setTimeout(() => reject(new Error(`${label} timeout after ${PLAN_CALL_TIMEOUT_MS}ms`)), PLAN_CALL_TIMEOUT_MS),
    ),
  ]);
}

/**
 * Plan a single step with retry logic.
 * For steps >= CONVERSATION_THRESHOLD, uses conversation-aware planning
 * that sends prior assistant responses + context diffs instead of rebuilding
 * full context each time. This helps the LLM learn from its own prior actions.
 */
export async function planStep(
  cel: PlannerInterface,
  goal: string,
  context: ScreenContext,
  history: PlannerStepRecord[],
  loopWarning: string | null,
  maxSteps: number,
  useVision: boolean,
  callbacks: GoalRunnerCallbacks,
  deviceBaselineJson?: string | null,
  modelOverride?: string,
  conversation?: PlannerConversation | null,
  stepIndex?: number,
): Promise<PlannedStep> {
  const effectiveGoal = loopWarning
    ? `${goal}\n\nWARNING: ${loopWarning}`
    : goal;

  const planOptions = {
    maxSteps,
    loopWarning: loopWarning ?? undefined,
    deviceBaselineJson: deviceBaselineJson ?? undefined,
    model: modelOverride,
  };

  let lastError: unknown;
  // Reuse a buffer across retries within this planStep call so retry 2+
  // doesn't re-capture a screenshot that was already taken this step.
  let screenshotBuf: Buffer | null = null;

  // Opt-in deep profiler for the planner retry loop. CELLAR_PROFILE=1.
  const profile = typeof process !== "undefined" && process.env.CELLAR_PROFILE === "1";
  const plog = (phase: string, ms: number) => {
    if (profile) process.stderr.write(`{"lvl":"profile","step":"planner","phase":"${phase}","ms":${ms}}\n`);
  };

  for (let attempt = 0; attempt < PLAN_MAX_RETRIES; attempt++) {
    const tAttempt = Date.now();
    try {
      // Vision path: screenshot + structured context → LLM
      // On attempt 0 capture fresh (or consume pre-fetched cache); retries reuse.
      if (useVision && callbacks.screenshot) {
        try {
          if (screenshotBuf === null) {
            const tScreenshot = Date.now();
            const cached = (callbacks as any)._cachedScreenshot as Buffer | null;
            screenshotBuf = cached ?? await callbacks.screenshot();
            if (cached) (callbacks as any)._cachedScreenshot = null; // consume cache
            plog(`screenshot.attempt${attempt}`, Date.now() - tScreenshot);
          }
          const base64 = screenshotBuf.toString("base64");
          const tVision = Date.now();
          const result = await withPlanTimeout(
            cel.planStepWithVision(effectiveGoal, context, base64, history, planOptions),
            "planStepWithVision",
          );
          plog(`planStepWithVision.attempt${attempt}`, Date.now() - tVision);
          plog(`attempt${attempt}.ok`, Date.now() - tAttempt);
          return result;
        } catch {
          // Vision failed — fall through to text-only (keep buffer for potential reuse)
          plog(`planStepWithVision.attempt${attempt}.fail`, Date.now() - tAttempt);
        }
      }

      // Conversation-aware path: for steps 3+, send the full conversation thread
      // through Rust's multi-turn LLM API so the model sees its own prior responses
      // as proper assistant messages (not flat text in the user prompt).
      if (conversation && (stepIndex ?? 0) >= CONVERSATION_THRESHOLD && conversation.messages.length >= 4) {
        try {
          const tConvBuild = Date.now();
          // Build system prompt + index map from Rust (deterministic element numbering)
          const prompts = cel.buildPlanPrompt(effectiveGoal, context, history, planOptions);

          // Build multi-turn messages: system + conversation history + current user
          const messages: Array<{ role: string; content: string }> = [
            { role: "system", content: prompts.system },
          ];

          // Add recent conversation turns (skip the system message from PlannerConversation)
          const recentMsgs = conversation.messages.filter(m => m.role !== "system").slice(-8);
          for (const msg of recentMsgs) {
            messages.push({ role: msg.role, content: msg.content.slice(0, 2000) });
          }

          // Add current step's full context as the final user message
          messages.push({ role: "user", content: prompts.user });
          plog(`conv.build.attempt${attempt}`, Date.now() - tConvBuild);

          const tConvLlm = Date.now();
          const raw = await withPlanTimeout(
            cel.llmCompleteWithMessages(messages, 8192),
            "llmCompleteWithMessages",
          );
          plog(`conv.llm.attempt${attempt}`, Date.now() - tConvLlm);
          const cleaned = raw.replace(/```json?\n?/g, "").replace(/```/g, "").trim();
          const jsonStr = extractJsonObject(cleaned);
          const step = JSON.parse(jsonStr || cleaned) as PlannedStep;
          // Resolve numbered indices back to real element IDs
          resolveStepIndices(step, prompts.index_map);
          plog(`attempt${attempt}.ok`, Date.now() - tAttempt);
          return step;
        } catch {
          // Conversation path failed — fall through to standard path
          plog(`conv.attempt${attempt}.fail`, Date.now() - tAttempt);
        }
      }

      // Standard text-only path: structured context → Rust planner → LLM
      const tStd = Date.now();
      const result = await withPlanTimeout(
        cel.planStep(effectiveGoal, context, history, planOptions),
        "planStep",
      );
      plog(`planStep.attempt${attempt}`, Date.now() - tStd);
      plog(`attempt${attempt}.ok`, Date.now() - tAttempt);
      return result;
    } catch (e: unknown) {
      lastError = e;
      const msg = String(e);

      // Phase 5 (Provider-Native Structured Output): OpenAI and Gemini use
      // response_format: { type: "json_object" }, and Anthropic uses assistant
      // prefill with "{" — so most responses are valid JSON. This fallback
      // handles edge cases (reasoning models that don't support response_format,
      // or rare provider-side issues).
      if (msg.includes("LLM output parse error") && msg.includes("Raw:")) {
        const rawStart = msg.indexOf("Raw:") + 4;
        const rawContent = msg.slice(rawStart).trim();
        const jsonStr = extractJsonObject(rawContent);
        if (jsonStr) {
          try {
            return JSON.parse(jsonStr) as PlannedStep;
          } catch {
            // JSON extraction failed too — fall through to retry
          }
        }
      }

      if (attempt < PLAN_MAX_RETRIES - 1) {
        await sleep(500 * (attempt + 1));
      }
    }
  }

  // All retries exhausted — return a safe fallback instead of crashing.
  // Include page-text content so the extract has real data instead of empty string.
  const errorMsg = String(lastError).slice(0, 100);
  console.warn(`[planner] All ${PLAN_MAX_RETRIES} retries failed: ${errorMsg}. Returning extract fallback.`);
  const pageText = context.elements?.find(e => e.id === "page-text")?.value?.slice(0, 500) ?? "";
  return {
    reasoning: `Planning failed after ${PLAN_MAX_RETRIES} retries (${errorMsg}). Extracting visible data.`,
    action: { type: "extract", goal: "Extract any visible data from the current page", data: pageText || "No data available" },
    expected_outcome: "Return whatever data is currently visible",
    confidence: 0.3,
  };
}

/**
 * Conversation thread for persistent LLM context across steps.
 * Maintains messages array and handles diff-based context updates.
 */
export interface ConversationMessage {
  role: "system" | "user" | "assistant";
  content: string;
}

export class PlannerConversation {
  messages: ConversationMessage[] = [];
  private previousContext: ScreenContext | null = null;
  private readonly maxMessages = 22; // system + 10 turns

  constructor(systemPrompt: string) {
    this.messages.push({ role: "system", content: systemPrompt });
  }

  /** Build the user message for this step. Step 0 = full context, step N = diff. */
  buildUserMessage(
    step: number,
    goal: string,
    context: ScreenContext,
    lastAction?: string,
    lastError?: string,
  ): string {
    // Adaptive: full context for steps 0-2 (simple tasks finish here),
    // diff-based from step 3+ (complex tasks benefit from memory).
    const MULTITURN_THRESHOLD = 3;
    if (step < MULTITURN_THRESHOLD || !this.previousContext || step % 10 === 0) {
      this.previousContext = context;
      const elemSummary = context.elements
        .slice(0, 60)
        .map((e, i) => `[${i}] ${e.element_type} "${e.label ?? ""}" ${e.state.visible ? "visible" : "hidden"}`)
        .join("\n");
      return `TASK: ${goal}\nAPP: ${context.app} — ${context.window}\n\nELEMENTS:\n${elemSummary}\n\nRespond with JSON action.`;
    }

    // Diff-based update
    const diff = diffContexts(this.previousContext, context);
    const diffText = formatDiffForPrompt(diff);
    this.previousContext = context;

    const errorInfo = lastError ? `\nERROR: ${lastError} — try a DIFFERENT approach!` : "";
    return `RESULT: ${lastAction ?? "unknown"} executed.${errorInfo}\n\nCONTEXT UPDATE:\n${diffText}\n\nStep ${step + 1}. Respond with JSON action.`;
  }

  /** Add a user message and return updated messages for LLM call. */
  addUserMessage(content: string): ConversationMessage[] {
    this.messages.push({ role: "user", content });
    return this.messages;
  }

  /** Record the assistant's response. */
  addAssistantMessage(content: string): void {
    this.messages.push({ role: "assistant", content });
    // Compact if too long
    if (this.messages.length > this.maxMessages) {
      this.messages = [this.messages[0], ...this.messages.slice(-10)];
    }
  }
}
