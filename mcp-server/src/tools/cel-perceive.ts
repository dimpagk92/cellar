import { z } from "zod";
import type { Cel } from "@cellar/agent";
import { normalizeCortexAnomalies, normalizeCortexModel } from "@cellar/agent";
import { textResult, errorResult, sleep, axPermissionGuard } from "./shared.js";
import { getFrontmost } from "../helpers/focus.js";

// ─── Schema ─────────────────────────────────────────────────────────────────

export const celPerceiveSchema = z.discriminatedUnion("mode", [
  z.object({
    mode: z.literal("start"),
    goal: z.string().describe(
      "Natural-language goal driving this session. Cortex parses it into independently-checkable " +
      "constraints (factual / action / navigation / verification) and uses it as the target for " +
      "suggestion generation. Be specific — vague goals produce vague suggestions.",
    ),
    enable_suggestions: z.boolean().optional().describe(
      "When true (default), each `read` returns LLM-generated next-action recommendations grounded " +
      "in the current cortex model. Adds one LLM call per read (latency + cost). Disable for " +
      "passive monitoring or when the host already plans next actions.",
    ),
    enable_memory: z.boolean().optional().describe(
      "When true (default false), the session writes a checkpoint memory on every `cel_perceive " +
      "checkpoint` call and a final outcome memory on `cel_perceive stop`. Requires `workflow_id`. " +
      "Memories are durable, workflow-scoped, and surface in future sessions via the cortex " +
      "selector (PR3). Off by default for privacy — opt-in only.",
    ),
    workflow_id: z.string().optional().describe(
      "Required when `enable_memory: true`. Workflow scope for any memory writes — there is no " +
      "global memory scope in v1. Use a stable, human-readable identifier (e.g. 'concur-expense', " +
      "'morning-standup-prep') so the cortex selector can recall across sessions.",
    ),
  }),
  z.object({ mode: z.literal("stop") }),
  z.object({ mode: z.literal("read") }),
  z.object({
    mode: z.literal("feed"),
    action: z.string().describe(
      "What you just executed, in plain language (e.g. 'click Submit button', 'type \"hello\" into " +
      "search field'). Cortex uses this to attribute screen changes to the action and to detect " +
      "side effects vs. expected outcomes. Free-form — verb-first phrasing works best.",
    ),
    target: z.string().optional().describe(
      "Element id from a prior `cel_see` snapshot or a human description of what was acted on. " +
      "Used to correlate the action with the relevant region of the cortex model. Omit only if " +
      "the action has no specific target (e.g. global keyboard shortcut).",
    ),
    expected: z.string().optional().describe(
      "What you expected to happen (e.g. 'modal closes', 'page navigates to /dashboard'). " +
      "Cortex compares this against the diffed model after the screen settles and flags " +
      "discrepancies (anomalies, side effects, action did not land). Omit if you have no " +
      "specific expectation.",
    ),
  }),
  z.object({
    mode: z.literal("configure"),
    goal: z.string().optional().describe(
      "Replace the session goal mid-flight. Triggers re-extraction of constraints from the new " +
      "goal — prior progress on satisfied constraints is preserved where the new constraints " +
      "match by text. Use when the user pivots scope; otherwise keep the original goal.",
    ),
    enable_suggestions: z.boolean().optional().describe(
      "Turn LLM next-action suggestions on or off without ending the session. Useful to silence " +
      "suggestion costs during long passive observation, then re-enable when planning resumes.",
    ),
  }),
  z.object({
    mode: z.literal("checkpoint"),
    summary: z.string().describe(
      "One-paragraph summary of progress so far — what was accomplished, what's still pending. " +
      "Stored in the session log with a timestamp and the action count, surfaced on subsequent " +
      "`read` calls to anchor the LLM's understanding of session history. Write it as you would " +
      "write a status update to a colleague.",
    ),
  }),
  z.object({ mode: z.literal("status") }),
  z.object({
    mode: z.literal("plan_view"),
    goal: z.string().describe(
      "Natural-language goal the planning view should be built around. The cortex selector " +
      "uses this to score elements by relevance and to fold goal-relevant memories / knowledge / " +
      "events (when those land in PR3). Be specific.",
    ),
    budget: z
      .object({
        max_tokens: z.number().optional(),
        max_elements: z.number().optional(),
        max_memories: z.number().optional(),
        max_adapter_facts: z.number().optional(),
      })
      .optional()
      .describe(
        "Optional ceilings the cortex selector enforces — most-relevant items first, drop the rest. " +
        "When omitted, defaults sized to keep prompts under common LLM context windows. Override " +
        "per-call when your model has a larger context window or you're running a token-tight harness.",
      ),
  }),
]);

