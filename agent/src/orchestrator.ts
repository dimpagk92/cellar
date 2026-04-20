/**
 * Orchestrator — hierarchical task decomposition and sub-agent coordination.
 *
 * Inspired by Surfer 2's separation of strategic planning (orchestrator) from
 * tactical execution (sub-agents). The orchestrator decomposes complex goals
 * into sub-tasks, delegates each to `runGoal()`, and replans on failure.
 *
 * Uses `LlmRole::Orchestrator` (defaults to Gemini Flash) for cheap decomposition
 * and replanning. The expensive frontier model is only used inside sub-agents
 * for step-by-step planning.
 */

import type { Cel } from "./cel-bindings.js";
// Orchestrator uses the full Cel because it passes it to runGoal().
// Its own LLM calls only need Planner, but sub-agents need everything.
import type { ScreenContext, GoalMetrics } from "./types.js";
import type { AdapterRegistry } from "./action-executor.js";
import type { GoalRunnerConfig, GoalResult, GoalRunnerCallbacks } from "./goal-runner/config.js";
import type { ValidationResult } from "./goal-runner/validator.js";
import { validateAction } from "./goal-runner/validator.js";
import { runGoal } from "./goal-runner.js";
import type { Cortex } from "./cortex.js";

// ── Types ────────────────────────────────────────────────────────────────────

export interface SubTask {
  id: string;
  description: string;
  dependsOn: string[];
  status: "pending" | "in_progress" | "completed" | "failed";
  result?: GoalResult;
  validationResult?: ValidationResult;
  attempts: number;
  maxAttempts: number;
}

export interface OrchestratorConfig {
  goal: string;
  /** Max sub-tasks allowed from decomposition. Default: 10. */
  maxSubTasks?: number;
  /** Global step budget across all sub-tasks. Default: 30. */
  maxTotalSteps?: number;
  /** Max replanning attempts. Default: 2. */
  maxReplans?: number;
  /** Config passed through to each sub-agent's runGoal(). */
  subAgentConfig: Omit<GoalRunnerConfig, "goal">;
  /** Cortex instance shared across all sub-agents. */
  cortex?: Cortex;
}

export interface OrchestratorResult {
  status: "achieved" | "failed" | "timeout";
  summary: string;
  subTasks: SubTask[];
  totalSteps: number;
  metrics?: GoalMetrics;
}

// ── Constants ────────────────────────────────────────────────────────────────

const DEFAULT_MAX_SUB_TASKS = 10;
const DEFAULT_MAX_TOTAL_STEPS = 30;
const DEFAULT_MAX_REPLANS = 2;
const DEFAULT_MAX_ATTEMPTS = 2;

// ── Simple goal detection ────────────────────────────────────────────────────
// A goal is "simple" (skip decomposition) when it targets a single screen.
// We check: (1) starts with an action verb, (2) no multi-screen signals,
// (3) context has few elements (single-page UI).

const SIMPLE_VERB_PATTERNS = [
  /^click\s/i, /^type\s/i, /^press\s/i, /^scroll\s/i, /^wait\s/i,
  /^extract\s/i, /^open\s/i, /^close\s/i, /^select\s/i, /^fill\s/i,
  /^enter\s/i, /^submit\s/i, /^check\s/i, /^uncheck\s/i, /^toggle\s/i,
  /^focus\s/i, /^find\s/i, /^search\s/i, /^like\s/i, /^reply\s/i,
  /^navigate\s/i, /^choose\s/i, /^pick\s/i, /^drag\s/i, /^drop\s/i,
];

/** Signals that a goal spans multiple screens/apps → needs decomposition. */
const MULTI_SCREEN_SIGNALS = [
  /\bthen open\b/i,
  /\bswitch to\b.*\bapp\b/i,
  /\bopen\s+(slack|chrome|finder|mail|excel|safari|terminal)\b/i,
  /\bnavigate to\b.*\bthen\b/i,
  /\bcopy\b.*\bpaste\b/i,   // cross-app copy-paste
  /\bfrom\b.*\bto\b.*\b(app|window|tab)\b/i,
];

