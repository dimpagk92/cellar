/**
 * A/B Test Runner for Orchestrator Decomposition Prompts
 *
 * Tests different prompt flavors against a set of goals and evaluates
 * decomposition quality. Does NOT modify production code.
 *
 * Usage: npx tsx tests/prompt-ab-test/run-ab-test.ts
 *
 * Requires: CEL_LLM_ORCHESTRATOR_PROVIDER and CEL_LLM_ORCHESTRATOR_MODEL env vars
 * (or CEL_LLM_PROVIDER / CEL_LLM_MODEL as fallback)
 */

import { Cel } from "../../agent/src/cel-bindings.js";

// ── Test goals (covering different complexity levels) ────────────────────────

const TEST_GOALS = [
  // Simple — should produce 1-2 tasks
  {
    id: "simple-search",
    goal: "Open Chrome and search for 'weather today'",
    expectedTasks: 2,
    category: "simple",
  },
  // Medium — should produce 2-4 tasks
  {
    id: "form-fill",
    goal: "Go to the contact form, fill in name 'John Doe', email 'john@test.com', message 'Hello', and submit",
    expectedTasks: 3,
    category: "medium",
  },
  {
    id: "file-management",
    goal: "Find the file 'report.pdf' in Downloads, rename it to 'Q4-report.pdf', and move it to Documents",
    expectedTasks: 3,
    category: "medium",
  },
  // Complex — should produce 3-6 tasks
  {
    id: "multi-app",
    goal: "Find the quarterly earnings data in the spreadsheet, copy the revenue number, open Slack, and send it to the #finance channel",
    expectedTasks: 4,
    category: "complex",
  },
  {
    id: "research-task",
    goal: "Open the browser, search for 'Anthropic Claude pricing', find the per-token cost for Claude Sonnet, and paste it into a new TextEdit document",
    expectedTasks: 4,
    category: "complex",
  },
  // Edge cases
  {
    id: "ambiguous",
    goal: "Make the app look better",
    expectedTasks: 1, // should refuse to decompose or keep minimal
    category: "edge",
  },
  {
    id: "single-action",
    goal: "Click the submit button",
    expectedTasks: 1,
    category: "simple",
  },
  {
    id: "conditional",
    goal: "If the login page is showing, log in with test@example.com / password123. Otherwise, navigate to the dashboard.",
    expectedTasks: 2,
    category: "edge",
  },
];

// ── Prompt flavors ───────────────────────────────────────────────────────────

interface PromptFlavor {
  id: string;
  name: string;
  systemPrompt: (maxSubTasks: number) => string;
  userPrompt: (goal: string, contextSummary: string) => string;
}

