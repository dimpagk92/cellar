/**
 * Decomposition-only test for BrowserGym/MiniWoB++ goals.
 *
 * Tests ONLY whether the orchestrator decomposes goals correctly.
 * Does NOT execute any actions — pure LLM decomposition evaluation.
 *
 * Usage: npx tsx tests/orchestrator-test/decomposition-only.ts
 */

import { config } from "dotenv";
import path from "path";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
config({ path: path.join(__dirname, "..", "..", "benchmarks", ".env") });

const apiKey = process.env.GOOGLE_GEMINI_API_KEY || process.env.GEMINI_API_KEY || "";
process.env.CEL_LLM_PROVIDER = "gemini";
process.env.CEL_LLM_MODEL = process.env.CEL_LLM_MODEL || "gemini-2.5-flash";
process.env.CEL_LLM_API_KEY = apiKey;

import { Cel } from "../../agent/src/index.js";

// ── MiniWoB++ representative goals ──────────────────────────────────────────
// These are the actual task instructions from MiniWoB++ benchmarks.
// The context is always a simple HTML page with a few interactive elements.

interface TestGoal {
  taskId: string;
  goal: string;
  context: string; // simplified screen context
  idealTasks: number; // how many sub-tasks the orchestrator SHOULD produce
  category: "click" | "type" | "form" | "navigate" | "complex";
}

const MINIWOB_GOALS: TestGoal[] = [
  // Click tasks — should ALL be 1 task
  {
    taskId: "click-test",
    goal: "Click the button that says 'Click Me!'",
    context: 'App: Chromium, Window: "Click Test", Elements: 3',
    idealTasks: 1,
    category: "click",
  },
  {
    taskId: "click-button",
    goal: "Click on the 'Submit' button.",
    context: 'App: Chromium, Window: "Click Button", Elements: 5',
    idealTasks: 1,
    category: "click",
  },
  {
    taskId: "click-button-sequence",
    goal: "Click button ONE, then click button TWO.",
    context: 'App: Chromium, Window: "Click Sequence", Elements: 4',
    idealTasks: 1, // same screen, should be one task
    category: "click",
  },
  {
    taskId: "click-link",
    goal: "Click on the link that says 'Privacy Policy'.",
    context: 'App: Chromium, Window: "Links Page", Elements: 8',
    idealTasks: 1,
    category: "click",
  },
  {
    taskId: "click-dialog",
    goal: "Click the button in the dialog box to close it.",
    context: 'App: Chromium, Window: "Dialog Test", Elements: 6',
    idealTasks: 1,
    category: "click",
  },
  {
    taskId: "click-dialog-2",
    goal: "Click the 'x' to close the dialog, then click 'Submit'.",
    context: 'App: Chromium, Window: "Dialog Test 2", Elements: 7',
    idealTasks: 1, // same screen
    category: "click",
  },
  // Type tasks — should be 1 task
  {
    taskId: "enter-text",
    goal: "Type 'hello world' into the text field and press Submit.",
    context: 'App: Chromium, Window: "Enter Text", Elements: 4',
    idealTasks: 1,
    category: "type",
  },
  {
    taskId: "enter-password",
    goal: "Enter the password 'abc123' and click Login.",
    context: 'App: Chromium, Window: "Login", Elements: 5',
    idealTasks: 1,
    category: "type",
  },
  // Form tasks — should be 1 task (all fields on same screen)
  {
    taskId: "login-user",
    goal: "Enter username 'testuser' and password 'pass123', then click Login.",
    context: 'App: Chromium, Window: "Login", Elements: 6',
    idealTasks: 1,
    category: "form",
  },
  {
    taskId: "enter-text-dynamic",
    goal: "Wait for the text field to appear, then type 'dynamic text'.",
    context: 'App: Chromium, Window: "Dynamic Text", Elements: 3',
    idealTasks: 1,
    category: "type",
  },
  // Navigate tasks — may need 2 tasks
  {
    taskId: "navigate-tree",
    goal: "Navigate to 'Section 2 > Item 3' in the tree view.",
    context: 'App: Chromium, Window: "Tree Navigation", Elements: 12',
    idealTasks: 1,
    category: "navigate",
  },
  {
    taskId: "click-tab",
    goal: "Click on the 'Tab 2' tab.",
    context: 'App: Chromium, Window: "Tabs", Elements: 5',
    idealTasks: 1,
    category: "click",
  },
  {
    taskId: "click-tab-2",
    goal: "Switch to Tab 2, then click the button inside it.",
    context: 'App: Chromium, Window: "Tabs 2", Elements: 8',
    idealTasks: 1, // same screen
    category: "click",
  },
  // Focus tasks
  {
    taskId: "focus-text",
    goal: "Click on the input field to focus it, then type 'hello'.",
    context: 'App: Chromium, Window: "Focus Test", Elements: 4',
    idealTasks: 1,
    category: "type",
  },
  {
    taskId: "focus-text-2",
    goal: "Focus the second text field and type 'world'.",
    context: 'App: Chromium, Window: "Focus Test 2", Elements: 5',
    idealTasks: 1,
    category: "type",
  },
  // Complex tasks — may need 2-3 tasks
  {
    taskId: "click-checkboxes",
    goal: "Select the checkboxes for 'Option A' and 'Option C', then click Submit.",
    context: 'App: Chromium, Window: "Checkboxes", Elements: 8',
    idealTasks: 1, // same screen
    category: "complex",
  },
  {
    taskId: "choose-date",
    goal: "Select the date December 25, 2024 from the date picker.",
    context: 'App: Chromium, Window: "Date Picker", Elements: 10',
    idealTasks: 1,
    category: "complex",
  },
  {
    taskId: "search-engine",
    goal: "Type 'restaurants near me' in the search box and click Search.",
    context: 'App: Chromium, Window: "Search Engine", Elements: 5',
    idealTasks: 1,
    category: "type",
  },
  {
    taskId: "social-media",
    goal: "Like the post by 'Alice' and reply with 'Great post!'.",
    context: 'App: Chromium, Window: "Social Media", Elements: 15',
    idealTasks: 1, // same screen, two actions
    category: "complex",
  },
  {
    taskId: "email-inbox",
    goal: "Open the email from 'Bob' and click Reply.",
    context: 'App: Chromium, Window: "Email", Elements: 12',
    idealTasks: 2, // click email, then click reply (different view)
    category: "navigate",
  },
];