function isSimpleGoal(goal: string, elementCount?: number): boolean {
  const trimmed = goal.trim();

  // If multi-screen signals are present, always decompose
  if (MULTI_SCREEN_SIGNALS.some((p) => p.test(trimmed))) return false;

  // Starts with action verb → likely single-screen
  if (SIMPLE_VERB_PATTERNS.some((p) => p.test(trimmed))) return true;

  // Context-based: few elements = simple page, skip decomposition
  if (elementCount !== undefined && elementCount <= 20) return true;

  // Short goal (< 80 chars) with no "and then" / "after that" → likely single action
  if (trimmed.length < 80 && !/\b(then|after that|next|finally|afterwards)\b/i.test(trimmed)) return true;

  return false;
}

// ── Main orchestrator ────────────────────────────────────────────────────────

/**
 * Orchestrate a complex goal by decomposing it into sub-tasks,
 * delegating each to runGoal(), and replanning on failure.
 */
export async function orchestrate(
  cel: Cel,
  config: OrchestratorConfig,
  callbacks: GoalRunnerCallbacks,
  adapters?: AdapterRegistry,
): Promise<OrchestratorResult> {
  const maxSubTasks = config.maxSubTasks ?? DEFAULT_MAX_SUB_TASKS;
  const maxTotalSteps = config.maxTotalSteps ?? DEFAULT_MAX_TOTAL_STEPS;
  const maxReplans = config.maxReplans ?? DEFAULT_MAX_REPLANS;

  let totalSteps = 0;
  let replansUsed = 0;
  const allMetrics: Partial<GoalMetrics> = {};

  // ── Skip decomposition for simple goals ──────────────────────────────
  // Get element count for context-based detection
  let elementCount: number | undefined;
  try {
    const ctx = await callbacks.getContext();
    elementCount = ctx.elements.length;
  } catch { /* no context */ }

  let tasks: SubTask[];
  if (isSimpleGoal(config.goal, elementCount)) {
    tasks = [{
      id: "t1",
      description: config.goal,
      dependsOn: [],
      status: "pending",
      attempts: 0,
      maxAttempts: DEFAULT_MAX_ATTEMPTS,
    }];
  } else {
    tasks = await decompose(cel, config.goal, callbacks, maxSubTasks);
  }

  // ── Execution loop ───────────────────────────────────────────────────
  while (true) {
    const nextTask = findNextTask(tasks);
    if (!nextTask) break;

    // Check step budget
    const remainingSteps = maxTotalSteps - totalSteps;
    if (remainingSteps <= 0) {
      return {
        status: "timeout",
        summary: `Step budget exhausted (${maxTotalSteps} steps). Completed ${tasks.filter((t) => t.status === "completed").length}/${tasks.length} sub-tasks.`,
        subTasks: tasks,
        totalSteps,
        metrics: allMetrics as GoalMetrics,
      };
    }

    // Calculate per-task step budget
    const remainingTasks = tasks.filter((t) => t.status === "pending" || t.status === "in_progress").length;
    const stepBudget = Math.max(3, Math.ceil(remainingSteps / remainingTasks));

    nextTask.status = "in_progress";
    nextTask.attempts++;

    // ── Delegate to runGoal() ────────────────────────────────────────
    const subAgentConfig: GoalRunnerConfig = {
      ...config.subAgentConfig,
      goal: nextTask.description,
      maxSteps: stepBudget,
      cortex: config.cortex,
    };

    let result: GoalResult;
    try {
      result = await runGoal(cel, subAgentConfig, callbacks, adapters);
    } catch (e) {
      result = {
        status: "failed",
        summary: `Sub-agent threw: ${String(e).slice(0, 200)}`,
        totalSteps: 0,
        history: [],
      };
    }

    nextTask.result = result;
    totalSteps += result.totalSteps;
    mergeMetrics(allMetrics, result.metrics);

    // ── Judge result ─────────────────────────────────────────────────
    if (result.status === "achieved") {
      nextTask.status = "completed";
      continue;
    }

    // Sub-agent failed or hit max steps
    nextTask.status = "failed";

    // Check escalation signal
    const shouldReplan = result.escalation === "replan"
      || result.status === "failed"
      || result.status === "max_steps";

    if (shouldReplan && replansUsed < maxReplans && nextTask.attempts >= nextTask.maxAttempts) {
      replansUsed++;

      const completedTasks = tasks.filter((t) => t.status === "completed");
      const newTasks = await replan(
        cel, nextTask, completedTasks, config.goal, callbacks,
      );

      // Safety: if replan returns same first task, abort (LLM is stuck)
      if (newTasks.length > 0 && newTasks[0].description === nextTask.description) {
        return {
          status: "failed",
          summary: `Replanning produced the same task — aborting. Last failure: ${result.summary}`,
          subTasks: tasks,
          totalSteps,
          metrics: allMetrics as GoalMetrics,
        };
      }

      // Replace remaining pending tasks with replanned ones
      const keptTasks = tasks.filter((t) => t.status === "completed" || t.status === "failed");
      tasks = [...keptTasks, ...newTasks];
      continue;
    }

    // No replan available — retry if attempts remain
    if (nextTask.attempts < nextTask.maxAttempts) {
      nextTask.status = "pending";
      continue;
    }

    // All attempts and replans exhausted
    return {
      status: "failed",
      summary: `Sub-task "${nextTask.description}" failed after ${nextTask.attempts} attempts. ${result.summary}`,
      subTasks: tasks,
      totalSteps,
      metrics: allMetrics as GoalMetrics,
    };
  }

  // ── All tasks completed ────────────────────────────────────────────
  const completedCount = tasks.filter((t) => t.status === "completed").length;
  const lastResult = tasks[tasks.length - 1]?.result;

  return {
    status: completedCount === tasks.length ? "achieved" : "failed",
    summary: completedCount === tasks.length
      ? `All ${tasks.length} sub-tasks completed. ${lastResult?.summary ?? ""}`
      : `${completedCount}/${tasks.length} sub-tasks completed.`,
    subTasks: tasks,
    totalSteps,
    metrics: allMetrics as GoalMetrics,
  };
}