const FLAVORS: PromptFlavor[] = [
  // FLAVOR B: Strict granularity (runner-up from round 1)
  {
    id: "B-strict-granularity",
    name: "Strict granularity (penalize over-splitting)",
    systemPrompt: (max) => [
      "You are a task decomposition agent. Your job is to break a goal into the MINIMUM number of sub-tasks needed.",
      "",
      "RULES:",
      "- Each sub-task must involve a DIFFERENT screen or application state",
      "- Do NOT split actions within the same screen into separate tasks",
      "- If a goal can be done on one screen, return exactly ONE task",
      "- Filling a form = ONE task (not one per field)",
      "- Navigation + action on destination = TWO tasks at most",
      `- Maximum ${max} sub-tasks. Aim for 1-3 in most cases.`,
      "",
      "For the FIRST task, include a first_action hint.",
      "",
      "Respond with JSON only:",
      '{ "tasks": [{ "id": "t1", "description": "...", "depends_on": [], "first_action": "..." }, ...] }',
    ].join("\n"),
    userPrompt: (goal, ctx) => `Goal: ${goal}\nCurrent screen: ${ctx}`,
  },

  // FLAVOR C: Action-oriented (winner from round 1)
  {
    id: "C-action-oriented",
    name: "Action-oriented (imperative verbs)",
    systemPrompt: (max) => [
      "You are a task decomposition agent. Break the goal into actionable steps.",
      "",
      "RULES:",
      "- Start each task description with an imperative verb: Click, Type, Navigate, Open, Find, Select, Copy, Paste",
      "- Be SPECIFIC about targets: 'Click the Submit button' not 'Submit the form'",
      "- Include the exact data to enter: 'Type john@test.com in the email field' not 'Fill in email'",
      "- Group related actions on the same screen into one task",
      `- Maximum ${max} tasks. Keep it under 4 when possible.`,
      "",
      "For the FIRST task, include first_action with the exact first interaction.",
      "",
      "JSON only:",
      '{ "tasks": [{ "id": "t1", "description": "...", "depends_on": [], "first_action": "..." }, ...] }',
    ].join("\n"),
    userPrompt: (goal, ctx) => `Goal: ${goal}\nCurrent screen: ${ctx}`,
  },

  // FLAVOR F: Hybrid C+B — action-oriented with strict granularity
  {
    id: "F-hybrid-CB",
    name: "Hybrid C+B (action verbs + strict granularity)",
    systemPrompt: (max) => [
      "You are a task decomposition agent. Break the goal into the MINIMUM number of actionable steps.",
      "",
      "GRANULARITY RULES:",
      "- Each task must involve a DIFFERENT screen or application state",
      "- Do NOT split actions within the same screen into separate tasks",
      "- If a goal can be done on one screen, return exactly ONE task",
      "- Filling a form = ONE task (not one per field)",
      "- Navigation + action on destination = TWO tasks at most",
      `- Maximum ${max} tasks. Aim for 1-3 in most cases.`,
      "",
      "ACTION RULES:",
      "- Start each task with an imperative verb: Click, Type, Navigate, Open, Find, Select, Copy, Paste",
      "- Be SPECIFIC: 'Click the Submit button' not 'Submit the form'",
      "- Include exact data: 'Type john@test.com in the email field' not 'Fill in email'",
      "- For multi-field forms, list ALL fields in ONE task: 'Type John Doe in name, john@test.com in email, Hello in message, then click Submit'",
      "",
      "For the FIRST task, include first_action with the exact first interaction.",
      "",
      "JSON only:",
      '{ "tasks": [{ "id": "t1", "description": "...", "depends_on": [], "first_action": "..." }, ...] }',
    ].join("\n"),
    userPrompt: (goal, ctx) => `Goal: ${goal}\nCurrent screen: ${ctx}`,
  },

  // FLAVOR G: Hybrid C+B+D — adds context awareness
  {
    id: "G-hybrid-CBD",
    name: "Hybrid C+B+D (action + granularity + context)",
    systemPrompt: (max) => [
      "You are a task decomposition agent. Break the goal into the MINIMUM number of actionable steps.",
      "",
      "GRANULARITY RULES:",
      "- Each task must involve a DIFFERENT screen or application state",
      "- Do NOT split actions within the same screen into separate tasks",
      "- If a goal can be done on one screen, return exactly ONE task",
      "- Filling a form = ONE task (not one per field)",
      `- Maximum ${max} tasks. Aim for 1-3.`,
      "",
      "ACTION RULES:",
      "- Start each task with an imperative verb: Click, Type, Navigate, Open, Find, Select, Copy, Paste",
      "- Be SPECIFIC: 'Click the Submit button' not 'Submit the form'",
      "- Include exact data when available",
      "- For multi-field forms, list ALL fields in ONE task",
      "",
      "CONTEXT RULES:",
      "- If the current screen already shows what's needed, skip navigation",
      "- If the app is already open, don't include 'Open the app'",
      "- Adapt to what's CURRENTLY VISIBLE",
      "",
      "For the FIRST task, include first_action with the exact first interaction based on what's on screen.",
      "",
      "JSON only:",
      '{ "tasks": [{ "id": "t1", "description": "...", "depends_on": [], "first_action": "..." }, ...] }',
    ].join("\n"),
    userPrompt: (goal, ctx) => [
      `Goal: ${goal}`,
      `Current screen: ${ctx}`,
      "Plan based on what's currently visible. Skip steps for things already on screen.",
    ].join("\n"),
  },

  // FLAVOR H: Hybrid C+B+E — adds fallback hints
  {
    id: "H-hybrid-CBE",
    name: "Hybrid C+B+E (action + granularity + fallbacks)",
    systemPrompt: (max) => [
      "You are a task decomposition agent. Break the goal into the MINIMUM number of actionable steps.",
      "",
      "GRANULARITY RULES:",
      "- Each task must involve a DIFFERENT screen or application state",
      "- Do NOT split actions within the same screen into separate tasks",
      "- If a goal can be done on one screen, return exactly ONE task",
      "- Filling a form = ONE task (not one per field)",
      `- Maximum ${max} tasks. Aim for 1-3.`,
      "",
      "ACTION RULES:",
      "- Start each task with an imperative verb: Click, Type, Navigate, Open, Find, Select, Copy, Paste",
      "- Be SPECIFIC: 'Click the Submit button' not 'Submit the form'",
      "- Include exact data when available",
      "- For multi-field forms, list ALL fields in ONE task",
      "",
      "FALLBACK RULES:",
      "- For actions that might fail, add a short fallback in parentheses",
      "- Example: 'Click Submit (if not visible, press Enter)'",
      "- Only add fallbacks for uncertain actions, not every step",
      "",
      "For the FIRST task, include first_action.",
      "",
      "JSON only:",
      '{ "tasks": [{ "id": "t1", "description": "...", "depends_on": [], "first_action": "..." }, ...] }',
    ].join("\n"),
    userPrompt: (goal, ctx) => `Goal: ${goal}\nCurrent screen: ${ctx}`,
  },
];

