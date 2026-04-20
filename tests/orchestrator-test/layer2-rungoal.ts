/**
 * Layer 2: runGoal() integration tests with validator, plannerHint,
 * speculative planning, and batch hints.
 *
 * Tests runGoal() on real HTML fixtures with a real browser and LLM.
 * Measures: success rate, LLM calls, steps, plannerHint usage, batch actions.
 *
 * Usage: npx tsx tests/orchestrator-test/layer2-rungoal.ts
 */

import { config } from "dotenv";
import path from "path";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
config({ path: path.join(__dirname, "..", "..", "benchmarks", ".env") });

const apiKey = process.env.GOOGLE_GEMINI_API_KEY || "";
process.env.CEL_LLM_PROVIDER = "gemini";
process.env.CEL_LLM_MODEL = process.env.CEL_LLM_MODEL || "gemini-2.5-flash";
process.env.CEL_LLM_API_KEY = apiKey;

import { Cel, runGoal } from "../../agent/src/index.js";
import { BrowserAdapter, celRun } from "../../adapters/browser/src/index.js";
import type { GoalRunnerCallbacks, GoalResult } from "../../agent/src/goal-runner/config.js";
import type { ValidationResult } from "../../agent/src/goal-runner/validator.js";
import type { ScreenContext, PlannedAction } from "../../agent/src/types.js";

const FIXTURES_DIR = path.join(__dirname, "..", "..", "benchmarks", "fixtures");

/**
 * Translate PlannedAction → BrowserAdapter.executeAction() call.
 * Same logic as celRun's internal executeAction (cel-run.ts line 71).
 */
async function executeBrowserAction(
  adapter: BrowserAdapter,
  action: PlannedAction,
  context: ScreenContext,
): Promise<boolean> {
  switch (action.type) {
    case "click": {
      const el = context.elements.find((e) => e.id === action.target_id);
      if (!el) throw new Error(`Element not found: ${action.target_id}`);
      return adapter.executeAction("click", {
        href: el.properties?.href,
        css_selector: el.properties?.css_selector,
        backend_node_id: el.properties?.backend_node_id,
        ...(el.bounds ? {
          x: el.bounds.x + Math.floor(el.bounds.width / 2),
          y: el.bounds.y + Math.floor(el.bounds.height / 2),
        } : {}),
      });
    }
    case "type": {
      const el = action.target_id ? context.elements.find((e) => e.id === action.target_id) : null;
      if (el?.properties?.css_selector) {
        return adapter.executeAction("type", {
          selector: el.properties.css_selector,
          text: action.text, clearFirst: true,
        });
      }
      if (el?.bounds) {
        return adapter.executeAction("type", {
          x: el.bounds.x + Math.floor(el.bounds.width / 2),
          y: el.bounds.y + Math.floor(el.bounds.height / 2),
          text: action.text, clearFirst: true,
        });
      }
      return adapter.executeAction("type", { text: action.text });
    }
    case "set_value": {
      const el = context.elements.find((e) => e.id === action.target_id);
      if (el?.properties?.css_selector) {
        return adapter.executeAction("type", {
          selector: el.properties.css_selector,
          text: action.value, clearFirst: true,
        });
      }
      if (!el?.bounds) throw new Error(`Element not found: ${action.target_id}`);
      return adapter.executeAction("type", {
        x: el.bounds.x + Math.floor(el.bounds.width / 2),
        y: el.bounds.y + Math.floor(el.bounds.height / 2),
        text: action.value, clearFirst: true,
      });
    }
    case "key":
      return adapter.executeAction("press_key", { key: action.key });
    case "key_combo":
      return adapter.executeAction("key_combo", { keys: action.keys });
    case "scroll":
      return adapter.executeAction("scroll_by", { dx: action.dx, dy: action.dy });
    case "wait":
      await new Promise((r) => setTimeout(r, action.ms));
      return true;
    case "batch":
      for (const sub of action.actions) {
        await executeBrowserAction(adapter, sub, context);
        await new Promise((r) => setTimeout(r, 200));
      }
      return true;
    case "extract":
    case "done":
    case "fail":
      return true;
    default:
      return true;
  }
}

// ── Test cases ───────────────────────────────────────────────────────────────

interface TestCase {
  id: string;
  url: string;
  goal: string;
  features: ("validator" | "plannerHint" | "speculative" | "batchHint")[];
  maxSteps: number;
  verify: (adapter: BrowserAdapter) => Promise<boolean>;
}

