/**
 * Orchestrator Integration Test
 *
 * Runs the same goals through both runGoal() (flat loop) and orchestrate()
 * (hierarchical decomposition) against local MiniWoB HTML fixtures.
 * Compares success rate, steps, LLM calls, and latency.
 *
 * Usage: CEL_LLM_PROVIDER=gemini CEL_LLM_MODEL=gemini-2.5-flash \
 *        GEMINI_API_KEY=... npx tsx tests/orchestrator-test/run-test.ts
 */

import { config } from "dotenv";
import path from "path";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
config({ path: path.join(__dirname, "..", "..", "benchmarks", ".env") });

// Configure LLM env vars
const apiKey = process.env.GOOGLE_GEMINI_API_KEY || process.env.GEMINI_API_KEY || "";
process.env.CEL_LLM_PROVIDER = "gemini";
process.env.CEL_LLM_MODEL = process.env.CEL_LLM_MODEL || "gemini-2.5-flash";
process.env.CEL_LLM_API_KEY = apiKey;

import { Cel, runGoal, orchestrate } from "../../agent/src/index.js";
import { BrowserAdapter } from "../../adapters/browser/src/index.js";
import type { GoalRunnerCallbacks } from "../../agent/src/goal-runner/config.js";
import type { ScreenContext } from "../../agent/src/types.js";

// ── Test fixtures ────────────────────────────────────────────────────────────

const FIXTURES_DIR = path.join(__dirname, "..", "..", "benchmarks", "fixtures");

interface TestCase {
  id: string;
  url: string;
  goal: string;
  verify: (adapter: BrowserAdapter) => Promise<boolean>;
  category: "simple" | "medium" | "complex";
}

const TESTS: TestCase[] = [
  {
    id: "simple-form-submit",
    url: `file://${FIXTURES_DIR}/simple-form.html`,
    goal: "Fill in the name field with 'John Doe' and the email field with 'john@example.com', then click Submit",
    verify: async (adapter) => {
      try {
        const result = await adapter.evaluate("document.querySelector('.success')?.textContent || ''");
        return String(result).includes("success") || String(result).includes("submitted");
      } catch { return false; }
    },
    category: "medium",
  },
  {
    id: "click-button",
    url: `file://${FIXTURES_DIR}/simple-form.html`,
    goal: "Click the Submit button",
    verify: async () => true, // just testing that it executes without error
    category: "simple",
  },
];

// ── Runner ───────────────────────────────────────────────────────────────────

interface TestResult {
  testId: string;
  mode: "runGoal" | "orchestrate";
  success: boolean;
  status: string;
  steps: number;
  llmCalls: number;
  latencyMs: number;
  summary: string;
  subTasks?: number;
  error?: string;
}

async function runTest(
  cel: Cel,
  adapter: BrowserAdapter,
  test: TestCase,
  mode: "runGoal" | "orchestrate",
): Promise<TestResult> {
  const start = Date.now();

  // Navigate to test page
  await adapter.navigate(test.url);
  await new Promise((r) => setTimeout(r, 1000)); // wait for page load

  // Build callbacks from adapter
  const callbacks: GoalRunnerCallbacks = {
    getContext: async (): Promise<ScreenContext> => {
      return adapter.getContext();
    },
    screenshot: async () => adapter.screenshot(),
    stateFingerprint: () => {
      try { return adapter.getPageUrl(); } catch { return ""; }
    },
    executeAction: async (action, context) => {
      return adapter.executeAction(action, context);
    },
  };

  try {
    if (mode === "runGoal") {
      const result = await runGoal(
        cel,
        {
          goal: test.goal,
          maxSteps: 15,
          taskTimeout: 60000,
          enableVision: false,
          selfHeal: true,
          skipRouter: true,
        },
        callbacks,
      );

      const verified = await test.verify(adapter);
      return {
        testId: test.id,
        mode,
        success: result.status === "achieved" || verified,
        status: result.status,
        steps: result.totalSteps,
        llmCalls: result.metrics?.llmCalls ?? 0,
        latencyMs: Date.now() - start,
        summary: result.summary,
      };
    } else {
      const result = await orchestrate(
        cel,
        {
          goal: test.goal,
          maxTotalSteps: 15,
          maxReplans: 2,
          subAgentConfig: {
            maxSteps: 10,
            taskTimeout: 60000,
            enableVision: false,
            selfHeal: true,
            skipRouter: true,
            validator: { enabled: true },
          },
        },
        callbacks,
      );

      const verified = await test.verify(adapter);
      return {
        testId: test.id,
        mode,
        success: result.status === "achieved" || verified,
        status: result.status,
        steps: result.totalSteps,
        llmCalls: result.metrics?.llmCalls ?? 0,
        latencyMs: Date.now() - start,
        summary: result.summary,
        subTasks: result.subTasks.length,
      };
    }
  } catch (e) {
    return {
      testId: test.id,
      mode,
      success: false,
      status: "error",
      steps: 0,
      llmCalls: 0,
      latencyMs: Date.now() - start,
      summary: "",
      error: String(e).slice(0, 200),
    };
  }
}