// ── Evaluation criteria ──────────────────────────────────────────────────────

interface DecompositionResult {
  tasks: Array<{ id: string; description: string; depends_on?: string[]; first_action?: string }>;
  raw: string;
  parseSuccess: boolean;
  latencyMs: number;
}

interface EvaluationScore {
  flavorId: string;
  goalId: string;
  // Scores (0-10)
  taskCountScore: number;      // Penalty for too many or too few tasks
  specificityScore: number;     // How actionable are the descriptions
  orderingScore: number;        // Are tasks in logical order
  firstActionScore: number;     // Quality of the first_action hint
  redundancyScore: number;      // Penalty for overlapping tasks
  totalScore: number;
  // Metadata
  taskCount: number;
  expectedTasks: number;
  latencyMs: number;
  parseSuccess: boolean;
}

function evaluateDecomposition(
  result: DecompositionResult,
  goal: typeof TEST_GOALS[0],
): Omit<EvaluationScore, "flavorId"> {
  const tasks = result.tasks;

  // Task count scoring (10 = perfect match, penalty for deviation)
  const countDiff = Math.abs(tasks.length - goal.expectedTasks);
  const taskCountScore = Math.max(0, 10 - countDiff * 3);

  // Specificity: check for imperative verbs and concrete targets
  const actionVerbs = ["click", "type", "open", "find", "search", "navigate", "select", "copy", "paste", "fill", "enter", "go", "scroll", "press", "submit", "close", "rename", "move", "send", "attach"];
  let specificityTotal = 0;
  for (const task of tasks) {
    const desc = task.description.toLowerCase();
    const hasVerb = actionVerbs.some((v) => desc.startsWith(v) || desc.includes(v));
    const hasTarget = desc.includes("'") || desc.includes('"') || desc.includes("button") || desc.includes("field") || desc.includes("link");
    specificityTotal += (hasVerb ? 5 : 2) + (hasTarget ? 5 : 2);
  }
  const specificityScore = tasks.length > 0 ? Math.min(10, specificityTotal / tasks.length) : 0;

  // Ordering: check that tasks reference valid dependencies
  let orderingScore = 10;
  const taskIds = new Set(tasks.map((t) => t.id));
  for (const task of tasks) {
    for (const dep of task.depends_on ?? []) {
      if (!taskIds.has(dep)) orderingScore -= 3;
    }
  }
  orderingScore = Math.max(0, orderingScore);

  // First action quality
  let firstActionScore = 0;
  if (tasks.length > 0 && tasks[0].first_action) {
    const fa = tasks[0].first_action.toLowerCase();
    const hasVerb = actionVerbs.some((v) => fa.startsWith(v));
    firstActionScore = hasVerb ? 10 : 5;
  } else if (tasks.length > 0) {
    firstActionScore = 0; // Missing first_action
  }

  // Redundancy: check for overlapping descriptions
  let redundancyScore = 10;
  for (let i = 0; i < tasks.length; i++) {
    for (let j = i + 1; j < tasks.length; j++) {
      const words1 = new Set(tasks[i].description.toLowerCase().split(/\s+/));
      const words2 = new Set(tasks[j].description.toLowerCase().split(/\s+/));
      const overlap = [...words1].filter((w) => words2.has(w) && w.length > 3).length;
      const similarity = overlap / Math.max(words1.size, words2.size);
      if (similarity > 0.6) redundancyScore -= 3;
    }
  }
  redundancyScore = Math.max(0, redundancyScore);

  const totalScore = (taskCountScore + specificityScore + orderingScore + firstActionScore + redundancyScore) / 5;

  return {
    goalId: goal.id,
    taskCountScore,
    specificityScore,
    orderingScore,
    firstActionScore,
    redundancyScore,
    totalScore,
    taskCount: tasks.length,
    expectedTasks: goal.expectedTasks,
    latencyMs: result.latencyMs,
    parseSuccess: result.parseSuccess,
  };
}

// ── Runner ───────────────────────────────────────────────────────────────────

async function runFlavor(
  cel: Cel,
  flavor: PromptFlavor,
  goal: string,
  contextSummary: string,
): Promise<DecompositionResult> {
  const start = Date.now();
  try {
    const response = await cel.llmCompleteWithRole(
      flavor.systemPrompt(10),
      flavor.userPrompt(goal, contextSummary),
      "orchestrator",
      2048,
    );
    const latencyMs = Date.now() - start;
    const match = response.match(/\{[\s\S]*\}/);
    if (match) {
      const parsed = JSON.parse(match[0]);
      if (Array.isArray(parsed.tasks)) {
        return { tasks: parsed.tasks, raw: response, parseSuccess: true, latencyMs };
      }
    }
    return { tasks: [], raw: response, parseSuccess: false, latencyMs };
  } catch (e) {
    return { tasks: [], raw: String(e), parseSuccess: false, latencyMs: Date.now() - start };
  }
}