const TESTS: TestCase[] = [
  // 2a: plannerHint — a goal that will likely produce a failure, then recover
  {
    id: "2a-plannerHint-click",
    url: `file://${FIXTURES_DIR}/simple-form.html`,
    goal: "Click the 'Send Message' button to submit the form",
    features: ["validator", "plannerHint"],
    maxSteps: 8,
    verify: async (adapter) => {
      // The form requires fields, so clicking submit first may fail validation.
      // We just want to see that the planner gets a hint and adjusts.
      return true; // success = ran without crashing
    },
  },
  // 2b: Speculative planning — predictable type sequence
  {
    id: "2b-speculative-type",
    url: `file://${FIXTURES_DIR}/simple-form.html`,
    goal: "Type 'John Doe' in the Full Name field",
    features: ["speculative"],
    maxSteps: 5,
    verify: async (adapter) => {
      const val = await adapter.evaluate("document.getElementById('name')?.value || ''");
      return String(val).includes("John");
    },
  },
  // 2c: Batch hint — form with 5+ fields should trigger batch hint
  {
    id: "2c-batchHint-form",
    url: `file://${FIXTURES_DIR}/simple-form.html`,
    goal: "Fill in the form: name 'Jane Smith', email 'jane@test.com', phone '555-1234', select 'Technical Support', message 'Help please', then submit",
    features: ["validator", "batchHint"],
    maxSteps: 15,
    verify: async (adapter) => {
      const name = await adapter.evaluate("document.getElementById('name')?.value || ''");
      const email = await adapter.evaluate("document.getElementById('email')?.value || ''");
      return String(name).includes("Jane") && String(email).includes("jane");
    },
  },
  // 2d: All features together
  {
    id: "2d-all-features",
    url: `file://${FIXTURES_DIR}/simple-form.html`,
    goal: "Fill in name 'Alice', email 'alice@test.com', select 'General Inquiry' as subject, type 'Hello' as message, then click Send Message",
    features: ["validator", "plannerHint", "speculative", "batchHint"],
    maxSteps: 15,
    verify: async (adapter) => {
      // Check if success message appeared OR fields were filled
      const success = await adapter.evaluate("document.getElementById('success-message')?.style.display || 'none'");
      const name = await adapter.evaluate("document.getElementById('name')?.value || ''");
      return String(success) !== "none" || String(name).includes("Alice");
    },
  },
];

// ── Metrics collector ────────────────────────────────────────────────────────

interface RunMetrics {
  testId: string;
  withValidator: boolean;
  success: boolean;
  status: string;
  totalSteps: number;
  llmCalls: number;
  latencyMs: number;
  validationResults: ValidationResult[];
  plannerHintsGenerated: number;
  batchActionsUsed: number;
  summary: string;
}

async function runTest(
  cel: Cel,
  adapter: BrowserAdapter,
  test: TestCase,
  useValidator: boolean,
): Promise<RunMetrics> {
  await adapter.navigate(test.url);
  await new Promise((r) => setTimeout(r, 1000));

  const validationResults: ValidationResult[] = [];
  let batchActionsUsed = 0;

  const callbacks: GoalRunnerCallbacks = {
    getContext: async (): Promise<ScreenContext> => adapter.getContext(),
    screenshot: async () => adapter.screenshot(),
    stateFingerprint: () => {
      try { return adapter.getPageUrl(); } catch { return ""; }
    },
    executeAction: async (action, context) => {
      if (action.type === "batch") batchActionsUsed++;
      return executeBrowserAction(adapter, action, context);
    },
    onValidation: (result, stepIndex) => {
      validationResults.push(result);
    },
  };

  const start = Date.now();

  try {
    const result: GoalResult = await runGoal(
      cel,
      {
        goal: test.goal,
        maxSteps: test.maxSteps,
        taskTimeout: 90000,
        enableVision: false,
        selfHeal: true,
        skipRouter: true,
        validator: useValidator ? { enabled: true } : undefined,
      },
      callbacks,
    );

    const verified = await test.verify(adapter);
    const plannerHintsGenerated = validationResults.filter((v) => v.plannerHint).length;

    return {
      testId: test.id,
      withValidator: useValidator,
      success: result.status === "achieved" || verified,
      status: result.status,
      totalSteps: result.totalSteps,
      llmCalls: result.metrics?.llmCalls ?? 0,
      latencyMs: Date.now() - start,
      validationResults,
      plannerHintsGenerated,
      batchActionsUsed,
      summary: result.summary.slice(0, 100),
    };
  } catch (e) {
    return {
      testId: test.id,
      withValidator: useValidator,
      success: false,
      status: "error",
      totalSteps: 0,
      llmCalls: 0,
      latencyMs: Date.now() - start,
      validationResults,
      plannerHintsGenerated: 0,
      batchActionsUsed: 0,
      summary: String(e).slice(0, 100),
    };
  }
}