// ── Task decomposition ───────────────────────────────────────────────────────

async function decompose(
  cel: Cel,
  goal: string,
  callbacks: GoalRunnerCallbacks,
  maxSubTasks: number,
): Promise<SubTask[]> {
  // Get current screen context summary (not full context — save tokens)
  let contextSummary = "No context available.";
  try {
    const ctx = await callbacks.getContext();
    const elementCount = ctx.elements.length;
    const appName = ctx.app ?? "unknown";
    const windowTitle = ctx.window ?? "unknown";
    contextSummary = `App: ${appName}, Window: "${windowTitle}", Elements: ${elementCount}`;
  } catch { /* no context available */ }

  // Flavor C+B hybrid: action-oriented + strict single-screen merging.
  // Tested: 95% correct on MiniWoB++ goals (up from 50% with pure Flavor C).
  const systemPrompt = [
    "You are a task decomposition agent. Break the goal into the MINIMUM number of actionable steps.",
    "",
    "CRITICAL RULE: If all actions happen on the SAME screen, return exactly ONE task.",
    "Multiple clicks, typing, and submissions on the same page = ONE task, not separate tasks.",
    "",
    "Examples of ONE task (same screen):",
    '- "Click checkbox A, click checkbox C, then click Submit" = ONE task',
    '- "Type username, type password, click Login" = ONE task',
    '- "Click button ONE, then click button TWO" = ONE task',
    '- "Wait for element, then type text" = ONE task',
    "",
    "Only create multiple tasks when the screen CHANGES (navigation, new page, different app).",
    "",
    "ACTION RULES:",
    "- Start each task with an imperative verb: Click, Type, Navigate, Open, Find, Select",
    "- Be SPECIFIC: 'Click the Submit button' not 'Submit the form'",
    "- Include exact data: 'Type john@test.com in the email field'",
    "- List ALL actions for a screen in one task description",
    `- Maximum ${maxSubTasks} tasks. For single-screen goals, return exactly 1.`,
    "",
    "For the FIRST task, include first_action with the exact first interaction.",
    "",
    "JSON only:",
    '{ "tasks": [{ "id": "t1", "description": "...", "depends_on": [], "first_action": "..." }, ...] }',
  ].join("\n");

  const userPrompt = [
    `Goal: ${goal}`,
    `Current screen: ${contextSummary}`,
  ].join("\n");

  try {
    const response = await cel.llmCompleteWithRole(systemPrompt, userPrompt, "orchestrator", 2048);
    const match = response.match(/\{[\s\S]*\}/);
    if (match) {
      const parsed = JSON.parse(match[0]);
      if (Array.isArray(parsed.tasks) && parsed.tasks.length > 0) {
        return parsed.tasks.slice(0, maxSubTasks).map((t: { id?: string; description: string; depends_on?: string[]; first_action?: string }, i: number) => ({
          id: t.id ?? `t${i + 1}`,
          description: t.first_action && i === 0
            ? `${t.description} (Hint: start by ${t.first_action})`
            : t.description,
          dependsOn: t.depends_on ?? [],
          status: "pending" as const,
          attempts: 0,
          maxAttempts: DEFAULT_MAX_ATTEMPTS,
        }));
      }
    }
  } catch { /* decomposition failed — fallback to single task */ }

  // Fallback: treat entire goal as single task
  return [{
    id: "t1",
    description: goal,
    dependsOn: [],
    status: "pending",
    attempts: 0,
    maxAttempts: DEFAULT_MAX_ATTEMPTS,
  }];
}

