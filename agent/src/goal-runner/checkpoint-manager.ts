/**
 * CheckpointManager — captures and restores state at milestone boundaries.
 *
 * When the LLM signals a milestone ("progress": "milestone:on_results_page"),
 * the system captures a checkpoint: context fingerprint, URL, notebook snapshot,
 * step index. On Tier 3 replanning, the system backtracks to the most recent
 * checkpoint and restores the notebook.
 *
 * Checkpoint restore is best-effort:
 * - Browser: navigate to checkpoint URL
 * - Desktop: notebook restored, replan prompt includes checkpoint context
 *   (no reliable way to restore arbitrary desktop app state)
 */

import * as fs from "fs";
import * as path from "path";
import type { NotebookEntry } from "./notebook.js";

// ─── Types ──────────────────────────────────────────────────────────────────

export interface Checkpoint {
  /** Milestone label (e.g. "on_results_page"). */
  milestone: string;
  /** Step index when checkpoint was captured. */
  stepIndex: number;
  /** Fingerprint of the context at checkpoint time. */
  contextFingerprint: string;
  /** URL if browser-based (from context or stateFingerprint). */
  url: string | null;
  /** App + window title at checkpoint time. */
  appWindow: string;
  /** Deep copy of notebook entries at checkpoint time. */
  notebookSnapshot: NotebookEntry[];
  /** When the checkpoint was captured. */
  timestamp: number;
}

// ─── CheckpointManager ──────────────────────────────────────────────────────

export class CheckpointManager {
  private checkpoints: Checkpoint[] = [];
  private persistPath: string | null;

  /**
   * @param persistPath — optional file path to persist checkpoints to disk.
   *   When provided, checkpoints are written after each capture and
   *   restored on construction. Enables crash recovery.
   */
  constructor(persistPath?: string) {
    this.persistPath = persistPath ?? null;
    if (this.persistPath) {
      this.restoreFromDisk();
    }
  }

  /**
   * Capture a checkpoint at a milestone boundary.
   */
  capture(
    milestone: string,
    stepIndex: number,
    contextFingerprint: string,
    url: string | null,
    appWindow: string,
    notebookSnapshot: NotebookEntry[],
  ): Checkpoint {
    const checkpoint: Checkpoint = {
      milestone,
      stepIndex,
      contextFingerprint,
      url,
      appWindow,
      notebookSnapshot: notebookSnapshot.map(e => ({ ...e })),
      timestamp: Date.now(),
    };
    this.checkpoints.push(checkpoint);
    this.persistToDisk();
    return checkpoint;
  }

  /**
   * Get the most recent checkpoint.
   * Returns null if no checkpoints have been captured.
   */
  getLatest(): Checkpoint | null {
    return this.checkpoints.length > 0
      ? this.checkpoints[this.checkpoints.length - 1]
      : null;
  }

  /**
   * Get checkpoint for a specific milestone.
   * Returns the most recent one if multiple exist for the same milestone.
   */
  getByMilestone(milestone: string): Checkpoint | null {
    for (let i = this.checkpoints.length - 1; i >= 0; i--) {
      if (this.checkpoints[i].milestone === milestone) {
        return this.checkpoints[i];
      }
    }
    return null;
  }

  /**
   * Get the checkpoint BEFORE the most recent one (for Tier 3 backtracking).
   * If only one checkpoint exists, returns that one.
   */
  getPrevious(): Checkpoint | null {
    if (this.checkpoints.length >= 2) {
      return this.checkpoints[this.checkpoints.length - 2];
    }
    return this.checkpoints[0] ?? null;
  }

  /** Get all checkpoints in order. */
  getAll(): Checkpoint[] {
    return [...this.checkpoints];
  }

  /** Number of checkpoints captured. */
  get count(): number {
    return this.checkpoints.length;
  }

  /** Summary for debugging. */
  toSummary(): string {
    return this.checkpoints
      .map((cp, i) => `[${i}] ${cp.milestone} at step ${cp.stepIndex} (${cp.url ?? cp.appWindow})`)
      .join("\n");
  }

  /** Clear all checkpoints (and remove from disk). */
  clear(): void {
    this.checkpoints = [];
    if (this.persistPath) {
      try { fs.unlinkSync(this.persistPath); } catch { /* file may not exist */ }
    }
  }

  // ── Disk persistence ──────────────────────────────────────────────────

  private persistToDisk(): void {
    if (!this.persistPath) return;
    try {
      const dir = path.dirname(this.persistPath);
      if (!fs.existsSync(dir)) fs.mkdirSync(dir, { recursive: true });
      fs.writeFileSync(this.persistPath, JSON.stringify(this.checkpoints, null, 2));
    } catch (e) {
      console.warn(`[checkpoint] Failed to persist: ${String(e).slice(0, 80)}`);
    }
  }

  private restoreFromDisk(): void {
    if (!this.persistPath) return;
    try {
      if (fs.existsSync(this.persistPath)) {
        const data = fs.readFileSync(this.persistPath, "utf-8");
        const parsed = JSON.parse(data);
        if (Array.isArray(parsed)) {
          this.checkpoints = parsed;
        }
      }
    } catch (e) {
      console.warn(`[checkpoint] Failed to restore: ${String(e).slice(0, 80)}`);
    }
  }
}