type Input = z.infer<typeof celPerceiveSchema>;

// ─── Constraint types ───────────────────────────────────────────────────────

interface Constraint {
  text: string;
  kind: "factual" | "action" | "navigation" | "verification";
  satisfied: boolean;
}

// ─── Session state ──────────────────────────────────────────────────────────

interface ActionEntry {
  action: string;
  target?: string;
  expected?: string;
  timestamp: number;
  landed: boolean;
}

interface CheckpointEntry {
  id: number;
  summary: string;
  timestamp: number;
  actionsBeforeCheckpoint: number;
}

interface SessionState {
  goal: string;
  enableSuggestions: boolean;
  startTime: number;
  actionLog: ActionEntry[];
  observations: string[];
  checkpoints: CheckpointEntry[];
  constraints: Constraint[];
  totalReads: number;
  /** PR2 opt-in: when set, checkpoint + stop write a cortex memory under this workflow_id. */
  memoryWorkflowId?: string;
}

let session: SessionState | null = null;

// ─── Constraint helpers ─────────────────────────────────────────────────────

async function extractConstraints(cel: Cel, goal: string): Promise<Constraint[]> {
  try {
    const prompt = `Break this automation goal into explicit, independently checkable requirements.

Goal: "${goal}"

Return a JSON array (1-5 items) of objects with:
- "text": a short, specific requirement
- "kind": one of "factual", "action", "navigation", "verification"

Rules: Each must be independently verifiable. Max 5. JSON only, no markdown fences.`;

    const raw = await cel.llmComplete(
      "You extract checkable requirements from automation goals. Return valid JSON only.",
      prompt,
      512,
    );

    const cleaned = raw.replace(/```json?\n?/g, "").replace(/```/g, "").trim();
    const parsed = JSON.parse(cleaned) as Array<{ text: string; kind: string }>;

    return parsed.slice(0, 5).map((c) => ({
      text: c.text,
      kind: (["factual", "action", "navigation", "verification"].includes(c.kind)
        ? c.kind : "action") as Constraint["kind"],
      satisfied: false,
    }));
  } catch {
    return [{ text: goal, kind: "action", satisfied: false }];
  }
}

function checkConstraintSatisfaction(
  constraints: Constraint[],
  actionDescription: string,
  observation: string,
): number {
  let newlySatisfied = 0;
  const combined = `${actionDescription} ${observation}`.toLowerCase();

  for (const constraint of constraints) {
    if (constraint.satisfied) continue;
    const keywords = constraint.text.toLowerCase().replace(/[^a-z0-9\s]/g, " ")
      .split(/\s+/).filter((w) => w.length > 3);
    const matchCount = keywords.filter((kw) => combined.includes(kw)).length;
    if (keywords.length > 0 && matchCount / keywords.length >= 0.6) {
      constraint.satisfied = true;
      newlySatisfied++;
    }
  }
  return newlySatisfied;
}

// ─── Handler ────────────────────────────────────────────────────────────────

