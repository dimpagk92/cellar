import { z } from "zod";
import type { Cel, PlannerStepRecord, PlannedAction, ScreenContext } from "@cellar/agent";
import { textResult, errorResult } from "./shared.js";
import { ensureCdpChrome } from "../server.js";

export const celThinkSchema = z.discriminatedUnion("mode", [
  // --- plan ---
  z.object({
    mode: z.literal("plan"),
    goal: z
      .string()
      .describe("The goal to achieve (e.g. 'Open Settings and enable dark mode')"),
    history: z
      .array(
        z.object({
          step_index: z.number(),
          action: z.unknown().describe("Action object taken"),
          success: z.boolean(),
          error: z.string().optional(),
        }),
      )
      .default([])
      .describe("Previous steps taken (for multi-step planning)"),
    max_steps: z.number().optional().describe("Maximum total steps allowed"),
    loop_warning: z.string().optional().describe("Warning if a loop was detected"),
  }),

  // --- plan_with_vision ---
  z.object({
    mode: z.literal("plan_with_vision"),
    goal: z.string().describe("The goal to achieve"),
    history: z
      .array(
        z.object({
          step_index: z.number(),
          action: z.unknown().describe("Action object taken"),
          success: z.boolean(),
          error: z.string().optional(),
        }),
      )
      .default([]),
    max_steps: z.number().optional(),
    loop_warning: z.string().optional(),
  }),

  // --- search_knowledge ---
  z.object({
    mode: z.literal("search_knowledge"),
    query: z.string().describe("Full-text search query"),
    workflow_scope: z
      .string()
      .optional()
      .describe("Limit to a specific workflow scope (omit for global)"),
    limit: z.number().default(10).describe("Max results"),
  }),

  // --- store_knowledge ---
  z.object({
    mode: z.literal("store_knowledge"),
    content: z.string().describe("Knowledge content to store"),
    source: z.string().describe("Source description (e.g. 'user', 'observation')"),
    workflow_scope: z
      .string()
      .optional()
      .describe("Workflow scope (omit for global knowledge)"),
    tags: z.string().optional().describe("Comma-separated tags"),
  }),

  // --- memory_get ---
  z.object({
    mode: z.literal("memory_get"),
    workflow_name: z.string().describe("Workflow name"),
  }),

  // --- memory_set ---
  z.object({
    mode: z.literal("memory_set"),
    workflow_name: z.string().describe("Workflow name"),
    content: z.string().describe("New working memory content"),
  }),

  // --- observe ---
  z.object({
    mode: z.literal("observe"),
    workflow_name: z.string().describe("Workflow this observation applies to"),
    content: z.string().describe("Observation content"),
    priority: z.enum(["high", "medium", "low"]).default("medium"),
    source_run_ids: z
      .array(z.number())
      .default([])
      .describe("Run IDs this observation came from"),
  }),

  // --- get_observations ---
  z.object({
    mode: z.literal("get_observations"),
    workflow_name: z.string().describe("Workflow name"),
    limit: z.number().default(50),
  }),

  // --- run_start ---
  z.object({
    mode: z.literal("run_start"),
    workflow_name: z.string().describe("Workflow name"),
    steps_total: z.number().describe("Expected total steps"),
  }),

  // --- run_finish ---
  z.object({
    mode: z.literal("run_finish"),
    run_id: z.number().describe("Run ID from run_start"),
    status: z.enum(["completed", "failed"]).describe("Final run status"),
  }),

  // --- run_log_step ---
  z.object({
    mode: z.literal("run_log_step"),
    run_id: z.number().describe("Run ID from run_start"),
    step_index: z.number(),
    step_id: z.string(),
    action: z.string().describe("JSON-serialized action taken"),
    success: z.boolean(),
    confidence: z.number().min(0).max(1),
    context_snapshot: z.string().optional(),
    error: z.string().optional(),
  }),

  // --- run_history ---
  z.object({
    mode: z.literal("run_history"),
    limit: z.number().default(10),
  }),

  // --- run_steps ---
  z.object({
    mode: z.literal("run_steps"),
    run_id: z.number().describe("Run ID to get steps for"),
  }),

  // --- llm_complete ---
  z.object({
    mode: z.literal("llm_complete"),
    system_prompt: z.string().describe("System prompt for the LLM"),
    user_prompt: z.string().describe("User prompt / question"),
    max_tokens: z.number().default(4096).optional(),
  }),

  // --- llm_complete_with_image ---
  z.object({
    mode: z.literal("llm_complete_with_image"),
    system_prompt: z.string().describe("System prompt for the LLM"),
    user_prompt: z.string().describe("User prompt / question"),
    image_base64: z.string().describe("Base64-encoded PNG image"),
    max_tokens: z.number().default(4096).optional(),
  }),

  // --- eviction ---
  z.object({
    mode: z.literal("eviction"),
    run_retention_days: z.number().default(90),
    knowledge_retention_days: z.number().default(365),
  }),

  // --- run_goal: autonomous goal execution ---
  z.object({
    mode: z.literal("run_goal"),
    goal: z
      .string()
      .describe(
        "Natural language goal to achieve with delegated autonomy. CEL runs the full " +
          "see→plan→act loop internally and returns the result. " +
          "Use this only when you want CEL itself to own planning and execution; " +
          "if the host model can already reason step-by-step, prefer cel_see + cel_act " +
          "for better efficiency and fewer internal LLM calls. " +
          "Example: 'Open Finder and search for a file called passport'",
      ),
    max_steps: z.number().default(30).describe("Maximum iterations before giving up"),
    timeout_ms: z
      .number()
      .default(120000)
      .describe("Total timeout in milliseconds"),
    enable_vision: z
      .boolean()
      .default(true)
      .describe("Use screenshots when accessibility tree is sparse"),
    self_heal: z
      .boolean()
      .default(true)
      .describe("Re-plan on action failures"),
    context_lazy: z
      .boolean()
      .default(true)
      .describe(
        "Enable context-lazy planning: the planner decides how much context each step needs. " +
          "Blind actions (keyboard shortcuts, typing) skip the slow accessibility tree walk entirely.",
      ),
    decompose: z
      .boolean()
      .default(false)
      .describe(
        "Use the orchestrator to decompose the goal into sub-tasks. " +
          "Best for complex multi-step goals. Uses Gemini Flash for decomposition (cheap). " +
          "Enables replanning on failure — the orchestrator tries a different approach if a sub-task fails. " +
          "Avoid for straightforward browser tasks when the host model can directly drive cel_see/cel_act.",
      ),
    workflow_name: z
      .string()
      .optional()
      .describe(
        "Workflow name for history scoping. When provided, enables: " +
          "history-informed planning (learns from past runs), observation storage, " +
          "and working memory persistence across runs.",
      ),
    enable_notebook: z
      .boolean()
      .default(true)
      .describe(
        "Enable notebook for cross-replan data persistence. " +
          "The notebook records data discovered during execution (prices, URLs, " +
          "confirmation numbers) that persists across strategy changes.",
      ),
  }),
]);