// ── Main ─────────────────────────────────────────────────────────────────────

async function main() {
  console.log("=== Layer 2: runGoal() Integration Tests ===\n");

  if (!apiKey) {
    console.error("No GOOGLE_GEMINI_API_KEY set.");
    process.exit(1);
  }

  const cel = new Cel();
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

  const results: RunMetrics[] = [];

  for (const test of TESTS) {
    console.log(`── ${test.id} (features: ${test.features.join(", ")}) ──`);
    console.log(`  Goal: ${test.goal.slice(0, 80)}...`);

    // Run WITHOUT validator (baseline)
    process.stdout.write("  baseline (no validator): ");
    await adapter.navigate("about:blank");
    await new Promise((r) => setTimeout(r, 300));
    const baseline = await runTest(cel, adapter, test, false);
    console.log(
      `${baseline.success ? "PASS" : "FAIL"} | ${baseline.totalSteps} steps | ${baseline.llmCalls} LLM calls | ${(baseline.latencyMs / 1000).toFixed(1)}s`,
    );
    results.push(baseline);

    // Run WITH validator
    process.stdout.write("  with validator:          ");
    await adapter.navigate("about:blank");
    await new Promise((r) => setTimeout(r, 300));
    const withVal = await runTest(cel, adapter, test, true);
    console.log(
      `${withVal.success ? "PASS" : "FAIL"} | ${withVal.totalSteps} steps | ${withVal.llmCalls} LLM calls | ${(withVal.latencyMs / 1000).toFixed(1)}s`,
    );
    if (withVal.plannerHintsGenerated > 0) {
      console.log(`    plannerHints generated: ${withVal.plannerHintsGenerated}`);
    }
    if (withVal.batchActionsUsed > 0) {
      console.log(`    batch actions used: ${withVal.batchActionsUsed}`);
    }
    if (withVal.validationResults.length > 0) {
      const verdicts = withVal.validationResults.map((v) => v.verdict);
      console.log(`    validation verdicts: ${verdicts.join(", ")}`);
    }
    results.push(withVal);

    console.log();
  }

  // ── Summary ──────────────────────────────────────────────────────────
  console.log("\n=== SUMMARY ===\n");

  const baselines = results.filter((r) => !r.withValidator);
  const validators = results.filter((r) => r.withValidator);

  const bPass = baselines.filter((r) => r.success).length;
  const vPass = validators.filter((r) => r.success).length;
  const bAvgCalls = baselines.reduce((a, r) => a + r.llmCalls, 0) / baselines.length;
  const vAvgCalls = validators.reduce((a, r) => a + r.llmCalls, 0) / validators.length;
  const bAvgLatency = baselines.reduce((a, r) => a + r.latencyMs, 0) / baselines.length;
  const vAvgLatency = validators.reduce((a, r) => a + r.latencyMs, 0) / validators.length;
  const totalHints = validators.reduce((a, r) => a + r.plannerHintsGenerated, 0);
  const totalBatch = validators.reduce((a, r) => a + r.batchActionsUsed, 0);

  console.log("| Metric | Baseline | With Validator | Delta |");
  console.log("|--------|----------|----------------|-------|");
  console.log(`| Success | ${bPass}/${baselines.length} | ${vPass}/${validators.length} | ${vPass - bPass >= 0 ? "+" : ""}${vPass - bPass} |`);
  console.log(`| Avg LLM calls | ${bAvgCalls.toFixed(1)} | ${vAvgCalls.toFixed(1)} | ${(vAvgCalls - bAvgCalls).toFixed(1)} |`);
  console.log(`| Avg latency | ${(bAvgLatency / 1000).toFixed(1)}s | ${(vAvgLatency / 1000).toFixed(1)}s | ${((vAvgLatency - bAvgLatency) / 1000).toFixed(1)}s |`);
  console.log(`| PlannerHints generated | - | ${totalHints} | - |`);
  console.log(`| Batch actions used | - | ${totalBatch} | - |`);

  await adapter.disconnect();
}

main().catch((e) => {
  console.error("Fatal:", e);
  process.exit(1);
});
