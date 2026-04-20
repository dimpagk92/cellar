/**
 * A/B test for replan prompt variants.
 * Tests: quality of alternative strategies, conciseness, and actionability.
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

// ── Scenarios (same as replan test) ──────────────────────────────────────────

const SCENARIOS = [
  {
    id: "button-not-found",
    goal: "Submit the contact form",
    failedTask: "Click the Submit button",
    failureReason: "Element 'Submit' not found in context. Available: [Save, Cancel, Reset]",
    completedTasks: ["Fill in name 'John' and email 'john@test.com' in the form"],
    context: 'App: Chromium, Window: "Contact Form", Elements: 8',
  },
  {
    id: "search-failed",
    goal: "Find the quarterly report and download it",
    failedTask: "Type 'quarterly report' in the search box and press Enter",
    failureReason: "No search box found on page. The page shows a file browser with folders.",
    completedTasks: [],
    context: 'App: Chromium, Window: "Documents", Elements: 15',
  },
  {
    id: "login-wrong-page",
    goal: "Log in and navigate to the dashboard",
    failedTask: "Type 'admin' in username and 'pass123' in password, click Login",
    failureReason: "No username or password field found. Page shows 'Sign in with Google' button and 'Use SSO' link.",
    completedTasks: [],
    context: 'App: Chromium, Window: "Login", Elements: 5',
  },
  {
    id: "navigation-blocked",
    goal: "Open Settings and enable dark mode",
    failedTask: "Click the Settings menu item",
    failureReason: "Settings menu not visible. A cookie consent dialog is blocking the page.",
    completedTasks: [],
    context: 'App: Chromium, Window: "App", Elements: 12',
  },
  {
    id: "element-moved",
    goal: "Delete the selected emails",
    failedTask: "Click the Delete button in the toolbar",
    failureReason: "Delete button not in toolbar. Toolbar shows: Archive, Spam, Move, More...",
    completedTasks: ["Select all emails from 'Newsletter'"],
    context: 'App: Chromium, Window: "Gmail", Elements: 20',
  },
];

// ── Prompt variants ──────────────────────────────────────────────────────────

interface PromptVariant {
  id: string;
  name: string;
  system: string;
  user: (s: typeof SCENARIOS[0]) => string;
}

const VARIANTS: PromptVariant[] = [
  // A: Current production prompt
  {
    id: "A-current",
    name: "Current production",
    system: [
      "You are a replanning agent. A sub-task failed. Produce a NEW set of remaining tasks",
      "using a COMPLETELY DIFFERENT approach. Do not repeat the failed approach.",
      "",
      "Respond with JSON only:",
      '{ "tasks": [{ "id": "t1", "description": "...", "depends_on": [] }, ...] }',
    ].join("\n"),
    user: (s) => [
      `Original goal: ${s.goal}`,
      "",
      "Completed so far:",
      s.completedTasks.length > 0
        ? s.completedTasks.map((t) => `- [DONE] ${t}`).join("\n")
        : "No tasks completed yet.",
      "",
      `Failed task: "${s.failedTask}"`,
      `Failure reason: ${s.failureReason}`,
      "",
      `Current screen: ${s.context}`,
      "",
      "Produce a new plan with a different approach for the remaining work.",
    ].join("\n"),
  },

  // B: Concise + same-screen merging (apply our decomposition lessons)
  {
    id: "B-concise",
    name: "Concise + same-screen merging",
    system: [
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
    ].join("\n"),
    user: (s) => [
      `Goal: ${s.goal}`,
      s.completedTasks.length > 0 ? `Done: ${s.completedTasks.join("; ")}` : "",
      `Failed: "${s.failedTask}" — ${s.failureReason}`,
      `Screen: ${s.context}`,
    ].filter(Boolean).join("\n"),
  },

  // C: Failure-analysis focused (explain WHY it failed, then plan)
  {
    id: "C-analysis",
    name: "Failure analysis + plan",
    system: [
      "You are a replanning agent. Analyze why the previous approach failed, then produce a better plan.",
      "",
      "RULES:",
      "- First identify what went wrong (element missing, wrong page, blocked by popup, etc.)",
      "- Then produce tasks that avoid the same problem",
      "- Same screen = ONE task",
      "- Start with imperative verbs, be specific",
      "- Minimum tasks. Aim for 1-2.",
      "",
      "JSON only:",
      '{ "analysis": "brief explanation of failure", "tasks": [{ "id": "t1", "description": "...", "depends_on": [] }, ...] }',
    ].join("\n"),
    user: (s) => [
      `Goal: ${s.goal}`,
      s.completedTasks.length > 0 ? `Done: ${s.completedTasks.join("; ")}` : "",
      `Failed: "${s.failedTask}" — ${s.failureReason}`,
      `Screen: ${s.context}`,
    ].filter(Boolean).join("\n"),
  },
];

// ── Evaluation ───────────────────────────────────────────────────────────────

interface Result {
  variantId: string;
  scenarioId: string;
  taskCount: number;
  tasks: string[];
  isDifferent: boolean;
  isActionable: boolean;
  latencyMs: number;
}

async function runVariant(cel: Cel, variant: PromptVariant, scenario: typeof SCENARIOS[0]): Promise<Result> {
  const start = Date.now();
  try {
    const response = await cel.llmCompleteWithRole(variant.system, variant.user(scenario), "orchestrator", 2048);
    const latencyMs = Date.now() - start;

    const match = response.match(/\{[\s\S]*\}/);
    if (!match) return { variantId: variant.id, scenarioId: scenario.id, taskCount: 0, tasks: [], isDifferent: false, isActionable: false, latencyMs };

    const parsed = JSON.parse(match[0]);
    const tasks = (parsed.tasks ?? []).map((t: { description: string }) => t.description);

    const isDifferent = tasks.length > 0 && !tasks[0].toLowerCase().includes(scenario.failedTask.toLowerCase().slice(0, 20));

    const actionVerbs = ["click", "type", "navigate", "open", "find", "select", "copy", "paste", "press", "scroll", "wait", "accept", "dismiss", "search", "check"];
    const isActionable = tasks.length > 0 && tasks.every((t: string) =>
      actionVerbs.some((v) => t.toLowerCase().startsWith(v) || t.toLowerCase().includes(v))
    );

    return { variantId: variant.id, scenarioId: scenario.id, taskCount: tasks.length, tasks, isDifferent, isActionable, latencyMs };
  } catch {
    return { variantId: variant.id, scenarioId: scenario.id, taskCount: 0, tasks: [], isDifferent: false, isActionable: false, latencyMs: Date.now() - start };
  }
}

// ── Main ─────────────────────────────────────────────────────────────────────

async function main() {
  console.log("=== Replan Prompt A/B Test ===\n");
  const cel = new Cel();
  const allResults: Result[] = [];

  for (const variant of VARIANTS) {
    console.log(`\n── ${variant.id}: ${variant.name} ──`);

    for (const scenario of SCENARIOS) {
      process.stdout.write(`  ${scenario.id.padEnd(22)}`);
      const result = await runVariant(cel, variant, scenario);
      allResults.push(result);

      const status = result.isDifferent ? "DIFF" : "SAME";
      const actionable = result.isActionable ? "ACT" : "VAGUE";
      console.log(`${status} ${actionable} | ${result.taskCount} tasks | ${result.latencyMs}ms`);
      for (const t of result.tasks) {
        console.log(`    → ${t.slice(0, 85)}`);
      }
    }
  }

  // ── Summary ──────────────────────────────────────────────────────────
  console.log("\n\n=== SUMMARY ===\n");
  console.log("| Variant | Avg Tasks | Different | Actionable | Avg Latency |");
  console.log("|---------|-----------|-----------|------------|-------------|");

  for (const variant of VARIANTS) {
    const vResults = allResults.filter((r) => r.variantId === variant.id);
    const avgTasks = vResults.reduce((a, r) => a + r.taskCount, 0) / vResults.length;
    const diffCount = vResults.filter((r) => r.isDifferent).length;
    const actCount = vResults.filter((r) => r.isActionable).length;
    const avgLatency = vResults.reduce((a, r) => a + r.latencyMs, 0) / vResults.length;

    console.log(
      `| ${variant.id.padEnd(12)} | ${avgTasks.toFixed(1).padStart(5)}     | ${diffCount}/${vResults.length}       | ${actCount}/${vResults.length}        | ${avgLatency.toFixed(0).padStart(5)}ms     |`,
    );
  }
}

main().catch((e) => {
  console.error("Fatal:", e);
  process.exit(1);
});
