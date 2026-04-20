/**
 * Perception Socket — Thin wrapper around the Cortex for MCP consumption.
 *
 * The PerceptionSession is now just an interface layer that boots/shuts down
 * the Cortex and provides shaped results for the MCP tool. All actual
 * perception happens in the Cortex background loop.
 */

import type { ContextProvider } from "./interfaces/context-provider.js";
import type { EventSource } from "./interfaces/event-source.js";
import type { Planner } from "./interfaces/planner.js";

/** Minimal CEL capability set needed by PerceptionSession. */
type PerceptionDeps = ContextProvider & EventSource & Planner;
import type {
  PerceptionConfig,
  MentalModel,
  Anomaly,
  PulseResult,
  FeedResult,
  PerceptionSummary,
  ActionEntry,
  CheckpointEntry,
  CelEvent,
} from "./types.js";
import { Cortex, isCortexActive, getActiveCortex } from "./cortex.js";
import { extractConstraints, checkConstraintSatisfaction, type Constraint } from "./constraint-extractor.js";

// Re-export cortex utilities for MCP tools
export { isCortexActive, getActiveCortex };

// ─── Backwards compatibility ────────────────────────────────────────────────

/** @deprecated Use isCortexActive() instead. */
export function isPerceptionSessionActive(): boolean {
  return isCortexActive();
}

// ─── PerceptionSession ─────────────────────────────────────────────────────

export class PerceptionSession {
  private cel: PerceptionDeps;
  private cortex: Cortex | null = null;
  private goal: string;
  private enableSuggestions: boolean;
  private actionLog: ActionEntry[] = [];
  private observations: string[] = [];
  private checkpoints: CheckpointEntry[] = [];
  private constraints: Constraint[] = [];
  private startTime = 0;
  private readCount = 0;
  private active = false;

  constructor(cel: PerceptionDeps, config: PerceptionConfig) {
    this.cel = cel;
    this.goal = config.goal;
    this.enableSuggestions = config.enableSuggestions ?? true;
  }

  /** Start — boots the cortex. */
  async start(): Promise<{
    success: boolean;
    initialContext: { app: string; window: string; elementCount: number };
    constraints?: Array<{ text: string; kind: string; satisfied: boolean }>;
  }> {
    if (this.active) {
      throw new Error("Session already active.");
    }

    this.cortex = new Cortex(this.cel);
    await this.cortex.boot();
    this.startTime = Date.now();
    this.active = true;

    // Extract constraints from goal (non-blocking — fallback on error)
    try {
      this.constraints = await extractConstraints(this.cel, this.goal);
    } catch {
      this.constraints = [{ text: this.goal, kind: "action", satisfied: false }];
    }

    const ctx = this.cortex.model.currentContext;
    return {
      success: true,
      initialContext: {
        app: ctx.app,
        window: ctx.window,
        elementCount: ctx.elements.length,
      },
      constraints: this.constraints.map((c) => ({ text: c.text, kind: c.kind, satisfied: c.satisfied })),
    };
  }

  /** Stop — shuts down the cortex. */
  stop(): PerceptionSummary {
    if (!this.active || !this.cortex) {
      throw new Error("No active session to stop.");
    }

    const model = this.cortex.model;
    this.cortex.shutdown();
    this.cortex = null;
    this.active = false;

    const successful = this.actionLog.filter((a) => a.landed).length;
    const failed = this.actionLog.filter((a) => !a.landed).length;

    return {
      totalActions: this.actionLog.length,
      successfulActions: successful,
      failedActions: failed,
      totalAnomalies: model.anomalyQueue.length,
      totalPulses: this.readCount,
      durationMs: Date.now() - this.startTime,
      observations: this.observations,
    };
  }

  /**
   * Read — returns the mental model snapshot.
   * This does NOT trigger a new observation. The model is already current.
   */
  async read(): Promise<PulseResult> {
    if (!this.active || !this.cortex) {
      throw new Error("No active session. Call start first.");
    }

    this.readCount++;
    const model = this.cortex.model;

    // Compute context summary from the already-current model
    let actionableCount = 0;
    for (const el of model.currentContext.elements) {
      if (el.state?.enabled && el.state?.visible && (el.actions?.length ?? 0) > 0) {
        actionableCount++;
      }
    }

    // Drain anomalies
    const anomalies = this.cortex.consumeAnomalies();

    // Latest diff (most recent from rolling window)
    const latestDiff = model.recentDiffs.length > 0
      ? model.recentDiffs[model.recentDiffs.length - 1]
      : null;

    // LLM suggestion (optional)
    let suggestion: string | null = null;
    if (this.enableSuggestions) {
      try {
        const contextStr = `App: ${model.currentContext.app}, Window: ${model.currentContext.window}, ` +
          `Elements: ${model.currentContext.elements.length} (${actionableCount} actionable), ` +
          `Confidence: ${model.confidence.toFixed(2)}` +
          (model.focusedElement ? `, Focused: ${model.focusedElement.label ?? model.focusedElement.id}` : "");

        const temporalStr = [
          model.temporal.loading?.detected ? `Loading for ${model.temporal.loading.durationMs}ms` : null,
          model.temporal.errorPersisting?.detected ? `Error persisting: "${model.temporal.errorPersisting.message}"` : null,
          model.temporal.idleSince ? `Idle for ${Date.now() - model.temporal.idleSince}ms` : null,
          model.temporal.focusTrail.length > 2 ? `Focus trail: ${model.temporal.focusTrail.slice(-5).join(" → ")}` : null,
        ].filter(Boolean).join("; ");

        const diffStr = latestDiff
          ? `Changes: +${latestDiff.addedCount} -${latestDiff.removedCount} ~${latestDiff.changedCount}`
          : "No recent changes";

        const prompt = [
          `Goal: ${this.goal}`,
          contextStr,
          diffStr,
          temporalStr || "No temporal patterns",
          anomalies.length > 0 ? `Anomalies: ${anomalies.map((a) => a.description).join("; ")}` : "",
          `Actions taken: ${this.actionLog.length} (${this.actionLog.filter((a) => a.landed).length} landed)`,
          "",
          "What should be done next? 1 sentence. Address anomalies first.",
        ].filter(Boolean).join("\n");

        suggestion = await this.cel.llmComplete(
          "You are a desktop automation assistant. Given the current mental model and goal, suggest the next action.",
          prompt,
          256,
        );
      } catch {
        suggestion = null;
      }
    }

    return {
      contextSummary: {
        app: model.currentContext.app,
        window: model.currentContext.window,
        elementCount: model.currentContext.elements.length,
        actionableCount,
        focusedElement: model.focusedElement?.label ?? model.focusedElement?.id,
      },
      diff: latestDiff,
      anomalies,
      goalState: {
        goal: this.goal,
        currentAction: null,
        actionsCompleted: this.actionLog.filter((a) => a.landed).length,
        actionsFailed: this.actionLog.filter((a) => !a.landed).length,
      },
      suggestion,
      screenshotNeeded: model.visionNeeded,
      events: [], // events are processed internally by cortex
      constraints: this.constraints.length > 0
        ? this.constraints.map((c) => ({ text: c.text, kind: c.kind, satisfied: c.satisfied }))
        : undefined,
      checkpointSummary: this.getCheckpointSummary() || undefined,
    } as PulseResult;
  }