// ── Decomposition runner ─────────────────────────────────────────────────────

async function testDecomposition(cel: Cel, test: TestGoal): Promise<{
  taskCount: number;
  tasks: string[];
  correct: boolean;
  latencyMs: number;
}> {
  const maxSubTasks = 10;

  // Flavor C + B hybrid: action-oriented verbs + strict single-screen merging
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

  const userPrompt = `Goal: ${test.goal}\nCurrent screen: ${test.context}`;

  const start = Date.now();
  try {
    const response = await cel.llmCompleteWithRole(systemPrompt, userPrompt, "orchestrator", 2048);
    const latencyMs = Date.now() - start;

    const match = response.match(/\{[\s\S]*\}/);
    if (!match) return { taskCount: 0, tasks: [], correct: false, latencyMs };

    const parsed = JSON.parse(match[0]);
    if (!Array.isArray(parsed.tasks)) return { taskCount: 0, tasks: [], correct: false, latencyMs };

    const tasks = parsed.tasks.map((t: { description: string }) => t.description);
    const taskCount = tasks.length;
    const correct = taskCount === test.idealTasks;

    return { taskCount, tasks, correct, latencyMs };
  } catch (e) {
    return { taskCount: 0, tasks: [String(e).slice(0, 100)], correct: false, latencyMs: Date.now() - start };
  }
}

// ── Main ─────────────────────────────────────────────────────────────────────

async function main() {
  console.log("=== Orchestrator Decomposition Test (MiniWoB++ Goals) ===\n");

  if (!apiKey) {
    console.error("No API key. Set GOOGLE_GEMINI_API_KEY.");
    process.exit(1);
  }

  const cel = new Cel();

  let correctCount = 0;
  let overDecomposed = 0;
  let underDecomposed = 0;
  const totalLatencies: number[] = [];

  const categoryResults: Record<string, { correct: number; total: number }> = {};

  for (const test of MINIWOB_GOALS) {
    process.stdout.write(`  ${test.taskId.padEnd(25)}`);
    const result = await testDecomposition(cel, test);
    totalLatencies.push(result.latencyMs);

    const status = result.correct ? "OK" : result.taskCount > test.idealTasks ? "OVER" : "UNDER";
    if (result.correct) correctCount++;
    if (result.taskCount > test.idealTasks) overDecomposed++;
    if (result.taskCount < test.idealTasks) underDecomposed++;

    // Category tracking
    if (!categoryResults[test.category]) categoryResults[test.category] = { correct: 0, total: 0 };
    categoryResults[test.category].total++;
    if (result.correct) categoryResults[test.category].correct++;

    console.log(
      `${status.padEnd(5)} | got ${result.taskCount} tasks (expected ${test.idealTasks}) | ${result.latencyMs}ms`,
    );
    for (const t of result.tasks) {
      console.log(`         ${t.slice(0, 90)}`);
    }
  }

  // ── Summary ──────────────────────────────────────────────────────────
  const total = MINIWOB_GOALS.length;
  const avgLatency = totalLatencies.reduce((a, b) => a + b, 0) / totalLatencies.length;

  console.log("\n=== SUMMARY ===\n");
  console.log(`Correct decomposition: ${correctCount}/${total} (${((correctCount / total) * 100).toFixed(0)}%)`);
  console.log(`Over-decomposed:       ${overDecomposed}/${total}`);
  console.log(`Under-decomposed:      ${underDecomposed}/${total}`);
  console.log(`Avg latency:           ${avgLatency.toFixed(0)}ms`);

  console.log("\n=== PER CATEGORY ===\n");
  for (const [cat, r] of Object.entries(categoryResults)) {
    console.log(`  ${cat.padEnd(12)} ${r.correct}/${r.total} correct (${((r.correct / r.total) * 100).toFixed(0)}%)`);
  }

  if (overDecomposed > total * 0.3) {
    console.log("\n⚠️  Over-decomposition is the main issue. The prompt needs stronger granularity hints for single-screen tasks.");
  }
  if (correctCount === total) {
    console.log("\n✅ Perfect decomposition! Ready for execution testing.");
  }
}

main().catch((e) => {
  console.error("Fatal:", e);
  process.exit(1);
});
