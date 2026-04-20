/**
 * KnowledgeStore — persistence layer for knowledge, runs, and observations.
 *
 * Abstracts all cel-store operations from the Cel god class.
 * Consumers that need persistent state should depend on this
 * interface, not the full Cel class.
 */

import type {
  KnowledgeFact,
  RunRecord,
  StepRecord,
  ObservationRecord,
  ScoredKnowledgeRecord,
  EvictionResult,
} from "../cel-bindings.js";

export interface KnowledgeStore {
  // --- Knowledge ---

  /** Query the knowledge store. */
  queryKnowledge(query: string): KnowledgeFact[];

  /** Add a fact to the knowledge store. */
  addKnowledge(content: string, source: string): number;

  /** Search knowledge using FTS5 full-text search. */
  searchKnowledge(
    query: string,
    workflowScope?: string,
    limit?: number,
  ): ScoredKnowledgeRecord[];

  /** Add a scoped knowledge fact. */
  addScopedKnowledge(
    content: string,
    source: string,
    workflowScope?: string,
    tags?: string,
  ): number;

  // --- Run Tracking ---

  /** Start tracking a workflow run. */
  startRun(workflowName: string, stepsTotal: number): number;

  /** Finish a tracked workflow run. */
  finishRun(runId: number, status: "completed" | "failed"): void;

  /** Log a step result during a workflow run. */
  logStep(
    runId: number,
    stepIndex: number,
    stepId: string,
    action: string,
    success: boolean,
    confidence: number,
    contextSnapshot?: string,
    error?: string,
  ): number;

  /** Get run history, most recent first. */
  getRunHistory(limit?: number): RunRecord[];

  /** Get step results for a specific run. */
  getStepResults(runId: number): StepRecord[];

  // --- Working Memory ---

  /** Get working memory content for a workflow. */
  getWorkingMemory(workflowName: string): string;

  /** Update working memory for a workflow. */
  updateWorkingMemory(workflowName: string, content: string): void;

  // --- Observations ---

  /** Add an observation from past runs. */
  addObservation(
    workflowName: string,
    content: string,
    priority: "high" | "medium" | "low",
    sourceRunIds: number[],
  ): number;

  /** Get active observations for a workflow. */
  getObservations(workflowName: string, limit?: number): ObservationRecord[];

  // --- Eviction ---

  /** Run eviction policies. Returns counts of deleted rows. */
  runEviction(
    runRetentionDays?: number,
    knowledgeRetentionDays?: number,
  ): EvictionResult;
}
