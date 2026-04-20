/**
 * Structured replan event emitter.
 *
 * Emits one JSON line per replan / tier-escalation event to stderr so
 * operators can grep, aggregate, and feed these into monitoring without
 * parsing the CognitiveTrail's prose. Complements the human-readable trail.
 *
 * Events share a common envelope:
 *   { lvl: "replan", goal_id, ts, type, ...typed-fields }
 *
 * Where type ∈ { "tier", "stall", "tier4_cap", "preflight" }.
 *
 * Usage:
 *   const emit = makeReplanEmitter(goalId);
 *   emit.tier({ tier: 2, reason: "reactive_failure", ... });
 *   emit.stall({ stepIndex, stepsSinceProgress });
 *
 * Emission is silent when CELLAR_REPLAN_EVENTS env var is unset. When set to
 * "1" or "stderr", events go to stderr. Callers can also subscribe directly
 * via onEvent() for in-process consumers (test assertions, UIs).
 */

import type { ReplanTier } from "./failure-recovery.js";

// ─── Event envelope ──────────────────────────────────────────────────────────

export type ReplanEventType = "tier" | "stall" | "tier4_cap" | "preflight";

export interface ReplanEventBase {
  lvl: "replan";
  goal_id: string;
  ts: number;
  type: ReplanEventType;
}

export interface TierEvent extends ReplanEventBase {
  type: "tier";
  tier: ReplanTier;
  reason: "wrong_approach" | "reactive_failure";
  step_index: number;
  milestone: string;
  consecutive_failures: number;
  failed_strategies_count: number;
  backtracked: boolean;
  needs_redecompose: boolean;
}

export interface StallEvent extends ReplanEventBase {
  type: "stall";
  step_index: number;
  steps_since_progress: number;
}

export interface Tier4CapEvent extends ReplanEventBase {
  type: "tier4_cap";
  step_index: number;
  attempts: number;
}

export interface PreflightEvent extends ReplanEventBase {
  type: "preflight";
  stage: "history" | "feasibility" | "decompose";
  /** ms elapsed in this stage */
  ms: number;
  /** True if the stage fired; false when gated off / skipped */
  fired: boolean;
  /** Optional stage-specific fact. */
  note?: string;
}

export type ReplanEvent = TierEvent | StallEvent | Tier4CapEvent | PreflightEvent;

// ─── Emitter ─────────────────────────────────────────────────────────────────

export type ReplanListener = (event: ReplanEvent) => void;

function shouldEmitToStderr(): boolean {
  if (typeof process === "undefined") return false;
  const v = process.env.CELLAR_REPLAN_EVENTS;
  return v === "1" || v === "stderr";
}

/**
 * Per-goal emitter. Tags each event with a goal_id and timestamp so a log
 * aggregator can group events from one run.
 */
export class ReplanEventEmitter {
  private readonly goalId: string;
  private listeners: ReplanListener[] = [];
  private readonly stderr: boolean;

  constructor(goalId: string) {
    this.goalId = goalId;
    this.stderr = shouldEmitToStderr();
  }

  subscribe(listener: ReplanListener): () => void {
    this.listeners.push(listener);
    return () => {
      const i = this.listeners.indexOf(listener);
      if (i >= 0) this.listeners.splice(i, 1);
    };
  }

  private emit(event: ReplanEvent): void {
    if (this.stderr) {
      try { process.stderr.write(`${JSON.stringify(event)}\n`); } catch { /* swallow */ }
    }
    for (const l of this.listeners) {
      try { l(event); } catch { /* listener errors don't propagate */ }
    }
  }

  tier(args: Omit<TierEvent, "lvl" | "goal_id" | "ts" | "type">): void {
    this.emit({ lvl: "replan", goal_id: this.goalId, ts: Date.now(), type: "tier", ...args });
  }

  stall(args: Omit<StallEvent, "lvl" | "goal_id" | "ts" | "type">): void {
    this.emit({ lvl: "replan", goal_id: this.goalId, ts: Date.now(), type: "stall", ...args });
  }

  tier4Cap(args: Omit<Tier4CapEvent, "lvl" | "goal_id" | "ts" | "type">): void {
    this.emit({ lvl: "replan", goal_id: this.goalId, ts: Date.now(), type: "tier4_cap", ...args });
  }

  preflight(args: Omit<PreflightEvent, "lvl" | "goal_id" | "ts" | "type">): void {
    this.emit({ lvl: "replan", goal_id: this.goalId, ts: Date.now(), type: "preflight", ...args });
  }
}