export async function handleCelPerceive(cel: Cel, args: Input) {
  const denied = axPermissionGuard(cel);
  if (denied) return denied;
  try {
    switch (args.mode) {
      case "start": {
        if (session && cel.isCortexRunning()) {
          return errorResult("A perception session is already active. Call stop first.");
        }

        // Boot the Rust Cortex if not already running
        if (!cel.isCortexRunning()) {
          cel.bootCortex();
        }

        const goal = args.goal;
        const enableSuggestions = args.enable_suggestions ?? true;
        const enableMemory = args.enable_memory ?? false;
        if (enableMemory && !args.workflow_id) {
          return errorResult(
            "enable_memory: true requires `workflow_id`. There's no global memory scope " +
              "in v1 — pick a stable identifier (e.g. 'concur-expense') so memories scope correctly.",
          );
        }

        // Extract constraints from goal via LLM (non-blocking fallback on error)
        const constraints = await extractConstraints(cel, goal);

        session = {
          goal,
          enableSuggestions,
          startTime: Date.now(),
          actionLog: [],
          observations: [],
          checkpoints: [],
          constraints,
          totalReads: 0,
          memoryWorkflowId: enableMemory ? args.workflow_id : undefined,
        };

        const model = normalizeCortexModel(cel.readCortexModel());
        return textResult({
          success: true,
          initialContext: {
            app: model?.currentContext?.app ?? "",
            window: model?.currentContext?.window ?? "",
            elementCount: model?.currentContext?.elements?.length ?? 0,
          },
          constraints: constraints.map((c) => ({ text: c.text, kind: c.kind, satisfied: c.satisfied })),
        });
      }

      case "stop": {
        if (!session) {
          return errorResult("No active perception session to stop.");
        }

        const successfulActions = session.actionLog.filter((a) => a.landed).length;
        const failedActions = session.actionLog.filter((a) => !a.landed).length;
        const allConstraintsSatisfied = session.constraints.every((c) => c.satisfied);

        // PR2: opt-in final-outcome memory write. Captures the
        // workflow-level pass/fail so the selector can later answer
        // "what happened last time I tried this workflow?"
        let finalMemoryId: number | undefined;
        if (session.memoryWorkflowId) {
          try {
            finalMemoryId = cel.cortexMemoryInsert({
              workflow_id: session.memoryWorkflowId,
              kind: allConstraintsSatisfied ? "outcome" : "failure",
              content: {
                kind: allConstraintsSatisfied ? "outcome" : "failure",
                goal: session.goal,
                total_actions: session.actionLog.length,
                successful_actions: successfulActions,
                failed_actions: failedActions,
                checkpoints: session.checkpoints.length,
                duration_ms: Date.now() - session.startTime,
                constraints_satisfied: allConstraintsSatisfied,
                ts: new Date().toISOString(),
              },
              summary: allConstraintsSatisfied
                ? `Completed: ${session.goal}`
                : `Did not complete: ${session.goal}`,
              source_ref: `perceive:session:${session.startTime}`,
            });
          } catch (err) {
            session.observations.push(
              `[memory_write_failed] ${err instanceof Error ? err.message : String(err)}`,
            );
          }
        }

        const summary = {
          totalActions: session.actionLog.length,
          successfulActions,
          failedActions,
          totalReads: session.totalReads,
          durationMs: Date.now() - session.startTime,
          goal: session.goal,
          observations: session.observations,
          constraints: session.constraints.map((c) => ({ text: c.text, kind: c.kind, satisfied: c.satisfied })),
          memory_id: finalMemoryId ?? null,
        };

        cel.stopCortex();
        session = null;
        return textResult(summary);
      }

      case "read": {
        if (!session || !cel.isCortexRunning()) {
          return errorResult("No active perception session. Call start first.");
        }
        session.totalReads++;

        const model = normalizeCortexModel(cel.readCortexModel());
        if (!model) return errorResult("Failed to read Cortex model");

        const ctx = model.currentContext;
        const actionableCount = (ctx?.elements ?? []).filter(
          (el: any) => el.state?.enabled && el.state?.visible && (el.actions?.length ?? 0) > 0,
        ).length;

        const anomalies = normalizeCortexAnomalies(cel.consumeCortexAnomalies());
        const recentDiffs = model.recentDiffs ?? [];
        const latestDiff = recentDiffs.length > 0 ? recentDiffs[recentDiffs.length - 1] : null;

        // LLM suggestion (optional)
        let suggestion: string | null = null;
        if (session.enableSuggestions) {
          try {
            const contextStr = `App: ${ctx?.app}, Window: ${ctx?.window}, ` +
              `Elements: ${ctx?.elements?.length} (${actionableCount} actionable), ` +
              `Confidence: ${model.confidence?.toFixed?.(2) ?? model.confidence}` +
              (model.focusedElement ? `, Focused: ${model.focusedElement.label ?? model.focusedElement.id}` : "");

            const temporal = model.temporal ?? {};
            const temporalStr = [
              temporal.loading?.detected ? `Loading for ${temporal.loading.durationMs}ms` : null,
              temporal.errorPersisting?.detected ? `Error persisting: "${temporal.errorPersisting.message}"` : null,
              temporal.idleSince ? `Idle for ${Date.now() - temporal.idleSince}ms` : null,
              temporal.focusTrail?.length > 2 ? `Focus trail: ${temporal.focusTrail.slice(-5).join(" → ")}` : null,
            ].filter(Boolean).join("; ");

            const diffStr = latestDiff
              ? `Changes: +${latestDiff.addedCount} -${latestDiff.removedCount} ~${latestDiff.changedCount}`
              : "No recent changes";

            const landed = session.actionLog.filter((a) => a.landed).length;
            const prompt = [
              `Goal: ${session.goal}`,
              contextStr,
              diffStr,
              temporalStr || "No temporal patterns",
              anomalies.length > 0 ? `Anomalies: ${(anomalies as any[]).map((a: any) => a.description).join("; ")}` : "",
              `Actions taken: ${session.actionLog.length} (${landed} landed)`,
              "",
              "What should be done next? 1 sentence. Address anomalies first.",
            ].filter(Boolean).join("\n");

            suggestion = await cel.llmComplete(
              "You are a desktop automation assistant. Given the current mental model and goal, suggest the next action.",
              prompt,
              256,
            );
          } catch {
            suggestion = null;
          }
        }

        // Checkpoint summary
        const checkpointSummary = session.checkpoints.length > 0
          ? session.checkpoints.map((c) => `[Checkpoint ${c.id}] ${c.summary}`).join(" → ")
          : undefined;

        return textResult({
          contextSummary: {
            app: ctx?.app ?? "",
            window: ctx?.window ?? "",
            elementCount: ctx?.elements?.length ?? 0,
            actionableCount,
            focusedElement: model.focusedElement,
          },
          diff: latestDiff,
          anomalies,
          goalState: {
            goal: session.goal,
            actionsCompleted: session.actionLog.filter((a) => a.landed).length,
            actionsFailed: session.actionLog.filter((a) => !a.landed).length,
          },
          suggestion,
          screenshotNeeded: model.visionNeeded ?? false,
          constraints: session.constraints.length > 0
            ? session.constraints.map((c) => ({ text: c.text, kind: c.kind, satisfied: c.satisfied }))
            : undefined,
          checkpointSummary,
        });
      }

      case "feed": {
        if (!session || !cel.isCortexRunning()) {
          return errorResult("No active perception session. Call start first.");
        }

        // Snapshot the cortex's tracked app + the system frontmost BEFORE
        // notifying the cortex. If the action didn't visibly change anything
        // and these two disagree, it's strong evidence the event landed in a
        // different window than the cortex was tracking — almost always a
        // focus-race symptom worth surfacing structurally.
        const preModel = normalizeCortexModel(cel.readCortexModel());
        const expectedApp = preModel?.currentContext?.app ?? null;
        let actualApp: string | null = null;
        try {
          actualApp = await getFrontmost();
        } catch {
          // osascript can fail in headless test envs — degrade silently.
        }

        // Notify the Rust Cortex
        cel.notifyCortexAction(args.action);

        // Wait for 3 ticks (600ms)
        await sleep(600);

        const postModel = normalizeCortexModel(cel.readCortexModel());
        const anomalies = normalizeCortexAnomalies(cel.consumeCortexAnomalies());

        const latestDiff = (postModel?.recentDiffs ?? []).slice(-1)[0] ?? null;
        const actionLanded = latestDiff
          ? (latestDiff.addedCount > 0 || latestDiff.changedCount > 0 || latestDiff.removedCount > 0)
          : false;

        // Build the wrong-app diagnostic. Only surface when the action
        // didn't visibly land AND the two app names disagree — agents
        // looking at `actionLanded: false` need a hint about why, but a
        // matching-app no-op (e.g. typed into a focused field that didn't
        // re-render) shouldn't pollute the response.
        const landedInWrongApp =
          !actionLanded && expectedApp && actualApp && expectedApp !== actualApp
            ? { expected: expectedApp, actual: actualApp }
            : undefined;

        // Log action
        session.actionLog.push({
          action: args.action,
          target: args.target,
          expected: args.expected,
          timestamp: Date.now(),
          landed: actionLanded,
        });

        // Record observation
        const obs = actionLanded && latestDiff
          ? `Action "${args.action}" landed: +${latestDiff.addedCount} -${latestDiff.removedCount} ~${latestDiff.changedCount}`
          : landedInWrongApp
            ? `Action "${args.action}" produced no diff in "${expectedApp}" — frontmost was "${actualApp}"`
            : `Action "${args.action}" did NOT produce visible changes`;
        session.observations.push(obs);

        // Check constraint satisfaction
        if (session.constraints.length > 0) {
          checkConstraintSatisfaction(session.constraints, args.action, obs);
        }

        if (actionLanded) {
          cel.reportCortexActionSuccess();
        } else {
          cel.reportCortexActionFailure();
        }

        return textResult({
          actionLanded,
          landedInWrongApp,
          diff: latestDiff,
          nextFocusedElement: postModel?.focusedElement,
          anomalies,
          constraints: session.constraints.length > 0
            ? session.constraints.map((c) => ({ text: c.text, kind: c.kind, satisfied: c.satisfied }))
            : undefined,
        });
      }

      case "configure": {
        if (!session) return errorResult("No active perception session. Call start first.");
        if (args.goal) {
          session.goal = args.goal;
          session.observations.push(`Goal updated to: "${args.goal}"`);
        }
        if (args.enable_suggestions !== undefined) {
          session.enableSuggestions = args.enable_suggestions;
        }
        return textResult({ success: true, goal: session.goal });
      }

      case "checkpoint": {
        if (!session) return errorResult("No active perception session. Call start first.");

        const entry: CheckpointEntry = {
          id: session.checkpoints.length,
          summary: args.summary,
          timestamp: Date.now(),
          actionsBeforeCheckpoint: session.actionLog.length,
        };
        session.checkpoints.push(entry);

        // PR2: opt-in cortex memory write. We persist a structured
        // outcome memory so the next session's selector (PR3) can
        // surface "what was accomplished by checkpoint N of this
        // workflow." Caller opted in via `cel_perceive start
        // { enable_memory: true, workflow_id: "..." }`.
        let memoryId: number | undefined;
        if (session.memoryWorkflowId) {
          try {
            memoryId = cel.cortexMemoryInsert({
              workflow_id: session.memoryWorkflowId,
              kind: "outcome",
              content: {
                kind: "outcome",
                checkpoint_id: entry.id,
                summary: args.summary,
                actions: entry.actionsBeforeCheckpoint,
                ts: new Date(entry.timestamp).toISOString(),
              },
              summary: `[checkpoint ${entry.id}] ${args.summary}`,
              source_ref: `perceive:checkpoint:${entry.id}`,
            });
          } catch (err) {
            // Don't fail the checkpoint just because memory write
            // failed — log it in the session observations and keep going.
            session.observations.push(
              `[memory_write_failed] ${err instanceof Error ? err.message : String(err)}`,
            );
          }
        }

        // Reset rolling state
        session.observations = [`[Checkpoint ${entry.id}] ${args.summary}`];
        session.actionLog = [];

        return textResult({ success: true, checkpoint_id: entry.id, memory_id: memoryId ?? null });
      }

      case "status": {
        if (!cel.isCortexRunning()) {
          return textResult({ active: false, message: "No cortex is running. Call start to boot one." });
        }

        const model = normalizeCortexModel(cel.readCortexModel());
        const ctx = model?.currentContext;
        const actionableCount = (ctx?.elements ?? []).filter(
          (el: any) => el.state?.enabled && el.state?.visible && (el.actions?.length ?? 0) > 0,
        ).length;

        return textResult({
          active: true,
          confidence: model?.confidence,
          visionNeeded: model?.visionNeeded,
          ageMs: model?.ageMs,
          cycleCount: model?.cycleCount,
          uptimeMs: model?.uptimeMs,
          app: ctx?.app,
          window: ctx?.window,
          elementCount: ctx?.elements?.length ?? 0,
          actionableCount,
          stableElements: model?.stability?.stable?.size ?? 0,
          volatileElements: model?.stability?.volatile?.size ?? 0,
          pendingAnomalies: model?.anomalyQueue?.length ?? 0,
          temporal: model?.temporal,
        });
      }

      case "plan_view": {
        // Standalone — does not require a perception session, but does
        // require a booted cortex (boot it on demand if needed).
        if (!cel.isCortexRunning()) {
          cel.bootCortex();
        }
        const view = await cel.canonicalBuildPlanningView(args.goal, {
          budget: args.budget
            ? {
                max_tokens: args.budget.max_tokens ?? 8000,
                max_elements: args.budget.max_elements ?? 80,
                max_memories: args.budget.max_memories ?? 8,
                max_adapter_facts: args.budget.max_adapter_facts ?? 12,
              }
            : undefined,
        });
        return textResult(view);
      }
    }
  } catch (err) {
    return errorResult(err instanceof Error ? err.message : String(err));
  }
}