  /** Feed — report an action and check if it landed. */
  async feed(action: string, target?: string, expected?: string): Promise<FeedResult> {
    if (!this.active || !this.cortex) {
      throw new Error("No active session. Call start first.");
    }

    const preContext = this.cortex.model.currentContext;

    // Notify cortex — forces fresh context on next tick
    this.cortex.notifyAction(action, target);

    // Wait for cortex to process the post-action state (a few ticks)
    await new Promise((resolve) => setTimeout(resolve, TICK_INTERVAL_MS * 3));

    const postModel = this.cortex.model;
    const latestDiff = postModel.recentDiffs.length > 0
      ? postModel.recentDiffs[postModel.recentDiffs.length - 1]
      : null;

    const actionLanded = latestDiff !== null &&
      (latestDiff.addedCount > 0 || latestDiff.changedCount > 0 || latestDiff.removedCount > 0);

    if (actionLanded) {
      this.cortex.reportActionSuccess();
    } else {
      this.cortex.reportActionFailure();
    }

    // Drain anomalies
    const anomalies = this.cortex.consumeAnomalies();

    // Log the action
    this.actionLog.push({
      action,
      target,
      expected,
      timestamp: Date.now(),
      landed: actionLanded,
    });

    // Record observation
    const obs = actionLanded && latestDiff
      ? `Action "${action}" landed: +${latestDiff.addedCount} -${latestDiff.removedCount} ~${latestDiff.changedCount}`
      : `Action "${action}" did NOT produce visible changes`;
    this.observations.push(obs);

    // Check constraint satisfaction
    if (this.constraints.length > 0) {
      checkConstraintSatisfaction(this.constraints, action, obs);
    }

    return {
      actionLanded,
      diff: latestDiff,
      nextFocusedElement: postModel.focusedElement?.label ?? postModel.focusedElement?.id,
      anomalies,
    };
  }

  /**
   * Checkpoint — summarize completed work and reset working history.
   * Inspired by WebTactix's "partially_done" mechanism.
   * After a checkpoint, read() only shows post-checkpoint actions,
   * prepended with the checkpoint summary chain.
   */
  checkpoint(summary: string): { success: boolean; checkpoint_id: number } {
    if (!this.active) {
      throw new Error("No active session. Call start first.");
    }

    const entry: CheckpointEntry = {
      id: this.checkpoints.length,
      summary,
      timestamp: Date.now(),
      actionsBeforeCheckpoint: this.actionLog.length,
    };
    this.checkpoints.push(entry);

    // Reset rolling state — observations restart from checkpoint summary
    this.observations = [`[Checkpoint ${entry.id}] ${summary}`];
    this.actionLog = [];

    return { success: true, checkpoint_id: entry.id };
  }

  /** Get checkpoint history (summaries of all completed checkpoints). */
  getCheckpointSummary(): string {
    if (this.checkpoints.length === 0) return "";
    return this.checkpoints
      .map((c) => `[Checkpoint ${c.id}] ${c.summary}`)
      .join(" → ");
  }

  /** Configure — update goal mid-session. */
  configure(updates: Partial<PerceptionConfig>): { success: boolean } {
    if (!this.active) {
      throw new Error("No active session. Call start first.");
    }

    if (updates.goal !== undefined) {
      this.goal = updates.goal;
      this.observations.push(`Goal updated to: "${updates.goal}"`);
    }
    if (updates.enableSuggestions !== undefined) {
      this.enableSuggestions = updates.enableSuggestions;
    }

    return { success: true };
  }

  /** Check if this session is active. */
  isActive(): boolean {
    return this.active;
  }
}

// ─── Internal constant for feed timing ──────────────────────────────────────
const TICK_INTERVAL_MS = 200;