type Input = z.infer<typeof celThinkSchema>;

function isLikelyBrowserGoal(goal: string): boolean {
  const lower = goal.toLowerCase();
  return [
    "http://",
    "https://",
    "browser",
    "chrome",
    "chromium",
    "arc ",
    "brave",
    "edge",
    "gmail",
    "google ",
    "youtube",
    "search for",
    "open website",
    "web page",
    "news.google",
  ].some((needle) => lower.includes(needle));
}

function firstUrlInText(text: string): string | null {
  for (const token of text.split(/\s+/)) {
    const trimmed = token.trim().replace(/^[("'[]+|[)\]"',.]+$/g, "");
    if (trimmed.startsWith("http://") || trimmed.startsWith("https://")) {
      return trimmed;
    }
  }
  return null;
}

function focusBrowserApp(cel: Cel): void {
  const activateApp = (cel as any).activateApp?.bind(cel);
  if (!activateApp) return;

  const candidates = [
    "Google Chrome",
    "Chromium",
    "Brave Browser",
    "Microsoft Edge",
    "Arc",
    "Safari",
  ];

  for (const appName of candidates) {
    try {
      if (activateApp(appName)) {
        return;
      }
    } catch {
      // Best effort only — try next browser candidate.
    }
  }
}

export async function handleCelThink(cel: Cel, args: Input) {
  try {
    switch (args.mode) {
      case "plan": {
        const ctx = cel.getContext();
        const step = await cel.planStep(args.goal, ctx, args.history as PlannerStepRecord[], {
          maxSteps: args.max_steps,
          loopWarning: args.loop_warning,
        });
        return textResult(step);
      }

      case "plan_with_vision": {
        const ctx = cel.getContext();
        const screenshot = cel.captureScreen();
        const step = await cel.planStepWithVision(
          args.goal,
          ctx,
          screenshot.toString("base64"),
          args.history as PlannerStepRecord[],
          {
            maxSteps: args.max_steps,
            loopWarning: args.loop_warning,
          },
        );
        return textResult(step);
      }

      case "search_knowledge": {
        const results = cel.searchKnowledge(
          args.query,
          args.workflow_scope,
          args.limit,
        );
        return textResult(results);
      }

      case "store_knowledge": {
        const id = cel.addScopedKnowledge(
          args.content,
          args.source,
          args.workflow_scope,
          args.tags,
        );
        return textResult({ success: true, knowledge_id: id });
      }

      case "memory_get": {
        const memory = cel.getWorkingMemory(args.workflow_name);
        return textResult({ workflow_name: args.workflow_name, content: memory || "(empty)" });
      }

      case "memory_set": {
        cel.updateWorkingMemory(args.workflow_name, args.content);
        return textResult({ success: true, action: "updated" });
      }

      case "observe": {
        const id = cel.addObservation(
          args.workflow_name,
          args.content,
          args.priority,
          args.source_run_ids,
        );
        return textResult({ success: true, observation_id: id });
      }

      case "get_observations": {
        const observations = cel.getObservations(args.workflow_name, args.limit);
        return textResult(observations);
      }

      case "run_start": {
        const runId = cel.startRun(args.workflow_name, args.steps_total);
        return textResult({ run_id: runId });
      }

      case "run_finish": {
        cel.finishRun(args.run_id, args.status);
        return textResult({ success: true });
      }

      case "run_log_step": {
        const stepRowId = cel.logStep(
          args.run_id,
          args.step_index,
          args.step_id,
          args.action,
          args.success,
          args.confidence,
          args.context_snapshot,
          args.error,
        );
        return textResult({ step_row_id: stepRowId });
      }

      case "run_history": {
        const runs = cel.getRunHistory(args.limit);
        return textResult(runs);
      }

      case "run_steps": {
        const steps = cel.getStepResults(args.run_id);
        return textResult(steps);
      }

      case "llm_complete": {
        const response = await cel.llmComplete(
          args.system_prompt,
          args.user_prompt,
          args.max_tokens ?? undefined,
        );
        return textResult({ response });
      }

      case "llm_complete_with_image": {
        const response = await cel.llmCompleteWithImage(
          args.system_prompt,
          args.image_base64,
          args.user_prompt,
          args.max_tokens ?? undefined,
        );
        return textResult({ response });
      }

      case "eviction": {
        const result = cel.runEviction(
          args.run_retention_days,
          args.knowledge_retention_days,
        );
        return textResult(result);
      }

      case "run_goal": {
        const plannerGoal = args.goal.trim();

        const browserGoal = isLikelyBrowserGoal(args.goal);

        if (browserGoal) {
          await ensureCdpChrome(cel);
          focusBrowserApp(cel);
          await new Promise((resolve) => setTimeout(resolve, 1500));
        }

        // Ensure Cortex is running — it discovers and manages adapters automatically.
        // The browser adapter process driver activates when Chrome is frontmost.
        if (!cel.isCortexRunning()) {
          cel.bootCortex();
          if (browserGoal) {
            await new Promise((resolve) => setTimeout(resolve, 1500));
          }
        }

        // All goals go through the Rust runner.
        // The Cortex handles perception (200ms tick, adapter context fusion)
        // and execution dispatch (routes actions to the owning adapter).
        const rustResult = await cel.runGoalRust({
          goal: plannerGoal,
          max_steps: args.max_steps,
          timeout_ms: args.timeout_ms,
          enable_vision: args.enable_vision,
          self_heal: args.self_heal,
          enable_decomposition: args.decompose ?? false,
          enable_notebook: args.enable_notebook,
          workflow_name: args.workflow_name ?? null,
          constrain_to_url: firstUrlInText(args.goal),
        });
        return textResult(typeof rustResult === "string" ? JSON.parse(rustResult) : rustResult);
      }
    }
  } catch (err) {
    return errorResult(err instanceof Error ? err.message : String(err));
  }
}
