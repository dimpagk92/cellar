/**
 * Dedicated Validator Component — independent success/failure judgment.
 *
 * Inspired by Surfer 2 / Holo2's Policy-Localizer-Validator triad.
 * Separated from the planner to reduce bias: the validator independently
 * judges whether an action achieved its expected outcome.
 *
 * Two-tier validation (no LLM calls — pure heuristics):
 * 1. Heuristic tier (0ms): fingerprint comparison + grounding checks
 * 2. Diff tier (0ms): context diffing for state/value change detection
 */

import type { ScreenContext, PlannedAction, PlannedStep } from "../types.js";
import { validatePostAction, validateGrounding } from "./validation.js";
import { diffContexts, isDiffSignificant } from "../context-differ.js";
import { isTransitionAction } from "./helpers.js";

// ── Types ────────────────────────────────────────────────────────────────────

export interface ValidationResult {
  verdict: "success" | "failure" | "uncertain";
  confidence: number;
  reasoning: string;
  /** Hint for the orchestrator's replanning logic. */
  suggestedRecovery?: string;
  /** Whether the UI state actually changed (from fingerprint comparison). */
  stateChanged: boolean;
}

export interface ValidatorConfig {
  /** Enable the validator. When set, replaces inline post-action validation. */
  enabled?: boolean;
}

export interface ValidateActionParams {
  goal: string;
  step: PlannedStep;
  preContext: ScreenContext;
  postContext: ScreenContext;
  preFingerprint?: string;
  postFingerprint?: string;
  /** Page origin for HTTP error scoping. */
  pageOrigin?: string | null;
  /** Error from the execution catch block (exception, not validation). */
  executionError?: string;
}

// ── Planner fallback confidence threshold ────────────────────────────────────
// planStep() returns confidence: 0.3 when all retries are exhausted.
// This signals a planning failure, not a real action — we auto-fail these.
const PLANNER_FALLBACK_CONFIDENCE = 0.3;

// ── Validator ────────────────────────────────────────────────────────────────

/**
 * Validate an action's outcome using a three-tier approach.
 *
 * Tier 1 (Heuristic): fingerprint comparison + grounding checks
 * Tier 2 (Diff): context diff analysis
 * Tier 3 (LLM): optional cheap model judgment for uncertain cases
 */
export async function validateAction(
  params: ValidateActionParams,
  config: ValidatorConfig = {},
): Promise<ValidationResult> {
  const { step, preContext, postContext, preFingerprint, postFingerprint, pageOrigin, executionError } = params;
  const action = step.action;

  // ── Fast path: execution error ───────────────────────────────────────────
  if (executionError) {
    return {
      verdict: "failure",
      confidence: 0.9,
      reasoning: `Execution error: ${executionError.slice(0, 200)}`,
      suggestedRecovery: "retry_different_approach",
      stateChanged: false,
    };
  }

  // ── Fast path: planner fallback detection ────────────────────────────────
  // planStep() never throws — it returns a 0.3-confidence extract fallback.
  if (step.confidence <= PLANNER_FALLBACK_CONFIDENCE && action.type === "extract") {
    return {
      verdict: "failure",
      confidence: 0.95,
      reasoning: "Planning failed (all retries exhausted). This is a fallback extract, not a real action.",
      suggestedRecovery: "planning_failed",
      stateChanged: false,
    };
  }

  // ── Terminal actions: done/fail don't need state change validation ──────
  if (action.type === "done") {
    const groundingError = validateGrounding(step, postContext, pageOrigin);
    if (groundingError) {
      return {
        verdict: "failure",
        confidence: 0.85,
        reasoning: `Done claim rejected: ${groundingError}`,
        suggestedRecovery: "goal_not_met",
        stateChanged: false,
      };
    }
    return {
      verdict: "success",
      confidence: 0.9,
      reasoning: "Done action passed grounding validation.",
      stateChanged: false,
    };
  }

  if (action.type === "fail") {
    return {
      verdict: "failure",
      confidence: 0.9,
      reasoning: `Agent declared failure: ${(action as { reason: string }).reason}`,
      suggestedRecovery: "agent_gave_up",
      stateChanged: false,
    };
  }

  // ── Tier 1: Heuristic — fingerprint comparison ─────────────────────────
  let stateChanged = true;
  let heuristicVerdict: "success" | "failure" | "uncertain" = "uncertain";
  let heuristicReason = "";

  if (isTransitionAction(action)) {
    const postValidation = validatePostAction(preFingerprint, postFingerprint);
    if (postValidation) {
      stateChanged = false;
      heuristicVerdict = "failure";
      heuristicReason = postValidation;
    } else if (preFingerprint !== undefined && postFingerprint !== undefined) {
      // Fingerprints differ — state changed
      stateChanged = true;
      heuristicVerdict = "success";
      heuristicReason = "State changed after action.";
    }
    // If fingerprints unavailable, stays "uncertain"
  } else {
    // Non-transition actions (wait, scroll, extract) — no state change expected
    heuristicVerdict = "success";
    heuristicReason = `Non-transition action (${action.type}) completed without error.`;
  }

  // ── Tier 2: Diff — context change analysis ─────────────────────────────
  const diff = diffContexts(preContext, postContext);
  const diffSignificant = isDiffSignificant(diff);

  // Also check for value/state changes on the target element (isDiffSignificant
  // only looks at added elements and expanded/selected/visible — it misses
  // value changes on existing elements and checked state changes).
  const hasValueOrStateChange = diff.changed.length > 0 && diff.changed.some(
    (c) => c.changes.some((ch) =>
      ch.startsWith("value:") || ch.startsWith("checked:") || ch.startsWith("focused:"),
    ),
  );

  // Tier 2 can override Tier 1's failure when diff shows actual changes.
  // This handles the common case where fingerprints are coarse (e.g., URL-based)
  // but the page state actually changed (form fields filled, checkboxes toggled).
  if (heuristicVerdict === "uncertain" || (heuristicVerdict === "failure" && (diffSignificant || hasValueOrStateChange))) {
    if (diffSignificant || hasValueOrStateChange) {
      heuristicVerdict = "success";
      heuristicReason = diffSignificant
        ? `Context diff shows ${diff.added.length} added, ${diff.changed.length} changed elements.`
        : `Target element state changed (${diff.changed.map((c) => c.changes.join(", ")).join("; ")}).`;
      stateChanged = true;
    } else if (isTransitionAction(action) && heuristicVerdict === "uncertain") {
      // Transition action but no diff — likely failed
      heuristicVerdict = "failure";
      heuristicReason = "Transition action produced no significant context change.";
      stateChanged = false;
    }
  }

  // Confidence boost/penalty based on diff
  let confidence = 0.7;
  if (heuristicVerdict === "success" && diffSignificant) {
    confidence = 0.85;
  } else if (heuristicVerdict === "failure" && !stateChanged) {
    confidence = 0.8;
  }

  return {
    verdict: heuristicVerdict,
    confidence,
    reasoning: heuristicReason,
    suggestedRecovery: heuristicVerdict === "failure" ? "retry_different_approach" : undefined,
    stateChanged,
  };
}

