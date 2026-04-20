/**
 * HistoryAdvisor — queries cel-store for past experience to inform planning.
 *
 * Integrates 4 data sources from the persistent store:
 * 1. Knowledge (FTS5) — relevant facts from past runs
 * 2. Observations — priority-ordered learnings for this workflow
 * 3. Run History — recent runs filtered by goal similarity
 * 4. Working Memory — per-workflow scratchpad
 *
 * Usage:
 *   Before starting:
 *     const advice = await HistoryAdvisor.query(cel, goal, "hotel-booking");
 *     // → advice string injected into system prompt on step 0 only
 *
 *   During replanning:
 *     const advice = await HistoryAdvisor.queryForReplan(cel, goal, failureReason, "hotel-booking");
 *     // → advice includes failure-specific knowledge search
 *
 *   After completion:
 *     await HistoryAdvisor.storeOutcome(cel, goal, trail, notebook, "hotel-booking", success);
 */

import type { KnowledgeStore } from "../interfaces/knowledge-store.js";
import type { CognitiveTrail } from "./cognitive-trail.js";
import type { Notebook } from "./notebook.js";

// ─── Types ──────────────────────────────────────────────────────────────────

interface RunRecord {
  id: number;
  workflow_name: string;
  status: string;
  steps_completed: number;
  steps_total: number;
  interventions: number;
  started_at: string;
}

interface ObservationRecord {
  id: number;
  content: string;
  priority: string;
  observed_at: string;
}

interface ScoredKnowledgeRecord {
  id: number;
  content: string;
  source: string;
  score: number;
}

// ─── Constants ──────────────────────────────────────────────────────────────

/** Stop words excluded from keyword extraction. */
const STOP_WORDS = new Set([
  "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
  "have", "has", "had", "do", "does", "did", "will", "would", "could",
  "should", "may", "might", "shall", "can", "to", "of", "in", "for",
  "on", "with", "at", "by", "from", "as", "into", "through", "during",
  "before", "after", "above", "below", "between", "and", "but", "or",
  "not", "no", "nor", "so", "yet", "both", "either", "neither", "each",
  "every", "all", "any", "few", "more", "most", "other", "some", "such",
  "this", "that", "these", "those", "i", "me", "my", "we", "our", "you",
  "your", "he", "him", "his", "she", "her", "it", "its", "they", "them",
  "their", "what", "which", "who", "whom", "how", "when", "where", "why",
]);

// ─── HistoryAdvisor ─────────────────────────────────────────────────────────

export class HistoryAdvisor {
  /**
   * Query all 4 data sources and produce an advice string.
   * Returns null if no relevant data found.
   *
   * Used before starting (step 0 system prompt).
   */
  static async query(
    cel: KnowledgeStore,
    goal: string,
    workflowName?: string,
  ): Promise<string | null> {
    const keywords = extractKeywords(goal);
    if (keywords.length === 0) return null;

    const parts: string[] = [];

    // 1. Knowledge FTS5 search
    const knowledgeQuery = keywords.join(" ");
    const knowledge = cel.searchKnowledge(knowledgeQuery, workflowName, 5);
    if (knowledge.length > 0) {
      const items = knowledge.map((k: ScoredKnowledgeRecord) => `- ${k.content}`).join("\n");
      parts.push(`RELEVANT KNOWLEDGE:\n${items}`);
    }

    // 2. Observations for this workflow (cap at 2 to avoid polluting planner context)
    if (workflowName) {
      const observations = cel.getObservations(workflowName, 2);
      if (observations.length > 0) {
        const items = observations
          .map((o: ObservationRecord) => `- [${o.priority}] ${o.content}`)
          .join("\n");
        parts.push(`PAST OBSERVATIONS:\n${items}`);
      }
    }

    // 3. Run history — filter by goal keyword overlap
    const runs = cel.getRunHistory(20);
    const relevantRuns = runs.filter((r: RunRecord) => {
      const name = r.workflow_name.toLowerCase();
      return keywords.some(kw => name.includes(kw));
    });
    if (relevantRuns.length > 0) {
      const successes = relevantRuns.filter((r: RunRecord) => r.status === "completed").length;
      const total = relevantRuns.length;
      const avgSteps = Math.round(
        relevantRuns.reduce((sum: number, r: RunRecord) => sum + r.steps_completed, 0) / total,
      );
      parts.push(
        `PAST RUNS: ${successes}/${total} succeeded (avg ${avgSteps} steps). ` +
        `${total - successes > 0 ? `${total - successes} failed.` : ""}`,
      );
    }

    // 4. Working memory
    if (workflowName) {
      const memory = cel.getWorkingMemory(workflowName);
      if (memory && memory.trim().length > 0) {
        parts.push(`WORKING MEMORY:\n${memory.trim()}`);
      }
    }

    return parts.length > 0 ? parts.join("\n\n") : null;
  }

  /**
   * Query with failure-specific context (used during replanning).
   * Searches knowledge using the failure reason in addition to goal keywords.
   */
  static async queryForReplan(
    cel: KnowledgeStore,
    goal: string,
    failureReason: string,
    workflowName?: string,
  ): Promise<string | null> {
    // Query with failure reason as additional search terms
    const failureKeywords = extractKeywords(failureReason);
    const goalKeywords = extractKeywords(goal);
    const combinedQuery = [...new Set([...failureKeywords, ...goalKeywords])].join(" ");

    const knowledge = cel.searchKnowledge(combinedQuery, workflowName, 5);
    if (knowledge.length === 0) return null;

    const items = knowledge.map((k: ScoredKnowledgeRecord) => `- ${k.content}`).join("\n");
    return `PAST EXPERIENCE WITH SIMILAR FAILURES:\n${items}`;
  }

  /**
   * Store learnings from a completed goal back to cel-store.
   * Called after goal execution finishes (success or failure).
   */
  static async storeOutcome(
    cel: KnowledgeStore,
    goal: string,
    trail: CognitiveTrail,
    notebook: Notebook,
    workflowName: string | undefined,
    success: boolean,
  ): Promise<void> {
    if (!workflowName) return;

    // Store notebook data as scoped knowledge
    for (const entry of notebook.all()) {
      if (entry.category === "data" || entry.category === "url") {
        cel.addScopedKnowledge(
          `${entry.key}: ${entry.value}`,
          `goal-runner:${goal.slice(0, 50)}`,
          workflowName,
          entry.category,
        );
      }
    }

    // Store key learning as observation
    const priority = success ? "medium" : "high";
    const summary = success
      ? `Goal "${goal.slice(0, 80)}" succeeded.`
      : `Goal "${goal.slice(0, 80)}" failed.`;

    // Include milestone info from trail
    const milestones = trail.recent()
      .filter(e => e.type === "MILESTONE")
      .map(e => e.content);

    const content = milestones.length > 0
      ? `${summary} Milestones reached: ${milestones.join(", ")}.`
      : summary;

    cel.addObservation(workflowName, content, priority, []);

    // Update working memory with notebook summary
    const notebookSummary = notebook.toSummary();
    if (notebookSummary) {
      cel.updateWorkingMemory(workflowName, notebookSummary);
    }
  }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/** Extract meaningful keywords from a goal string. */
function extractKeywords(text: string): string[] {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9\s]/g, " ")
    .split(/\s+/)
    .filter(word => word.length > 2 && !STOP_WORDS.has(word));
}
