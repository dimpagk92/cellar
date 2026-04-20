import { Command } from "commander";
import { Cel, ensureDedicatedCdpBrowser } from "@cellar/agent";

interface RunGoalOptions {
  maxSteps: number;
  timeoutMs: number;
  vision: boolean;
  selfHeal: boolean;
  decompose: boolean;
  notebook: boolean;
  json: boolean;
}

/**
 * Heuristic: does this goal want a browser? A goal is browser-ish if it
 * references http(s)://, a top-level domain, or common browser verbs.
 * Keep loose — false positives just cost a ~50ms no-op ensure check;
 * false negatives cost the full deterministic-path failure we saw on
 * the HN eval smoke.
 */
function looksBrowserGoal(goal: string): boolean {
  const lower = goal.toLowerCase();
  if (/\bhttps?:\/\//.test(lower)) return true;
  if (/\b(navigate|open|visit|browse|go to|extract from|read)\b/.test(lower)) {
    return true;
  }
  if (/\.(com|org|net|io|dev|app|co\.|edu|gov|ai)\b/.test(lower)) return true;
  return false;
}

interface GoalResult {
  status?: string;
  summary?: string;
  total_steps?: number;
  duration_ms?: number;
  metrics?: {
    llm_calls?: number;
    vision_calls?: number;
    action_successes?: number;
    action_failures?: number;
    replans?: number;
    total_llm_tokens?: number;
    stale_targets?: number;
    refreshes?: number;
  };
}

export const runGoalCommand = new Command("run-goal")
  .description("Execute a natural-language goal (routes through the full Rust goal-runner)")
  .argument("<goal>", "Natural-language goal, quoted")
  .option("--max-steps <n>", "Maximum steps before giving up", (v) => parseInt(v, 10), 30)
  .option("--timeout-ms <n>", "Total timeout in ms", (v) => parseInt(v, 10), 120_000)
  .option("--no-vision", "Disable vision fallback")
  .option("--no-self-heal", "Disable self-healing retries")
  .option("--decompose", "Enable milestone decomposition", false)
  .option("--no-notebook", "Disable notebook persistence")
  .option("--json", "Output raw GoalResult JSON", false)
  .action(async (goal: string, opts: RunGoalOptions) => {
    const cel = new Cel();
    if (!cel.isNativeAvailable) {
      console.error(
        "CEL native module not available. Build it with `cargo build -p cel-napi` and copy the .node file into cel/cel-napi/.",
      );
      process.exit(1);
    }

    const backend = process.env.CEL_RUNTIME_BACKEND ?? "local";
    const remoteUrl = process.env.CEL_RUNTIME_URL;

    // Local backend: boot Cortex in-process (mirrors what the MCP server does).
    // Remote backend: the worker boots its own Cortex; we just dispatch over HTTP.
    if (backend === "local" && !cel.isCortexRunning()) {
      console.error("Booting Cortex...");
      cel.bootCortex();
    }

    // Goals that mention a URL or navigation will hit the deterministic
    // fast path, which needs a live CEL CDP browser. Eagerly ensure one
    // when the goal looks browser-y — idempotent if the browser is
    // already running (just verifies and returns). Non-browser goals
    // (e.g. native desktop tasks) skip this so the TCC prompt and
    // process-launch latency don't burn budget on them.
    if (backend === "local" && looksBrowserGoal(goal)) {
      try {
        const ensure = await ensureDedicatedCdpBrowser({ cel });
        if (!ensure.ok) {
          console.error(`Warning: could not ensure CEL browser: ${ensure.message}`);
        }
      } catch (err) {
        console.error(
          `Warning: CEL browser ensure threw: ${err instanceof Error ? err.message : err}`,
        );
      }
    }

    console.error(`Goal:    ${goal}`);
    console.error(
      `Backend: ${backend}${backend === "remote" && remoteUrl ? ` (${remoteUrl})` : ""}`,
    );

    const start = Date.now();
    try {
      const result = (await cel.runGoalRust({
        goal,
        max_steps: opts.maxSteps,
        timeout_ms: opts.timeoutMs,
        enable_vision: opts.vision,
        self_heal: opts.selfHeal,
        enable_decomposition: opts.decompose,
        enable_notebook: opts.notebook,
      })) as GoalResult;
      const wallMs = Date.now() - start;

      if (opts.json) {
        console.log(JSON.stringify(result, null, 2));
        return;
      }

      console.log("");
      console.log("=== Result ===");
      console.log(`Status:   ${result.status ?? "(unknown)"}`);
      console.log(`Steps:    ${result.total_steps ?? 0}`);
      console.log(`Duration: ${result.duration_ms ?? wallMs}ms (wall: ${wallMs}ms)`);
      console.log(`Summary:  ${result.summary ?? ""}`);
      const m = result.metrics ?? {};
      const successes = m.action_successes ?? 0;
      const failures = m.action_failures ?? 0;
      console.log(
        `Metrics:  llm=${m.llm_calls ?? 0} vision=${m.vision_calls ?? 0} actions=${successes}/${successes + failures} replans=${m.replans ?? 0}`,
      );

      if (result.status && result.status !== "Achieved") {
        process.exit(1);
      }
    } catch (err) {
      console.error(`Goal execution failed: ${err instanceof Error ? err.message : err}`);
      process.exit(1);
    }
  });