// ── Main ─────────────────────────────────────────────────────────────────────

async function main() {
  console.log("=== Orchestrator Prompt A/B Test ===\n");

  // Initialize CEL (just for LLM access)
  let cel: Cel;
  try {
    cel = new Cel();
  } catch (e) {
    console.error("Failed to init Cel:", e);
    console.log("\nMake sure CEL_LLM_PROVIDER and related env vars are set.");
    process.exit(1);
  }

  const mockContext = 'App: Chrome, Window: "Google Search", Elements: 15';
  const allScores: EvaluationScore[] = [];

  // Run each flavor against each goal
  for (const flavor of FLAVORS) {
    console.log(`\n── Flavor ${flavor.id}: ${flavor.name} ──`);

    for (const testGoal of TEST_GOALS) {
      process.stdout.write(`  ${testGoal.id}... `);
      const result = await runFlavor(cel, flavor, testGoal.goal, mockContext);
      const score = evaluateDecomposition(result, testGoal);

      allScores.push({ ...score, flavorId: flavor.id });

      const status = result.parseSuccess ? "OK" : "PARSE_FAIL";
      console.log(
        `${status} | tasks: ${result.tasks.length} (expected ${testGoal.expectedTasks}) | ` +
        `score: ${score.totalScore.toFixed(1)}/10 | ${result.latencyMs}ms`,
      );

      if (result.tasks.length > 0) {
        for (const t of result.tasks) {
          console.log(`    ${t.id}: ${t.description.slice(0, 80)}`);
        }
      }
    }
  }

  // ── Summary ────────────────────────────────────────────────────────────
  console.log("\n\n=== RESULTS SUMMARY ===\n");

  // Aggregate by flavor
  const flavorSummaries = new Map<string, { scores: number[]; latencies: number[]; parseFailures: number }>();
  for (const score of allScores) {
    if (!flavorSummaries.has(score.flavorId)) {
      flavorSummaries.set(score.flavorId, { scores: [], latencies: [], parseFailures: 0 });
    }
    const s = flavorSummaries.get(score.flavorId)!;
    s.scores.push(score.totalScore);
    s.latencies.push(score.latencyMs);
    if (!score.parseSuccess) s.parseFailures++;
  }

  console.log("| Flavor | Avg Score | Avg Latency | Parse Failures | Best For |");
  console.log("|--------|-----------|-------------|----------------|----------|");

  const ranked: Array<{ id: string; avg: number }> = [];
  for (const [id, summary] of flavorSummaries) {
    const avg = summary.scores.reduce((a, b) => a + b, 0) / summary.scores.length;
    const avgLatency = summary.latencies.reduce((a, b) => a + b, 0) / summary.latencies.length;
    ranked.push({ id, avg });

    const flavor = FLAVORS.find((f) => f.id === id)!;
    console.log(
      `| ${id} | ${avg.toFixed(1)}/10 | ${avgLatency.toFixed(0)}ms | ${summary.parseFailures}/${TEST_GOALS.length} | ${flavor.name} |`,
    );
  }

  ranked.sort((a, b) => b.avg - a.avg);
  console.log(`\n🏆 Winner: ${ranked[0].id} (avg score: ${ranked[0].avg.toFixed(1)}/10)`);

  // Per-category breakdown
  console.log("\n\n=== PER-CATEGORY BREAKDOWN ===\n");
  for (const category of ["simple", "medium", "complex", "edge"]) {
    console.log(`\n${category.toUpperCase()}:`);
    for (const [id, _] of flavorSummaries) {
      const catScores = allScores.filter((s) => s.flavorId === id && TEST_GOALS.find((g) => g.id === s.goalId)?.category === category);
      if (catScores.length === 0) continue;
      const avg = catScores.reduce((a, b) => a + b.totalScore, 0) / catScores.length;
      console.log(`  ${id}: ${avg.toFixed(1)}/10`);
    }
  }

  // Detailed scoring breakdown
  console.log("\n\n=== DETAILED SCORING ===\n");
  console.log("| Flavor | Goal | Count | Specificity | Order | 1stAction | Redundancy | Total |");
  console.log("|--------|------|-------|-------------|-------|-----------|------------|-------|");
  for (const score of allScores) {
    console.log(
      `| ${score.flavorId} | ${score.goalId} | ${score.taskCountScore}/10 | ${score.specificityScore.toFixed(1)}/10 | ${score.orderingScore}/10 | ${score.firstActionScore}/10 | ${score.redundancyScore}/10 | ${score.totalScore.toFixed(1)}/10 |`,
    );
  }
}

main().catch(console.error);
