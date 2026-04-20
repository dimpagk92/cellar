/**
 * Constraint Extractor — decomposes goals into verifiable sub-requirements.
 *
 * Inspired by WebTactix's ConstraintAgent: before execution begins,
 * the user's goal is broken into explicit, checkable constraints.
 * These are tracked throughout execution and shown in perception reads.
 */

import type { Planner } from "./interfaces/planner.js";

// ─── Types ─────────────────────────────────────────────────────────────────

export interface Constraint {
  text: string;
  kind: "factual" | "action" | "navigation" | "verification";
  satisfied: boolean;
}

// ─── Extraction ────────────────────────────────────────────────────────────

/**
 * Use a fast LLM call to decompose a goal into checkable constraints.
 * Returns 1-5 constraints. Falls back to a single constraint on error.
 */
export async function extractConstraints(
  cel: Planner,
  goal: string,
): Promise<Constraint[]> {
  try {
    const prompt = `Break this automation goal into explicit, independently checkable requirements.

Goal: "${goal}"

Return a JSON array (1-5 items) of objects with:
- "text": a short, specific requirement (e.g., "Navigate to settings page")
- "kind": one of "factual" (data to verify), "action" (UI action to perform), "navigation" (page/app to reach), "verification" (check a result)

Rules:
- Each constraint must be independently verifiable
- Be specific, not vague
- Max 5 constraints
- JSON only, no markdown fences`;

    const raw = await cel.llmComplete(
      "You extract checkable requirements from automation goals. Return valid JSON only.",
      prompt,
      512,
    );

    // Parse JSON — handle markdown fences if present
    const cleaned = raw.replace(/```json?\n?/g, "").replace(/```/g, "").trim();
    const parsed = JSON.parse(cleaned) as Array<{ text: string; kind: string }>;

    return parsed.slice(0, 5).map((c) => ({
      text: c.text,
      kind: (["factual", "action", "navigation", "verification"].includes(c.kind)
        ? c.kind
        : "action") as Constraint["kind"],
      satisfied: false,
    }));
  } catch {
    // Fallback: single constraint matching the full goal
    return [{ text: goal, kind: "action", satisfied: false }];
  }
}

// ─── Satisfaction Checking ─────────────────────────────────────────────────

/**
 * Check if any unsatisfied constraints are now met based on action feedback.
 * Simple keyword matching — catches obvious completions.
 * Returns the number of newly satisfied constraints.
 */
export function checkConstraintSatisfaction(
  constraints: Constraint[],
  actionDescription: string,
  observation: string,
): number {
  let newlySatisfied = 0;
  const combined = `${actionDescription} ${observation}`.toLowerCase();

  for (const constraint of constraints) {
    if (constraint.satisfied) continue;

    // Extract keywords from the constraint text
    const keywords = constraint.text
      .toLowerCase()
      .replace(/[^a-z0-9\s]/g, " ")
      .split(/\s+/)
      .filter((w) => w.length > 3);

    // Require at least 60% of keywords to be present
    const matchCount = keywords.filter((kw) => combined.includes(kw)).length;
    if (keywords.length > 0 && matchCount / keywords.length >= 0.6) {
      constraint.satisfied = true;
      newlySatisfied++;
    }
  }

  return newlySatisfied;
}
