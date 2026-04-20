/**
 * JSONL Run Transcript — append-only log of everything that happens during a workflow run.
 *
 * Layout: ~/.cellar/runs/{runId}/transcript.jsonl
 *
 * Each line is a JSON object with timestamp, type, step info, and data.
 * This is the source of truth for replay, debugging, and observation extraction.
 */

import * as fs from "node:fs";
import * as path from "node:path";
import type { ScreenContext, WorkflowStep } from "./types.js";
import type { AssembledContext, StepResult } from "./context-assembly.js";

/** Types of transcript entries. */
export type TranscriptEntryType =
  | "run_start"
  | "context_capture"
  | "action_executed"
  | "step_complete"
  | "step_failed"
  | "paused"
  | "intervention"
  | "run_complete"
  | "observation_generated"
  | "cache_hit"
  | "cache_miss"
  | "self_heal_attempt"
  | "self_heal_success"
  | "self_heal_failed";

/** A single entry in the JSONL transcript. */
export interface TranscriptEntry {
  timestamp_ms: number;
  entry_type: TranscriptEntryType;
  step_index?: number;
  step_id?: string;
  data: Record<string, unknown>;
}

/** Manages the JSONL transcript for a single workflow run. */
export class RunTranscript {
  private filePath: string;
  private baseDir: string;

  constructor(
    private runId: number,
    cellarDir?: string,
  ) {
    const home = process.env.HOME ?? ".";
    this.baseDir = cellarDir ?? path.join(home, ".cellar");
    const runDir = path.join(this.baseDir, "runs", String(runId));
    fs.mkdirSync(runDir, { recursive: true });
    this.filePath = path.join(runDir, "transcript.jsonl");
  }

  /** Append an entry to the transcript. */
  private append(entry: TranscriptEntry): void {
    const line = JSON.stringify(entry) + "\n";
    fs.appendFileSync(this.filePath, line, "utf-8");
  }

  /** Log run start. */
  logRunStart(workflowName: string, stepsTotal: number): void {
    this.append({
      timestamp_ms: Date.now(),
      entry_type: "run_start",
      data: { workflow: workflowName, steps_total: stepsTotal, run_id: this.runId },
    });
  }

  /** Log a context capture before a step. */
  logContextCapture(stepIndex: number, stepId: string, context: AssembledContext): void {
    this.append({
      timestamp_ms: Date.now(),
      entry_type: "context_capture",
      step_index: stepIndex,
      step_id: stepId,
      data: {
        app: context.screen.app,
        window: context.screen.window,
        element_count: context.screen.elements.length,
        observations_count: context.observations.length,
        knowledge_count: context.knowledge.length,
        has_working_memory: context.workingMemory.length > 0,
      },
    });
  }

  /** Log a successful action execution. */
  logActionExecuted(
    stepIndex: number,
    stepId: string,
    action: Record<string, unknown>,
    confidence: number,
  ): void {
    this.append({
      timestamp_ms: Date.now(),
      entry_type: "action_executed",
      step_index: stepIndex,
      step_id: stepId,
      data: { action, confidence },
    });
  }

  /** Log step completion. */
  logStepComplete(stepIndex: number, stepId: string, confidence: number): void {
    this.append({
      timestamp_ms: Date.now(),
      entry_type: "step_complete",
      step_index: stepIndex,
      step_id: stepId,
      data: { confidence },
    });
  }

  /** Log step failure. */
  logStepFailed(stepIndex: number, stepId: string, error: string, confidence: number): void {
    this.append({
      timestamp_ms: Date.now(),
      entry_type: "step_failed",
      step_index: stepIndex,
      step_id: stepId,
      data: { error, confidence },
    });
  }

  /** Log a pause (low confidence). */
  logPaused(stepIndex: number, stepId: string, confidence: number): void {
    this.append({
      timestamp_ms: Date.now(),
      entry_type: "paused",
      step_index: stepIndex,
      step_id: stepId,
      data: { confidence },
    });
  }

  /** Log a user intervention. */
  logIntervention(stepIndex: number, stepId: string, userAction: Record<string, unknown>): void {
    this.append({
      timestamp_ms: Date.now(),
      entry_type: "intervention",
      step_index: stepIndex,
      step_id: stepId,
      data: { user_action: userAction },
    });
  }

  /** Log run completion. */
  logRunComplete(status: string, steps: StepResult[]): void {
    const succeeded = steps.filter((s) => s.success).length;
    const failed = steps.filter((s) => !s.success).length;
    const avgConfidence = steps.length > 0
      ? steps.reduce((sum, s) => sum + s.confidence, 0) / steps.length
      : 0;

    this.append({
      timestamp_ms: Date.now(),
      entry_type: "run_complete",
      data: {
        status,
        steps_succeeded: succeeded,
        steps_failed: failed,
        avg_confidence: Math.round(avgConfidence * 1000) / 1000,
      },
    });
  }

  /** Log an observation generated from this run. */
  logObservationGenerated(observationId: number, content: string, priority: string): void {
    this.append({
      timestamp_ms: Date.now(),
      entry_type: "observation_generated",
      data: { observation_id: observationId, content, priority },
    });
  }

  /** Log a cache hit (action replayed from cache). */
  logCacheHit(stepIndex: number, cacheKey: string): void {
    this.append({
      timestamp_ms: Date.now(),
      entry_type: "cache_hit",
      step_index: stepIndex,
      data: { cache_key: cacheKey },
    });
  }

  /** Log a cache miss (no cached action found, planning from scratch). */
  logCacheMiss(stepIndex: number, cacheKey: string): void {
    this.append({
      timestamp_ms: Date.now(),
      entry_type: "cache_miss",
      step_index: stepIndex,
      data: { cache_key: cacheKey },
    });
  }

  /** Log a self-heal attempt. */
  logSelfHealAttempt(stepIndex: number, failedAction: Record<string, unknown>, reason: string): void {
    this.append({
      timestamp_ms: Date.now(),
      entry_type: "self_heal_attempt",
      step_index: stepIndex,
      data: { failed_action: failedAction, reason },
    });
  }

  /** Log a successful self-heal. */
  logSelfHealSuccess(stepIndex: number, repairedAction: Record<string, unknown>, attemptNumber: number): void {
    this.append({
      timestamp_ms: Date.now(),
      entry_type: "self_heal_success",
      step_index: stepIndex,
      data: { repaired_action: repairedAction, attempt_number: attemptNumber },
    });
  }

  /** Log a failed self-heal (all attempts exhausted). */
  logSelfHealFailed(stepIndex: number, failedAction: Record<string, unknown>, reason: string): void {
    this.append({
      timestamp_ms: Date.now(),
      entry_type: "self_heal_failed",
      step_index: stepIndex,
      data: { failed_action: failedAction, reason },
    });
  }

  /** Read all entries from the transcript. */
  read(): TranscriptEntry[] {
    if (!fs.existsSync(this.filePath)) return [];
    const content = fs.readFileSync(this.filePath, "utf-8");
    return content
      .split("\n")
      .filter((line) => line.trim().length > 0)
      .map((line) => JSON.parse(line) as TranscriptEntry);
  }

  /** Get the file path of the transcript. */
  getPath(): string {
    return this.filePath;
  }
}
