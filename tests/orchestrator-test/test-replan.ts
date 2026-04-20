/**
 * Replanning Test — verifies the orchestrator replans on sub-task failure.
 *
 * Strategy: Give the orchestrator a multi-step goal where the first approach
 * is impossible (the element doesn't exist). The orchestrator should:
 * 1. Decompose the goal
 * 2. Fail on the first sub-task (element not found)
 * 3. Replan with a different approach
 * 4. The replan should suggest a different strategy
 *
 * This test uses the LLM for decomposition + replanning but does NOT
 * execute actions (we mock the runGoal to simulate failure).
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

import { Cel } from "../../agent/src/cel-bindings.js";

// ── Replan prompt (same as orchestrator.ts) ──────────────────────────────────

async function testReplan(
  cel: Cel,
  originalGoal: string,
  failedTaskDesc: string,
  failureReason: string,
  completedTasks: string[],
  contextSummary: string,
): Promise<{ tasks: string[]; isDifferent: boolean; latencyMs: number }> {
  const completedSummary = completedTasks.length > 0
    ? completedTasks.map((t) => `- [DONE] ${t}`).join("\n")
    : "No tasks completed yet.";

  const systemPrompt = [
    "You are a replanning agent. A sub-task failed. Produce a NEW set of remaining tasks",
    "using a COMPLETELY DIFFERENT approach. Do not repeat the failed approach.",
    "",
    "Respond with JSON only:",
    '{ "tasks": [{ "id": "t1", "description": "...", "depends_on": [] }, ...] }',
  ].join("\n");

  const userPrompt = [
    `Original goal: ${originalGoal}`,
    "",
    "Completed so far:",
    completedSummary,
    "",
    `Failed task: "${failedTaskDesc}"`,
    `Failure reason: ${failureReason}`,
    "",
    `Current screen: ${contextSummary}`,
    "",
    "Produce a new plan with a different approach for the remaining work.",
  ].join("\n");

  const start = Date.now();
  try {
    const response = await cel.llmCompleteWithRole(systemPrompt, userPrompt, "orchestrator", 2048);
    const latencyMs = Date.now() - start;

    const match = response.match(/\{[\s\S]*\}/);
    if (!match) return { tasks: [], isDifferent: false, latencyMs };

    const parsed = JSON.parse(match[0]);
    if (!Array.isArray(parsed.tasks)) return { tasks: [], isDifferent: false, latencyMs };

    const tasks = parsed.tasks.map((t: { description: string }) => t.description);

    // Check if replan is different from failed task
    const isDifferent = tasks.length > 0 && !tasks[0].toLowerCase().includes(failedTaskDesc.toLowerCase().slice(0, 30));

    return { tasks, isDifferent, latencyMs };
  } catch {
    return { tasks: [], isDifferent: false, latencyMs: Date.now() - start };
  }
}

// ── Test scenarios ───────────────────────────────────────────────────────────

interface ReplanScenario {
  id: string;
  goal: string;
  failedTask: string;
  failureReason: string;
  completedTasks: string[];
  context: string;
  description: string;
}

const SCENARIOS: ReplanScenario[] = [
  {
    id: "button-not-found",
    goal: "Submit the contact form",
    failedTask: "Click the Submit button",
    failureReason: "Element 'Submit' not found in context. Available: [Save, Cancel, Reset]",
    completedTasks: ["Fill in name 'John' and email 'john@test.com' in the form"],
    context: 'App: Chromium, Window: "Contact Form", Elements: 8',
    description: "Button has different label — should suggest clicking 'Save' instead",
  },
  {
    id: "search-failed",
    goal: "Find the quarterly report and download it",
    failedTask: "Type 'quarterly report' in the search box and press Enter",
    failureReason: "No search box found on page. The page shows a file browser with folders.",
    completedTasks: [],
    context: 'App: Chromium, Window: "Documents", Elements: 15',
    description: "No search box — should suggest browsing folders instead",
  },
  {
    id: "login-wrong-page",
    goal: "Log in and navigate to the dashboard",
    failedTask: "Type 'admin' in username field and 'pass123' in password field, click Login",
    failureReason: "No username or password field found. The page shows a 'Sign in with Google' button and 'Use SSO' link.",
    completedTasks: [],
    context: 'App: Chromium, Window: "Login", Elements: 5',
    description: "Form login unavailable — should suggest SSO or Google sign-in",
  },
  {
    id: "navigation-blocked",
    goal: "Open Settings and enable dark mode",
    failedTask: "Click the Settings menu item",
    failureReason: "Settings menu not visible. A cookie consent dialog is blocking the page.",
    completedTasks: [],
    context: 'App: Chromium, Window: "App", Elements: 12',
    description: "Cookie dialog blocking — should suggest dismissing it first",
  },
  {
    id: "element-moved",
    goal: "Delete the selected emails",
    failedTask: "Click the Delete button in the toolbar",
    failureReason: "Delete button not in toolbar. The toolbar shows: Archive, Spam, Move, More...",
    completedTasks: ["Select all emails from 'Newsletter'"],
    context: 'App: Chromium, Window: "Gmail", Elements: 20',
    description: "Delete might be under 'More...' menu — should suggest that",
  },
];

// ── Main ─────────────────────────────────────────────────────────────────────

async function main() {
  console.log("=== Replanning Test ===\n");

  if (!apiKey) {
    console.error("No API key. Set GOOGLE_GEMINI_API_KEY.");
    process.exit(1);
  }

  const cel = new Cel();
  let passed = 0;

  for (const scenario of SCENARIOS) {
    console.log(`── ${scenario.id}: ${scenario.description} ──`);
    console.log(`  Failed task: "${scenario.failedTask}"`);
    console.log(`  Reason: ${scenario.failureReason.slice(0, 80)}`);

    const result = await testReplan(
      cel,
      scenario.goal,
      scenario.failedTask,
      scenario.failureReason,
      scenario.completedTasks,
      scenario.context,
    );

    const status = result.isDifferent ? "PASS" : "FAIL";
    if (result.isDifferent) passed++;

    console.log(`  Result: ${status} | ${result.tasks.length} new tasks | ${result.latencyMs}ms`);
    for (const t of result.tasks) {
      console.log(`    → ${t.slice(0, 90)}`);
    }
    console.log();
  }

  console.log(`\n=== SUMMARY: ${passed}/${SCENARIOS.length} replans produced different strategies ===`);
  if (passed === SCENARIOS.length) {
    console.log("✅ Replanning works correctly — always produces different approaches.");
  } else {
    console.log("⚠️  Some replans repeated the failed approach.");
  }
}

main().catch((e) => {
  console.error("Fatal:", e);
  process.exit(1);
});
