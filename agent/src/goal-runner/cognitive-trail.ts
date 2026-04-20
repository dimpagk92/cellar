/**
 * Cognitive Trail — system-internal narrative memory for the goal runner.
 *
 * NOT injected into the LLM prompt during normal execution (the existing
 * PlannerConversation handles LLM memory via message history). The trail is:
 *
 * 1. The system's memory — tracks what happened, what was tried, what was learned
 * 2. Injected into the prompt ONLY during replanning — when the conversation
 *    resets, the trail bridges prior context
 * 3. Human-readable — for debugging, transparency, and final result narrative
 *
 * Compaction: after COMPACT_THRESHOLD entries, older entries are summarized
 * into a single summary block. Recent entries kept in full.
 */

// ─── Types ──────────────────────────────────────────────────────────────────

export type TrailEntryType =
  | "THINK"       // LLM's thinking output
  | "ACT_OK"      // Action executed successfully
  | "ACT_FAIL"    // Action failed
  | "NOTE"        // Notebook write (data discovered)
  | "INTERRUPT"   // Cortex interrupt (dialog dismissed, loading wait)
  | "MILESTONE"   // Milestone reached — checkpoint captured
  | "REASSESS"    // Proactive reassessment triggered
  | "REPLAN"      // Strategy changed
  | "HEAL"        // Self-healing repaired a failed action
  | "SKIP"        // Pre-execute guard skipped the action (already satisfied)
  | "FOCUS_WARN"; // Pre-execute guard flagged a focus/keystroke-landing risk

export interface TrailEntry {
  step: number;
  type: TrailEntryType;
  content: string;
  timestamp: number;
  /**
   * Optional namespace for multi-subsystem tagging (borrowed from LangGraph's
   * astream_events envelope). Examples: "planner", "executor", "cortex",
   * "preflight". When unset, callers may infer a default from the entry type.
   */
  ns?: string;
}

/**
 * Event envelope emitted when entries are added. Same shape as the stored
 * TrailEntry but carries the full entry for observability consumers.
 */
export interface TrailEvent {
  type: "trail.add";
  entry: TrailEntry;
}

export type TrailListener = (event: TrailEvent) => void;

// ─── Constants ──────────────────────────────────────────────────────────────

/** When entries exceed this count, older ones are compacted. */
const COMPACT_THRESHOLD = 15;

/** How many recent entries to keep in full after compaction. */
const RECENT_KEEP = 8;

// ─── CognitiveTrail ─────────────────────────────────────────────────────────

export class CognitiveTrail {
  private entries: TrailEntry[] = [];
  private compactedSummary: string | null = null;
  private listeners: TrailListener[] = [];

  /** Add an entry to the trail. */
  add(step: number, type: TrailEntryType, content: string, ns?: string): void {
    const entry: TrailEntry = { step, type, content, timestamp: Date.now() };
    if (ns) entry.ns = ns;
    this.entries.push(entry);
    this.compactIfNeeded();
    // Emit to observability listeners (best-effort — listener errors never
    // interrupt the runner). Streaming UIs can subscribe for live event feeds.
    for (const l of this.listeners) {
      try { l({ type: "trail.add", entry }); } catch { /* listener failed — ignore */ }
    }
  }

  /** Subscribe to trail events. Returns an unsubscribe function. */
  subscribe(listener: TrailListener): () => void {
    this.listeners.push(listener);
    return () => {
      const i = this.listeners.indexOf(listener);
      if (i >= 0) this.listeners.splice(i, 1);
    };
  }

  /** Get all current (non-compacted) entries. */
  recent(): TrailEntry[] {
    return this.entries;
  }

  /** Total entries ever recorded (including compacted). */
  get length(): number {
    return this.entries.length + (this.compactedSummary ? COMPACT_THRESHOLD - RECENT_KEEP : 0);
  }

  /**
   * Format for LLM prompt injection (used during replanning only).
   * Returns compacted summary + recent entries.
   */
  toPromptContext(recentCount: number = RECENT_KEEP): string {
    const parts: string[] = [];

    if (this.compactedSummary) {
      parts.push(this.compactedSummary);
    }

    const recent = this.entries.slice(-recentCount);
    for (const entry of recent) {
      const prefix = `[${entry.step}]`;
      if (entry.type === "THINK") {
        parts.push(`${prefix} THINK: "${entry.content}"`);
      } else if (entry.type === "NOTE") {
        parts.push(`${prefix} NOTE: ${entry.content}`);
      } else if (entry.type === "MILESTONE") {
        parts.push(`${prefix} MILESTONE: ${entry.content} ✓`);
      } else {
        parts.push(`${prefix} ${entry.type}: ${entry.content}`);
      }
    }

    return parts.join("\n");
  }

  /** Human-readable summary for GoalResult and debugging. */
  toSummary(): string {
    const parts: string[] = [];
    if (this.compactedSummary) parts.push(this.compactedSummary);

    for (const entry of this.entries) {
      parts.push(`[Step ${entry.step}] ${entry.type}: ${entry.content}`);
    }
    return parts.join("\n");
  }

  /** Deep-copy entries for checkpoint storage. */
  snapshot(): { entries: TrailEntry[]; compactedSummary: string | null } {
    return {
      entries: this.entries.map(e => ({ ...e })),
      compactedSummary: this.compactedSummary,
    };
  }

  /** Restore from a checkpoint snapshot. */
  restoreFromSnapshot(snap: { entries: TrailEntry[]; compactedSummary: string | null }): void {
    this.entries = snap.entries.map(e => ({ ...e }));
    this.compactedSummary = snap.compactedSummary;
  }

  // ─── Compaction ─────────────────────────────────────────────────────────

  private compactIfNeeded(): void {
    if (this.entries.length <= COMPACT_THRESHOLD) return;

    const toCompact = this.entries.slice(0, this.entries.length - RECENT_KEEP);
    const recent = this.entries.slice(-RECENT_KEEP);

    // Build summary from older entries
    const oks = toCompact.filter(e => e.type === "ACT_OK").length;
    const fails = toCompact.filter(e => e.type === "ACT_FAIL").length;
    const notes = toCompact.filter(e => e.type === "NOTE").map(e => e.content);
    const milestones = toCompact.filter(e => e.type === "MILESTONE").map(e => e.content);
    const firstStep = toCompact[0]?.step ?? 0;
    const lastStep = toCompact[toCompact.length - 1]?.step ?? 0;

    const actions = toCompact
      .filter(e => e.type === "ACT_OK" || e.type === "ACT_FAIL")
      .map(e => e.content)
      .slice(0, 5)
      .join(", ");

    let summary = `[Steps ${firstStep}-${lastStep}]: ${actions} (${oks} OK, ${fails} failed)`;
    if (milestones.length > 0) summary += `. Milestones: ${milestones.join(", ")}`;
    if (notes.length > 0) summary += `. Notes: ${notes.slice(0, 3).join("; ")}`;

    // Append to existing compacted summary
    this.compactedSummary = this.compactedSummary
      ? `${this.compactedSummary}\n${summary}`
      : summary;

    this.entries = recent;
  }
}
