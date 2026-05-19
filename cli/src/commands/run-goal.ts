import { Command } from "commander";
import { Cel, ensureDedicatedCdpBrowser } from "@cellar/agent/runtime";

// Canonical run-goal surface — the CLI is a thin shim over
// `CanonicalGoalRunner::run`. The only flags are budget limits; every
// former toggle (vision / self-heal / decompose / notebook) lives
// inside the canonical agent loop as implicit behavior. See
// docs/canonical-agent-plan.md.
interface RunGoalOptions {
  maxSteps: number;
  timeoutMs: number;
  json: boolean;
}

interface CanonicalResult {
  status?: string;
  summary?: string;
  extracted_data?: unknown;
  failure_report?: {
    failing_sub_goal: string;
    failing_step: string;
    attempts: string[];
  };
}

export const runGoalCommand = new Command("run-goal")
  .description("Execute a natural-language goal via the canonical Rust agent")
  .argument("<goal>", "Natural-language goal, quoted")
  .option("--max-steps <n>", "Maximum steps before giving up", (v) => parseInt(v, 10), 80)
  .option("--timeout-ms <n>", "Total timeout in ms", (v) => parseInt(v, 10), 900_000)
  .option("--json", "Output raw GoalOutcome JSON", false)
  .action(async (goal: string, opts: RunGoalOptions) => {
    const cel = new Cel();
    if (!cel.isNativeAvailable) {
      console.error(
        "CEL native module not available. Build it with `cargo build -p cel-napi --release` and copy the .node file into cel/cel-napi/.",
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

    // Always ensure the dedicated CEL browser when running locally.
    // Idempotent — cheap no-op when the browser is already up. Without
    // it, `connect_to_focused_app` would bind to whatever browser
    // happens to be frontmost (Safari, regular Chrome, etc.) for goals
    // whose prompt text doesn't name a URL.
    if (backend === "local") {
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
      const raw = await cel.runGoalRust({
        goal,
        max_steps: opts.maxSteps,
        timeout_ms: opts.timeoutMs,
      });
      const result = (typeof raw === "string" ? JSON.parse(raw) : raw) as CanonicalResult;
      const wallMs = Date.now() - start;

      if (opts.json) {
        console.log(JSON.stringify(result, null, 2));
        return;
      }

      console.log("");
      console.log("=== Result ===");
      console.log(`Status:   ${result.status ?? "(unknown)"}`);
      console.log(`Duration: ${wallMs}ms`);
      console.log(`Summary:  ${result.summary ?? ""}`);
      if (result.failure_report) {
        console.log(`Failing sub-goal: ${result.failure_report.failing_sub_goal}`);
        console.log(`Failing step:     ${result.failure_report.failing_step}`);
        for (const [i, msg] of result.failure_report.attempts.entries()) {
          console.log(`Attempt ${i + 1}: ${msg}`);
        }
      }

      if (result.status && result.status !== "Achieved") {
        process.exit(1);
      }
    } catch (err) {
      console.error(`Goal execution failed: ${err instanceof Error ? err.message : err}`);
      process.exit(1);
    }
  });
