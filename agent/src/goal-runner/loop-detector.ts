/**
 * Loop Detector — detects repeat, ping-pong, and stale context loops.
 * Uses escalating nudges inspired by Browser-Use.
 */

import type { PlannedAction } from "../types.js";
import { simpleHash, actionSignature } from "./helpers.js";

const LOOP_WINDOW = 8;
const REPEAT_THRESHOLD = 5;       // Browser-Use style: gentle nudge at 5
const DIRECT_THRESHOLD = 8;       // Escalate to "direct" at 8
const FORCEFUL_THRESHOLD = 12;    // Auto-fail at 12
const STALE_THRESHOLD = 6;
const LOOP_GRACE_STEPS = 4;
const MONOTONY_THRESHOLD = 12;    // Same action TYPE 12+ times in a row
const MONOTONY_DIRECT = 18;
const MONOTONY_FORCEFUL = 25;

export type LoopSeverity = "gentle" | "direct" | "forceful";

export type LoopSignal =
  | { type: "none" }
  | { type: "repeat"; action: string; count: number; severity: LoopSeverity }
  | { type: "ping_pong"; actionA: string; actionB: string; severity: LoopSeverity }
  | { type: "stale_context"; stepsUnchanged: number; severity: LoopSeverity }
  | { type: "monotony"; actionType: string; count: number; severity: LoopSeverity };

export class LoopDetector {
  private actionHashes: number[] = [];
  private actionSummaries: string[] = [];
  private contextHashes: number[] = [];
  private graceRemaining: number | null = null;
  private cumulativeRepeatCount = 0;
  private lastActionHash: number | null = null;
  private consecutiveStagnantPages = 0;
  private lastContextHash: number | null = null;
  private _loopCount = 0;
  private lastActionType: string | null = null;
  private consecutiveSameType = 0;

  /** Current loop detection count. */
  get loopCount(): number { return this._loopCount; }

  check(action: PlannedAction, contextHash: number): LoopSignal {
    const summary = actionSignature(action);
    const currentHash = simpleHash(summary);

    if (this.lastActionHash !== null && currentHash === this.lastActionHash) {
      this.cumulativeRepeatCount++;
    } else {
      this.cumulativeRepeatCount = 1;
    }
    this.lastActionHash = currentHash;

    if (this.lastContextHash !== null && contextHash === this.lastContextHash) {
      this.consecutiveStagnantPages++;
    } else {
      this.consecutiveStagnantPages = 0;
    }
    this.lastContextHash = contextHash;

    // Action-type monotony tracking (e.g., 20 consecutive "extract" or "click")
    const actionType = action.type;
    if (actionType === this.lastActionType) {
      this.consecutiveSameType++;
    } else {
      this.consecutiveSameType = 1;
      this.lastActionType = actionType;
    }

    this.actionHashes.push(currentHash);
    this.actionSummaries.push(summary);
    if (this.actionHashes.length > LOOP_WINDOW) {
      this.actionHashes.shift();
      this.actionSummaries.shift();
    }

    this.contextHashes.push(contextHash);
    if (this.contextHashes.length > LOOP_WINDOW) {
      this.contextHashes.shift();
    }

    if (this.graceRemaining !== null) {
      this.graceRemaining--;
    }

    return this.detectRepeat() ?? this.detectPingPong() ?? this.detectStale() ?? this.detectMonotony() ?? { type: "none" };
  }

  shouldAutoFail(): boolean {
    // Auto-fail after 2 loop detections once grace expires,
    // OR after sustained looping (5+ consecutive detections).
    if (this._loopCount >= 5) return true;
    return this.graceRemaining !== null && this.graceRemaining <= 0 && this._loopCount >= 2;
  }

  startGrace(): void {
    this._loopCount++;
    // Only grant grace on the FIRST loop detection.
    // Subsequent detections should NOT reset the countdown — the agent
    // already had its chance to recover during the initial grace period.
    if (this.graceRemaining === null || this.graceRemaining <= 0) {
      this.graceRemaining = LOOP_GRACE_STEPS;
    }
    // Don't reset graceRemaining if it's still counting down
  }

  resetGrace(): void {
    this._loopCount = 0;
    this.graceRemaining = null;
  }

