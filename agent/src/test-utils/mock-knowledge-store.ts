/**
 * Mock KnowledgeStore for testing.
 *
 * In-memory implementation — no SQLite, no native module needed.
 */

import type { KnowledgeStore } from "../interfaces/knowledge-store.js";
import type {
  KnowledgeFact,
  RunRecord,
  StepRecord,
  ObservationRecord,
  ScoredKnowledgeRecord,
  EvictionResult,
} from "../cel-bindings.js";

/** Create an in-memory mock KnowledgeStore. */
export function createMockKnowledgeStore(): KnowledgeStore & {
  /** Inspect stored knowledge. */
  _knowledge: KnowledgeFact[];
  /** Inspect stored observations. */
  _observations: ObservationRecord[];
  /** Inspect working memory. */
  _workingMemory: Map<string, string>;
  /** Inspect runs. */
  _runs: RunRecord[];
} {
  let nextId = 1;
  const knowledge: KnowledgeFact[] = [];
  const observations: ObservationRecord[] = [];
  const workingMemory = new Map<string, string>();
  const runs: RunRecord[] = [];
  const steps: StepRecord[] = [];

  return {
    _knowledge: knowledge,
    _observations: observations,
    _workingMemory: workingMemory,
    _runs: runs,

    queryKnowledge(query) {
      return knowledge.filter(k =>
        k.content.toLowerCase().includes(query.toLowerCase()),
      );
    },
    addKnowledge(content, source) {
      const id = nextId++;
      knowledge.push({ id, content, source, created_at: new Date().toISOString() });
      return id;
    },
    searchKnowledge(query, _workflowScope, _limit) {
      return knowledge
        .filter(k => k.content.toLowerCase().includes(query.toLowerCase()))
        .map(k => ({
          id: k.id,
          content: k.content,
          source: k.source,
          workflow_scope: null,
          score: 1.0,
          created_at: k.created_at,
        }));
    },
    addScopedKnowledge(content, source, _workflowScope, _tags) {
      const id = nextId++;
      knowledge.push({ id, content, source, created_at: new Date().toISOString() });
      return id;
    },

    startRun(workflowName, stepsTotal) {
      const id = nextId++;
      runs.push({
        id,
        workflow_name: workflowName,
        started_at: new Date().toISOString(),
        finished_at: null,
        status: "running",
        steps_completed: 0,
        steps_total: stepsTotal,
        interventions: 0,
      });
      return id;
    },
    finishRun(runId, status) {
      const run = runs.find(r => r.id === runId);
      if (run) {
        run.status = status;
        run.finished_at = new Date().toISOString();
      }
    },
    logStep(runId, stepIndex, stepId, action, success, confidence, contextSnapshot, error) {
      const id = nextId++;
      steps.push({
        id,
        run_id: runId,
        step_index: stepIndex,
        step_id: stepId,
        action,
        success,
        confidence,
        context_snapshot: contextSnapshot ?? null,
        error: error ?? null,
        executed_at: new Date().toISOString(),
      });
      return id;
    },
    getRunHistory(limit = 20) {
      return runs.slice(-limit).reverse();
    },
    getStepResults(runId) {
      return steps.filter(s => s.run_id === runId);
    },

    getWorkingMemory(workflowName) {
      return workingMemory.get(workflowName) ?? "";
    },
    updateWorkingMemory(workflowName, content) {
      workingMemory.set(workflowName, content);
    },

    addObservation(workflowName, content, priority, sourceRunIds) {
      const id = nextId++;
      observations.push({
        id,
        workflow_name: workflowName,
        content,
        priority,
        source_run_ids: JSON.stringify(sourceRunIds),
        observed_at: new Date().toISOString(),
        referenced_at: null,
        superseded_by: null,
        created_at: new Date().toISOString(),
      });
      return id;
    },
    getObservations(workflowName, limit = 50) {
      return observations
        .filter(o => o.workflow_name === workflowName)
        .slice(-limit);
    },

    runEviction() {
      return { superseded_observations: 0, old_runs: 0, old_knowledge: 0 };
    },
  };
}
