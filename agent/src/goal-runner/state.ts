/**
 * GoalState — typed state schema for the cognitive loop.
 *
 * Inspired by LangGraph's StateGraph: each field has explicit merge semantics
 * (reducers) instead of ad-hoc manager methods. This is a SCAFFOLD — the outer
 * runner (goal-runner.ts) still orchestrates the managers directly; this module
 * gives us a single typed surface we can migrate toward.
 *
 * Reducers:
 *   - append: appends new items to the existing array (trail entries)
 *   - overwrite: replaces the old value (current milestone)
 *   - mergeDict: shallow-merges new keys over existing (notebook updates)
 *   - resetEphemeral: resets to initial value (loop detector on replan)
 *
 * Why this matters: once state is typed, a future refactor can replace the
 * imperative `state.x.y.z = ...` mutations with `reduce(state, update)` calls
 * that can be serialized for durable execution (thread_id + checkpointer).
 */

import type { TrailEntry, TrailEntryType } from "./cognitive-trail.js";
import type { NotebookEntry } from "./notebook.js";
import type { StrategyAttempt } from "./strategy-tracker.js";
import type { Checkpoint } from "./checkpoint-manager.js";
import type { PlannerStepRecord } from "../types.js";

// ── Reducer semantics (documentation types) ─────────────────────────────────

export type Reducer<T> = (current: T, update: T) => T;

/** Append-only list. New items are concatenated at the end. */
export const appendReducer = <T>(current: T[], update: T[]): T[] => [...current, ...update];

/** Shallow merge. New keys win over existing. */
export const mergeDictReducer = <T>(current: Record<string, T>, update: Record<string, T>): Record<string, T> =>
  ({ ...current, ...update });

/** Overwrite. Used for scalar fields like currentMilestone. */
export const overwriteReducer = <T>(_current: T, update: T): T => update;

// ── Persistent state (survives Tier 2+ replans) ─────────────────────────────

export interface PersistentState {
  /** Append-only narrative log. */
  trail: TrailEntry[];
  /** Key-value store for discovered data. */
  notebook: Record<string, NotebookEntry>;
  /** All strategy attempts ever registered, per milestone. */
  strategyAttempts: Record<string, StrategyAttempt[]>;
  /** Milestone checkpoints captured during the run. */
  checkpoints: Checkpoint[];
  /** Recorded planner outputs and outcomes. */
  history: PlannerStepRecord[];
}

// ── Ephemeral state (resets on Tier 2+ replans) ─────────────────────────────

export interface EphemeralState {
  /** Current in-progress milestone label. */
  currentMilestone: string;
  /** Tier-1 nudge / replan prompt injection for the next step. */
  loopWarning: string | null;
  /** Consecutive action failures (resets on replan or success). */
  consecutiveFailures: number;
  /** Anti-loop counters that leak across replans if not reset. */
  sameClickCount: number;
  consecutiveScrolls: number;
  consecutiveNotebookWrites: number;
  lastClickTarget: string;
}

// ── Full typed goal state ───────────────────────────────────────────────────

export interface GoalState {
  goal: string;
  stepIndex: number;
  phase: CognitivePhase;
  persistent: PersistentState;
  ephemeral: EphemeralState;
}

/**
 * The seven cognitive-loop phases from docs/planning-redesign-graph.md §1.
 * Named at the type level so event consumers can filter by phase.
 */
export type CognitivePhase =
  | "pre_flight"
  | "perceive"
  | "think"
  | "assess"
  | "ground"
  | "act"
  | "reflect"
  | "gate"
  | "done";

// ── Factories ───────────────────────────────────────────────────────────────

export function initialPersistentState(): PersistentState {
  return {
    trail: [],
    notebook: {},
    strategyAttempts: {},
    checkpoints: [],
    history: [],
  };
}

export function initialEphemeralState(): EphemeralState {
  return {
    currentMilestone: "default",
    loopWarning: null,
    consecutiveFailures: 0,
    sameClickCount: 0,
    consecutiveScrolls: 0,
    consecutiveNotebookWrites: 0,
    lastClickTarget: "",
  };
}

/** Reset ephemeral state for a Tier 2+ replan. Notebook + trail survive. */
export function resetEphemeralForReplan(state: EphemeralState): EphemeralState {
  return {
    ...state,
    loopWarning: null,
    consecutiveFailures: 0,
    sameClickCount: 0,
    consecutiveScrolls: 0,
    consecutiveNotebookWrites: 0,
    lastClickTarget: "",
  };
}

// ── Trail event type re-export (for convenience) ────────────────────────────
export type { TrailEntryType };
