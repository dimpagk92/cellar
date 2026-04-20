/**
 * Alternative Queue — stores unexplored plan branches for backtracking.
 *
 * Inspired by WebTactix's global PriorityQueue: when the planner generates
 * alternatives alongside the primary action, they're stored here. On failure,
 * the goal-runner pops from this queue instead of asking the LLM for a new
 * approach — saving an entire LLM roundtrip.
 */

import type { PlannedAction } from "../types.js";

// ─── Types ─────────────────────────────────────────────────────────────────

export interface QueuedAlternative {
  /** Human-readable description (e.g., "Try keyboard shortcut instead"). */
  description: string;
  /** The concrete action to try. */
  action: PlannedAction;
  /** Which step generated this alternative. */
  sourceStep: number;
  /** Context hash at generation time — for staleness checking. */
  contextHash: string;
  /** Higher priority = try first. */
  priority: number;
}

// ─── Queue ─────────────────────────────────────────────────────────────────

export class AlternativeQueue {
  private queue: QueuedAlternative[] = [];

  /** Insert an alternative, maintaining priority order (highest first). */
  push(alt: QueuedAlternative): void {
    // Binary search for insertion point
    let lo = 0;
    let hi = this.queue.length;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (this.queue[mid].priority >= alt.priority) {
        lo = mid + 1;
      } else {
        hi = mid;
      }
    }
    this.queue.splice(lo, 0, alt);
  }

  /** Pop the highest-priority alternative. Returns null if empty. */
  pop(): QueuedAlternative | null {
    return this.queue.shift() ?? null;
  }

  /** Peek at the next alternative without removing it. */
  peek(): QueuedAlternative | null {
    return this.queue[0] ?? null;
  }

  /** Remove alternatives whose context hash no longer matches. */
  pruneStale(currentHash: string): void {
    this.queue = this.queue.filter((alt) => alt.contextHash === currentHash);
  }

  /** Number of alternatives in the queue. */
  size(): number {
    return this.queue.length;
  }

  /** Clear all alternatives. */
  clear(): void {
    this.queue = [];
  }

  /** Get a summary of queued alternatives for debugging. */
  summary(): string[] {
    return this.queue.map(
      (a) => `[p=${a.priority}] ${a.description} (step ${a.sourceStep})`,
    );
  }
}
