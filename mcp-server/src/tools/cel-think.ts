import { z } from "zod";
import type { Cel, PlannerStepRecord, PlannedAction, ScreenContext } from "@cellar/agent/runtime";
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
    // MCP transports that auto-encode object literals (notably the wire layer
    // in some hosts) deliver `action` as a JSON object even when callers pass
    // a string. Accept either shape and coerce to a string before persisting,
    // so the agent gets a clear contract instead of a zod parse error.
    action: z
      .union([z.string(), z.record(z.unknown())])
      .transform((v) => (typeof v === "string" ? v : JSON.stringify(v)))
      .describe("Action taken — either a string or an object (will be JSON-stringified)"),
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

  // --- cortex memory (PR2): explicit, workflow-scoped, opt-in ---
  z.object({
    mode: z.literal("store_memory"),
    workflow_id: z
      .string()
      .describe(
        "Workflow this memory belongs to. Required — there is no global memory scope in v1. " +
          "Memories are only surfaced for the same workflow.",
      ),
    kind: z
      .enum(["outcome", "prior", "failure", "preference"])
      .describe(
        "Memory kind: " +
          "'outcome' (what happened — replayable action + result), " +
          "'prior' (a generalisation), " +
          "'failure' (something to avoid + workaround), " +
          "'preference' (user preference informing future planning).",
      ),
    content: z
      .unknown()
      .describe(
        "Structured payload — shape depends on `kind`. Free-form JSON; the cortex selector reads " +
          "from `summary` for the catalog and from `content` for hydration.",
      ),
    summary: z
      .string()
      .optional()
      .describe("One-line summary the selector uses to decide whether to hydrate this memory."),
    tags: z.array(z.string()).optional().describe("Optional tags for retrieval."),
    source_ref: z
      .string()
      .optional()
      .describe("Optional back-reference (transcript span id, checkpoint id, adapter fact id)."),
  }),
  z.object({
    mode: z.literal("search_memory"),
    workflow_id: z.string().describe("Workflow scope — memories outside this workflow are not searched."),
    query: z
      .string()
      .describe("Free-text query. Case-insensitive substring match over summary + content (v1)."),
    limit: z
      .number()
      .default(20)
      .describe("Max number of results, most-recent-first. Default 20."),
  }),
  z.object({
    mode: z.literal("prune_memory"),
    threshold: z
      .number()
      .default(0.01)
      .describe(
        "Decay-score threshold (0.0..1.0). Memories below this are deleted. " +
          "Default 0.01 cuts at ~20 months given the 90-day half-life. " +
          "0.5 cuts at ~3 months; 0.125 cuts at ~9 months. " +
          "Pass 0.0 for a no-op.",
      ),
  }),

  // --- run_goal: autonomous goal execution via the canonical agent ---
  // Only budget limits are tunable — `enable_vision`, `self_heal`,
  // `decompose`, `enable_notebook`, `workflow_name`, etc. are no longer
  // knobs (see docs/canonical-agent-plan.md). The canonical loop is one
  // shape for every caller; varying it per-invocation was the main
  // source of CLI-vs-eval drift.
  z.object({
    mode: z.literal("run_goal"),
    goal: z
      .string()
      .describe(
        "Natural language goal. The canonical agent runs a single " +
          "perceive → plan → sub_goals → steps loop with 3-strike retry. " +
          "Prefer cel_see + cel_act when the host model can already reason " +
          "step-by-step; use run_goal only to delegate the whole loop.",
      ),
    max_steps: z.number().default(80).describe("Total step budget across sub-goals"),
    timeout_ms: z
      .number()
      .default(900_000)
      .describe("Total wall-clock timeout in milliseconds"),
  }),
]);

type Input = z.infer<typeof celThinkSchema>;

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

      case "store_memory": {
        const id = cel.cortexMemoryInsert({
          workflow_id: args.workflow_id,
          kind: args.kind,
          content: args.content,
          summary: args.summary,
          tags: args.tags,
          source_ref: args.source_ref,
        });
        return textResult({ id });
      }

      case "search_memory": {
        const memories = cel.cortexMemorySearch(args.workflow_id, args.query, args.limit);
        return textResult({ memories, count: memories.length });
      }

      case "prune_memory": {
        const deleted = cel.cortexMemoryPrune(args.threshold);
        return textResult({ deleted });
      }

      case "run_goal": {
        // Canonical path: the MCP server is a thin shim over
        // CanonicalGoalRunner::run. No legacy flags, no per-invocation
        // routing decisions — the canonical agent is one shape for
        // every caller. See docs/canonical-agent-plan.md.
        if (!cel.isCortexRunning()) {
          cel.bootCortex();
        }
        // Always ensure the dedicated CDP browser. Cheap no-op when it's
        // already running. Prevents `connect_to_focused_app` from latching
        // onto Safari or whatever's frontmost when the goal is browser-ish
        // but doesn't name a URL.
        await ensureCdpChrome(cel);
        const rustResult = await cel.runGoalRust({
          goal: args.goal.trim(),
          max_steps: args.max_steps,
          timeout_ms: args.timeout_ms,
        });
        return textResult(typeof rustResult === "string" ? JSON.parse(rustResult) : rustResult);
      }
    }
  } catch (err) {
    return errorResult(err instanceof Error ? err.message : String(err));
  }
}