  getWarning(signal: LoopSignal): string {
    if (signal.type === "none") return "";

    const doneHint = this._loopCount >= 1
      ? ` If you believe the goal has already been achieved, output a "done" action.`
      : "";

    switch (signal.type) {
      case "repeat":
        switch (signal.severity) {
          case "gentle": return `You have repeated "${signal.action}" ${signal.count} times. Consider a different approach.${doneHint}`;
          case "direct": return `You appear stuck — "${signal.action}" repeated ${signal.count} times. List 3 alternative approaches and pick one.${doneHint}`;
          case "forceful": return `STOP. You have repeated "${signal.action}" ${signal.count} times. Output a completely different action or "done".`;
        }
        break;
      case "ping_pong":
        switch (signal.severity) {
          case "gentle": return `You're alternating between "${signal.actionA}" and "${signal.actionB}". Consider a different approach.${doneHint}`;
          case "direct": return `Stuck alternating "${signal.actionA}" and "${signal.actionB}". List 3 alternatives.${doneHint}`;
          case "forceful": return `STOP. Alternating "${signal.actionA}" and "${signal.actionB}". Output completely different action or "done".`;
        }
        break;
      case "stale_context":
        switch (signal.severity) {
          case "gentle": return `Context unchanged for ${signal.stepsUnchanged} steps. Try a different approach.${doneHint}`;
          case "direct": return `Context unchanged for ${signal.stepsUnchanged} steps. List 3 alternatives.${doneHint}`;
          case "forceful": return `STOP. Context identical for ${signal.stepsUnchanged} steps. Output different action or "done".`;
        }
        break;
      case "monotony":
        switch (signal.severity) {
          case "gentle": return `You have used "${signal.actionType}" ${signal.count} consecutive times. Try a completely different action type (e.g., click, scroll, navigate, done).${doneHint}`;
          case "direct": return `Stuck on "${signal.actionType}" for ${signal.count} steps. You MUST switch to a different action type or output "done".${doneHint}`;
          case "forceful": return `STOP. "${signal.actionType}" used ${signal.count} times in a row. Output "done" with whatever data you have, or "fail".`;
        }
        break;
    }
    return "";
  }

  private repeatSeverity(count: number): LoopSeverity {
    if (count >= FORCEFUL_THRESHOLD) return "forceful";
    if (count >= DIRECT_THRESHOLD) return "direct";
    return "gentle";
  }

  private staleSeverity(steps: number): LoopSeverity {
    if (steps >= FORCEFUL_THRESHOLD) return "forceful";
    if (steps >= DIRECT_THRESHOLD) return "direct";
    return "gentle";
  }

  private detectRepeat(): LoopSignal | null {
    if (this.cumulativeRepeatCount < REPEAT_THRESHOLD) return null;
    return {
      type: "repeat",
      action: this.actionSummaries[this.actionSummaries.length - 1],
      count: this.cumulativeRepeatCount,
      severity: this.repeatSeverity(this.cumulativeRepeatCount),
    };
  }

  private detectPingPong(): LoopSignal | null {
    const h = this.actionHashes;
    if (h.length < 4) return null;
    const [a, b, c, d] = h.slice(-4);
    if (a === c && b === d && a !== b) {
      const s = this.actionSummaries;
      const severity: LoopSeverity = this._loopCount >= 2 ? "forceful" : this._loopCount >= 1 ? "direct" : "gentle";
      return { type: "ping_pong", actionA: s[s.length - 2], actionB: s[s.length - 1], severity };
    }
    return null;
  }

  private detectStale(): LoopSignal | null {
    if (this.consecutiveStagnantPages < STALE_THRESHOLD) return null;
    return {
      type: "stale_context",
      stepsUnchanged: this.consecutiveStagnantPages,
      severity: this.staleSeverity(this.consecutiveStagnantPages),
    };
  }

  private monotonySeverity(count: number): LoopSeverity {
    if (count >= MONOTONY_FORCEFUL) return "forceful";
    if (count >= MONOTONY_DIRECT) return "direct";
    return "gentle";
  }

  private detectMonotony(): LoopSignal | null {
    if (this.consecutiveSameType < MONOTONY_THRESHOLD) return null;
    return {
      type: "monotony",
      actionType: this.lastActionType!,
      count: this.consecutiveSameType,
      severity: this.monotonySeverity(this.consecutiveSameType),
    };
  }

  /**
   * Reset all loop detection state for a new strategy.
   * Called during Tier 2+ replanning so the new approach gets a clean slate.
   */
  resetForNewStrategy(): void {
    this.actionHashes = [];
    this.actionSummaries = [];
    this.contextHashes = [];
    this.graceRemaining = null;
    this.cumulativeRepeatCount = 0;
    this.lastActionHash = null;
    this.consecutiveStagnantPages = 0;
    this.lastContextHash = null;
    this._loopCount = 0;
    this.lastActionType = null;
    this.consecutiveSameType = 0;
  }
}
