import { randomUUID } from "crypto";

import { Command } from "commander";
import {
  Cel,
  CelLangGraphDriver,
  celConfig,
  createCellarReactAgent,
  ensureDedicatedCdpBrowser,
  extractFinalAgentText,
  hasConfiguredLlmAuth,
  serializeAgentMessages,
} from "@cellar/agent";

interface RunGoalLangGraphOptions {
  maxSteps: number;
  timeoutMs: number;
  json: boolean;
}

export const runGoalLangGraphCommand = new Command("run-goal-langgraph")
  .description("Execute a natural-language goal via the LangGraph-first TS runtime")
  .argument("<goal>", "Natural-language goal, quoted")
  .option("--max-steps <n>", "Maximum executed steps before the planner fails", (v) => parseInt(v, 10), 40)
  .option("--timeout-ms <n>", "Total timeout in ms", (v) => parseInt(v, 10), 300_000)
  .option("--json", "Output raw final state JSON", false)
  .action(async (goal: string, opts: RunGoalLangGraphOptions) => {
    const cel = new Cel();
    if (!cel.isNativeAvailable) {
      console.error(
        "CEL native module not available. Build it with `cargo build -p cel-napi --release` and copy the .node file into cel/cel-napi/.",
      );
      process.exit(1);
    }
    if (!celConfig.llmProvider) {
      console.error(
        "No LLM provider configured. Set CEL_LLM_PROVIDER and the matching API key, or configure ~/.cellar/config.toml first.",
      );
      process.exit(1);
    }
    if (!hasConfiguredLlmAuth()) {
      console.error(
        `LLM provider '${celConfig.llmProvider}' is configured, but no matching API credential was found. Set the provider API key in env or ~/.cellar/config.toml before running LangGraph goals.`,
      );
      process.exit(1);
    }

    let bootedHere = false;
    if (!cel.isCortexRunning()) {
      console.error("Booting Cortex...");
      cel.bootCortex();
      bootedHere = true;
      await new Promise((resolve) => setTimeout(resolve, 700));
    }

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

    const threadId = `langgraph-${randomUUID()}`;
    const driver = new CelLangGraphDriver(cel);
    const { agent, session } = createCellarReactAgent({
      driver,
      llm: cel,
      maxActions: opts.maxSteps,
      goal,
    });

    console.error(`Goal:      ${goal}`);
    console.error(`Runtime:   langgraph`);
    console.error(`Thread ID: ${threadId}`);

    const startedAt = Date.now();
    try {
      const result = await agent.invoke(
        {
          messages: [
            {
              role: "user",
              content: goal,
            },
          ],
        },
        {
          configurable: { thread_id: threadId },
          recursionLimit: Math.max(opts.maxSteps * 6, 60),
          signal: AbortSignal.timeout(opts.timeoutMs),
        },
      );
      const wallMs = Date.now() - startedAt;
      const finalText = extractFinalAgentText(result.messages);

      if (opts.json) {
        console.log(JSON.stringify({
          thread_id: threadId,
          duration_ms: wallMs,
          steps_used: session.executedSteps,
          final_text: finalText,
          messages: serializeAgentMessages(result.messages),
        }, null, 2));
        return;
      }

      console.log("");
      console.log("=== LangGraph Result ===");
      console.log(`Duration: ${wallMs}ms`);
      console.log(`Steps:    ${session.executedSteps} executed actions`);
      console.log(`Provider: ${celConfig.llmProvider}`);
      console.log("Status:   Completed");
      console.log(`Answer:   ${finalText || "(empty final answer)"}`);
    } catch (err) {
      console.error(`LangGraph run failed: ${err instanceof Error ? err.message : err}`);
      process.exit(1);
    } finally {
      if (bootedHere && cel.isCortexRunning()) {
        try {
          cel.stopCortex();
        } catch {
          // Best effort only.
        }
      }
    }
  });