// ── Replanning ───────────────────────────────────────────────────────────────

async function replan(
  cel: Cel,
  failedTask: SubTask,
  completedTasks: SubTask[],
  originalGoal: string,
  callbacks: GoalRunnerCallbacks,
): Promise<SubTask[]> {
  let contextSummary = "No context available.";
  try {
    const ctx = await callbacks.getContext();
    const elementCount = ctx.elements.length;
    const appName = ctx.app ?? "unknown";
    const windowTitle = ctx.window ?? "unknown";
    contextSummary = `App: ${appName}, Window: "${windowTitle}", Elements: ${elementCount}`;
  } catch { /* no context available */ }

  const completedSummary = completedTasks.length > 0
    ? completedTasks.map((t) => `- [DONE] ${t.description}: ${t.result?.summary ?? "OK"}`).join("\n")
    : "No tasks completed yet.";

  // Variant B (A/B test winner): Concise + same-screen merging.
  // Tested: 5/5 different strategies, 80% actionable, avg 1.8 tasks, 1288ms.
  const systemPrompt = [
    "You are a replanning agent. A sub-task failed. Produce a NEW plan with a DIFFERENT approach.",
    "",
    "RULES:",
    "- Do NOT repeat the failed approach",
    "- Use the failure reason to choose a better strategy",
    "- Same screen = ONE task. Only split across screen changes.",
    "- Start each task with an imperative verb",
    "- Be specific about targets and data",
    "- Minimum tasks needed. Aim for 1-2.",
    "",
    "JSON only:",
    '{ "tasks": [{ "id": "t1", "description": "...", "depends_on": [] }, ...] }',
  ].join("\n");

  const userPrompt = [
    `Goal: ${originalGoal}`,
    completedTasks.length > 0
      ? `Done: ${completedTasks.map((t) => t.description).join("; ")}`
      : "",
    `Failed: "${failedTask.description}" — ${failedTask.result?.summary ?? "Unknown"}`,
    `Screen: ${contextSummary}`,
  ].filter(Boolean).join("\n");

  try {
    const response = await cel.llmCompleteWithRole(systemPrompt, userPrompt, "orchestrator", 2048);
    const match = response.match(/\{[\s\S]*\}/);
    if (match) {
      const parsed = JSON.parse(match[0]);
      if (Array.isArray(parsed.tasks) && parsed.tasks.length > 0) {
        return parsed.tasks.map((t: { id?: string; description: string; depends_on?: string[] }, i: number) => ({
          id: t.id ?? `r${i + 1}`,
          description: t.description,
          dependsOn: t.depends_on ?? [],
          status: "pending" as const,
          attempts: 0,
          maxAttempts: DEFAULT_MAX_ATTEMPTS,
        }));
      }
    }
  } catch { /* replan failed */ }

  // Replan failed — return empty (will cause orchestrator to abort)
  return [];
}

// ── Helpers ──────────────────────────────────────────────────────────────────

function findNextTask(tasks: SubTask[]): SubTask | undefined {
  return tasks.find((t) => {
    if (t.status !== "pending") return false;
    // Check all dependencies are completed
    return t.dependsOn.every((depId) => {
      const dep = tasks.find((d) => d.id === depId);
      return dep && dep.status === "completed";
    });
  });
}

function mergeMetrics(target: Partial<GoalMetrics>, source?: GoalMetrics): void {
  if (!source) return;
  for (const [key, value] of Object.entries(source)) {
    if (typeof value === "number") {
      (target as Record<string, number>)[key] = ((target as Record<string, number>)[key] ?? 0) + value;
    }
  }
}
