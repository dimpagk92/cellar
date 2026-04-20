import { Command } from "commander";
import { Cel } from "@cellar/agent";

export const historyCommand = new Command("history")
  .description("Show workflow run history from the CEL Store")
  .option("-n, --limit <count>", "Number of runs to show", "20")
  .option("--run <id>", "Show step details for a specific run ID")
  .action((opts: { limit: string; run?: string }) => {
    const cel = new Cel();

    if (opts.run) {
      const runId = parseInt(opts.run, 10);
      const steps = cel.getStepResults(runId);
      if (steps.length === 0) {
        console.log(`No step results found for run #${runId}`);
        return;
      }
      console.log(`\nSteps for run #${runId}:\n`);
      console.log(
        "  #   Step ID              Action          Success  Confidence  Time",
      );
      console.log("  " + "-".repeat(78));
      for (const step of steps) {
        const actionType = (() => {
          try {
            return JSON.parse(step.action).type ?? "?";
          } catch {
            return step.action;
          }
        })();
        const status = step.success ? "yes" : "FAIL";
        const err = step.error ? `  Error: ${step.error}` : "";
        console.log(
          `  ${String(step.step_index).padStart(2)}  ${step.step_id.padEnd(18)}  ${actionType.padEnd(14)}  ${status.padEnd(7)}  ${step.confidence.toFixed(2).padStart(10)}  ${step.executed_at}${err}`,
        );
      }
      return;
    }

    const history = cel.getRunHistory(parseInt(opts.limit, 10));
    if (history.length === 0) {
      console.log("No run history found.");
      return;
    }

    console.log("\nRecent workflow runs:\n");
    console.log(
      "  ID  Workflow             Status      Steps       Interventions  Started",
    );
    console.log("  " + "-".repeat(82));
    for (const run of history) {
      console.log(
        `  ${String(run.id).padStart(3)}  ${run.workflow_name.padEnd(18)}  ${run.status.padEnd(10)}  ${run.steps_completed}/${run.steps_total}`.padEnd(
          60,
        ) +
          `  ${String(run.interventions).padStart(13)}  ${run.started_at}`,
      );
    }
    console.log(`\nUse "dilipod history --run <ID>" to see step details.`);
  });
