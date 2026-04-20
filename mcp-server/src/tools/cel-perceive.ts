import { z } from "zod";
import type { Cel } from "@cellar/agent";
import { normalizeCortexAnomalies, normalizeCortexModel } from "@cellar/agent";
import { textResult, errorResult, sleep } from "./shared.js";

// ─── Schema ─────────────────────────────────────────────────────────────────

export const celPerceiveSchema = z.discriminatedUnion("mode", [
  z.object({
    mode: z.literal("start"),
    goal: z.string().describe("The goal this perception session is tracking"),
    enable_suggestions: z.boolean().optional()
      .describe("Generate LLM next-action suggestions on read. Default true"),
  }),
  z.object({ mode: z.literal("stop") }),
  z.object({ mode: z.literal("read") }),
  z.object({
    mode: z.literal("feed"),
    action: z.string().describe("Description of the action taken"),
    target: z.string().optional().describe("Element ID or description of the action target"),
    expected: z.string().optional().describe("Expected outcome"),
  }),
  z.object({
    mode: z.literal("configure"),
    goal: z.string().optional().describe("Updated goal"),
    enable_suggestions: z.boolean().optional().describe("Toggle LLM suggestions"),
  }),
  z.object({
    mode: z.literal("checkpoint"),
    summary: z.string().describe("Summary of what was accomplished so far"),
  }),
  z.object({ mode: z.literal("status") }),
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

        const summary = {
          totalActions: session.actionLog.length,
          successfulActions,
          failedActions,
          totalReads: session.totalReads,
          durationMs: Date.now() - session.startTime,
          goal: session.goal,
          observations: session.observations,
          constraints: session.constraints.map((c) => ({ text: c.text, kind: c.kind, satisfied: c.satisfied })),
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

        // Reset rolling state
        session.observations = [`[Checkpoint ${entry.id}] ${args.summary}`];
        session.actionLog = [];

        return textResult({ success: true, checkpoint_id: entry.id });
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
    }
  } catch (err) {
    return errorResult(err instanceof Error ? err.message : String(err));
  }
}