// ── Main ─────────────────────────────────────────────────────────────────────

async function main() {
  console.log("=== Orchestrator vs runGoal Integration Test ===\n");

  if (!apiKey) {
    console.error("No API key found. Set GOOGLE_GEMINI_API_KEY or GEMINI_API_KEY");
    process.exit(1);
  }

  const cel = new Cel();
  console.log(`CEL native: ${cel.isNativeAvailable ? "yes" : "no"}`);

  const adapter = new BrowserAdapter({
    cel,
    browser: "chromium",
    useCdp: true,
    headless: true,
    stealth: false,
    viewport: { width: 1280, height: 800 },
    sanitize: true,
  });

  await adapter.connect();
  console.log("Browser connected.\n");

  const results: TestResult[] = [];

  for (const test of TESTS) {
    console.log(`── ${test.id} (${test.category}) ──`);

    // Run with runGoal (flat loop)
    process.stdout.write("  runGoal:     ");
    const rgResult = await runTest(cel, adapter, test, "runGoal");
    console.log(
      `${rgResult.success ? "PASS" : "FAIL"} | ${rgResult.steps} steps | ${rgResult.llmCalls} LLM calls | ${rgResult.latencyMs}ms` +
      (rgResult.error ? ` | ERROR: ${rgResult.error.slice(0, 80)}` : ""),
    );
    results.push(rgResult);

    // Navigate away to reset state
    await adapter.navigate("about:blank");
    await new Promise((r) => setTimeout(r, 500));

    // Run with orchestrate (hierarchical)
    process.stdout.write("  orchestrate: ");
    const oResult = await runTest(cel, adapter, test, "orchestrate");
    console.log(
      `${oResult.success ? "PASS" : "FAIL"} | ${oResult.steps} steps | ${oResult.llmCalls} LLM calls | ${oResult.latencyMs}ms | ${oResult.subTasks} sub-tasks` +
      (oResult.error ? ` | ERROR: ${oResult.error.slice(0, 80)}` : ""),
    );
    results.push(oResult);

    // Reset
    await adapter.navigate("about:blank");
    await new Promise((r) => setTimeout(r, 500));

    console.log();
  }

  // ── Summary ──────────────────────────────────────────────────────────
  console.log("\n=== SUMMARY ===\n");

  const rgResults = results.filter((r) => r.mode === "runGoal");
  const oResults = results.filter((r) => r.mode === "orchestrate");

  const rgPass = rgResults.filter((r) => r.success).length;
  const oPass = oResults.filter((r) => r.success).length;

  const rgAvgSteps = rgResults.reduce((a, r) => a + r.steps, 0) / rgResults.length;
  const oAvgSteps = oResults.reduce((a, r) => a + r.steps, 0) / oResults.length;

  const rgAvgCalls = rgResults.reduce((a, r) => a + r.llmCalls, 0) / rgResults.length;
  const oAvgCalls = oResults.reduce((a, r) => a + r.llmCalls, 0) / oResults.length;

  const rgAvgLatency = rgResults.reduce((a, r) => a + r.latencyMs, 0) / rgResults.length;
  const oAvgLatency = oResults.reduce((a, r) => a + r.latencyMs, 0) / oResults.length;

  console.log("| Metric | runGoal | orchestrate | Delta |");
  console.log("|--------|---------|-------------|-------|");
  console.log(`| Success rate | ${rgPass}/${rgResults.length} | ${oPass}/${oResults.length} | ${oPass - rgPass >= 0 ? "+" : ""}${oPass - rgPass} |`);
  console.log(`| Avg steps | ${rgAvgSteps.toFixed(1)} | ${oAvgSteps.toFixed(1)} | ${(oAvgSteps - rgAvgSteps).toFixed(1)} |`);
  console.log(`| Avg LLM calls | ${rgAvgCalls.toFixed(1)} | ${oAvgCalls.toFixed(1)} | ${(oAvgCalls - rgAvgCalls).toFixed(1)} |`);
  console.log(`| Avg latency | ${(rgAvgLatency / 1000).toFixed(1)}s | ${(oAvgLatency / 1000).toFixed(1)}s | ${((oAvgLatency - rgAvgLatency) / 1000).toFixed(1)}s |`);

  await adapter.disconnect();
}

main().catch((e) => {
  console.error("Fatal:", e);
  process.exit(1);
});
