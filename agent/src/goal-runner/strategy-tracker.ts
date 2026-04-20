/**
 * StrategyTracker — anti-loop protection for replanning.
 *
 * Prevents infinite replanning by tracking which strategies have been tried
 * for each milestone, enforcing that each replan produces a different approach,
 * and capping total replans per milestone and per goal.
 *
 * Usage:
 *   const tracker = new StrategyTracker();
 *   const id = tracker.register("search_form", "click each field and type");
 *   // ... strategy fails ...
 *   tracker.recordOutcome(id, "failed", "fields not responding to click");
 *   if (tracker.canReplan("search_form")) {
 *     // Build replan prompt including tracker.getFailedStrategies("search_form")
 *   }
 */

// ─── Types ──────────────────────────────────────────────────────────────────

export type StrategyOutcome = "success" | "failed" | "loop" | "timeout";

export interface StrategyAttempt {
  strategyId: string;
  milestone: string;
  description: string;
  stepsUsed: number;
  outcome: StrategyOutcome | null;  // null = in progress
  failureReason?: string;
  startedAt: number;
  finishedAt?: number;
}

// ─── Constants ──────────────────────────────────────────────────────────────

/** Max strategy attempts per milestone before giving up on that milestone. */
const MAX_PER_MILESTONE = 3;

/** Max total replans across the entire goal. */
const MAX_GLOBAL_REPLANS = 5;

// ─── StrategyTracker ────────────────────────────────────────────────────────

export class StrategyTracker {
  private attempts: Map<string, StrategyAttempt[]> = new Map();
  private globalReplanCount = 0;
  private nextId = 0;

  /**
   * Register a new strategy attempt for a milestone.
   * Returns a unique strategyId for tracking the outcome.
   */
  register(milestone: string, description: string): string {
    const strategyId = `${milestone}-strategy-${this.nextId++}`;
    const attempt: StrategyAttempt = {
      strategyId,
      milestone,
      description,
      stepsUsed: 0,
      outcome: null,
      startedAt: Date.now(),
    };

    const existing = this.attempts.get(milestone) ?? [];
    existing.push(attempt);
    this.attempts.set(milestone, existing);
    this.globalReplanCount++;

    return strategyId;
  }

  /**
   * Record the outcome of a strategy attempt.
   */
  recordOutcome(
    strategyId: string,
    outcome: StrategyOutcome,
    failureReason?: string,
    stepsUsed?: number,
  ): void {
    for (const attempts of this.attempts.values()) {
      const attempt = attempts.find(a => a.strategyId === strategyId);
      if (attempt) {
        attempt.outcome = outcome;
        attempt.failureReason = failureReason;
        attempt.finishedAt = Date.now();
        if (stepsUsed !== undefined) attempt.stepsUsed = stepsUsed;
        return;
      }
    }
  }

  /**
   * Whether another replan is allowed for this milestone.
   * Returns false if the milestone has exhausted its max attempts.
   */
  canReplan(milestone: string): boolean {
    const attempts = this.attempts.get(milestone) ?? [];
    const failedAttempts = attempts.filter(a => a.outcome !== null && a.outcome !== "success");
    return failedAttempts.length < MAX_PER_MILESTONE;
  }

  /**
   * Whether another replan is allowed at the global level.
   * Returns false if the goal has exhausted its max total replans.
   */
  canReplanGlobal(): boolean {
    return this.globalReplanCount < MAX_GLOBAL_REPLANS;
  }

  /**
   * Reset the global replan counter. Used exclusively by the Tier 4 recovery
   * path so that a re-decomposed milestone has a fresh strategy budget.
   * Per-milestone history is preserved so the LLM still sees what failed.
   *
   * This is the only knob that lets Tier 4 produce a genuine "second chance"
   * rather than a one-shot decomposition followed by inevitable failure.
   */
  resetGlobalCounter(): void {
    this.globalReplanCount = 0;
  }

  /**
   * Get descriptions of all failed strategies for a milestone.
   * Used to build the replan prompt: "These approaches FAILED: [list]"
   */
  getFailedStrategies(milestone?: string): string[] {
    const result: string[] = [];

    const sources = milestone
      ? [this.attempts.get(milestone) ?? []]
      : Array.from(this.attempts.values());

    for (const attempts of sources) {
      for (const attempt of attempts) {
        if (attempt.outcome && attempt.outcome !== "success") {
          let desc = `"${attempt.description}"`;
          if (attempt.failureReason) desc += ` — ${attempt.failureReason}`;
          if (attempt.stepsUsed > 0) desc += ` (${attempt.stepsUsed} steps)`;
          result.push(desc);
        }
      }
    }

    return result;
  }

  /**
   * Get the current in-progress strategy ID for a milestone, if any.
   */
  currentStrategy(milestone: string): string | null {
    const attempts = this.attempts.get(milestone) ?? [];
    const inProgress = attempts.find(a => a.outcome === null);
    return inProgress?.strategyId ?? null;
  }

  /**
   * Number of strategy attempts for a milestone (including in-progress).
   */
  attemptCount(milestone: string): number {
    return (this.attempts.get(milestone) ?? []).length;
  }

  /** Total replans across all milestones. */
  get totalReplans(): number {
    return this.globalReplanCount;
  }

  /** Summary for debugging / GoalResult. */
  toSummary(): string {
    const parts: string[] = [];
    for (const [milestone, attempts] of this.attempts) {
      const statuses = attempts.map(a => {
        const status = a.outcome ?? "in_progress";
        return `${a.description} → ${status}`;
      });
      parts.push(`${milestone}: ${statuses.join("; ")}`);
    }
    return parts.join("\n");
  }
}
