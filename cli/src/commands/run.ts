import { Command } from "commander";
import * as fs from "node:fs";
import * as path from "node:path";
import {
  Cel,
  WorkflowEngine,
  RunTranscript,
  loadWorkflow,
  importWorkflow,
  executeAction,
  processPostRun,
  type EngineCallbacks,
  type ScreenContext,
  type AssembledContext,
  type StepResult,
} from "@cellar/agent";

export const runCommand = new Command("run")
  .description("Execute a workflow")
  .argument("<workflow>", "Workflow name, JSON file path, or .dilipod file")
  .option("--priority <level>", "Queue priority (low|normal|high|critical)", "normal")
  .option("--dry-run", "Validate without executing")
  .option("--no-transcript", "Disable JSONL transcript logging")
  .option("--evict", "Run data eviction before starting")
  .action((workflowArg: string, opts: { priority: string; dryRun?: boolean; transcript?: boolean; evict?: boolean }) => {
    // Resolve workflow source
    let workflowPath = workflowArg;
    if (!fs.existsSync(workflowPath)) {
      // Try default workflows directory
      const home = process.env.HOME ?? ".";
      const defaultPath = path.join(home, ".cellar", "workflows", `${workflowArg}.json`);
      if (fs.existsSync(defaultPath)) {
        workflowPath = defaultPath;
      } else {
        console.error(`Workflow not found: ${workflowArg}`);
        console.error("Provide a file path or a workflow name in ~/.cellar/workflows/");
        process.exit(1);
      }
    }

    const workflow = workflowPath.endsWith(".dilipod")
      ? importWorkflow(workflowPath)
      : loadWorkflow(workflowPath);

    console.log(`Workflow: ${workflow.name}`);
    console.log(`  Steps: ${workflow.steps.length}`);
    console.log(`  App: ${workflow.app}`);

    if (opts.dryRun) {
      console.log("\nDry run — workflow validated successfully.");
      return;
    }

    const cel = new Cel();
    if (!cel.isNativeAvailable) {
      console.error("Error: CEL native module not available.");
      process.exit(1);
    }

    // Optional: run eviction before starting
    if (opts.evict) {
      const evicted = cel.runEviction();
      const total = evicted.superseded_observations + evicted.old_runs + evicted.old_knowledge;
      if (total > 0) {
        console.log(`  Evicted ${total} old records (${evicted.old_runs} runs, ${evicted.superseded_observations} observations, ${evicted.old_knowledge} knowledge)`);
      }
    }

    const runId = cel.startRun(workflow.name, workflow.steps.length);
    console.log(`\nStarting run #${runId} with priority ${opts.priority}...`);

    // Initialize transcript (unless disabled)
    const transcript = opts.transcript !== false ? new RunTranscript(runId) : undefined;
    transcript?.logRunStart(workflow.name, workflow.steps.length);

    let lastContext: ScreenContext = { app: "", window: "", elements: [], timestamp_ms: 0 };

    const callbacks: EngineCallbacks = {
      getContext: async () => {
        lastContext = cel.getContext();
        return lastContext;
      },
      executeAction: async (step, assembled) => {
        console.log(`  [${step.id}] ${step.description}`);

        // Log context capture to transcript
        const stepIdx = workflow.steps.indexOf(step);
        transcript?.logContextCapture(stepIdx, step.id, assembled);

        const maxConf = lastContext.elements.length > 0
          ? Math.max(...lastContext.elements.map((e) => e.confidence))
          : 0;
        try {
          const success = await executeAction(cel, step, lastContext);
          cel.logStep(
            runId,
            stepIdx,
            step.id,
            JSON.stringify(step.action),
            success,
            maxConf,
            JSON.stringify({ app: lastContext.app, window: lastContext.window, elementCount: lastContext.elements.length }),
          );

          if (success) {
            transcript?.logStepComplete(stepIdx, step.id, maxConf);
          } else {
            transcript?.logStepFailed(stepIdx, step.id, "Action returned false", maxConf);
          }

          return success;
        } catch (err) {
          const errMsg = err instanceof Error ? err.message : String(err);
          cel.logStep(
            runId,
            stepIdx,
            step.id,
            JSON.stringify(step.action),
            false,
            maxConf,
            undefined,
            errMsg,
          );
          transcript?.logStepFailed(stepIdx, step.id, errMsg, maxConf);
          throw err;
        }
      },
      onPause: async (step, assembled) => {
        const stepIdx = workflow.steps.indexOf(step);
        console.log(`  PAUSED at step ${step.id}: low confidence`);
        console.log("  Waiting 3s before retrying (Ctrl+C to abort)...");
        transcript?.logPaused(stepIdx, step.id, 0);
        await new Promise((resolve) => setTimeout(resolve, 3000));
      },
      onStepComplete: (step, idx) => {
        console.log(`  Step ${idx + 1}/${workflow.steps.length}: ${step.description}`);
      },
      onComplete: (wf, status, steps) => {
        cel.finishRun(runId, status as "completed" | "failed");

        // Log transcript completion
        transcript?.logRunComplete(status, steps);

        // Post-run hooks: extract observations, update working memory
        const postResult = processPostRun(cel, wf.name, runId, steps, transcript);
        console.log(`\nWorkflow "${wf.name}" ${status}.`);
        if (postResult.observationsCreated > 0) {
          console.log(`  Observations created: ${postResult.observationsCreated}`);
        }
        if (postResult.knowledgeAdded > 0) {
          console.log(`  Knowledge extracted: ${postResult.knowledgeAdded}`);
        }
        if (postResult.workingMemoryUpdated) {
          console.log(`  Working memory updated`);
        }
        if (transcript) {
          console.log(`  Transcript: ${transcript.getPath()}`);
        }

        engine.stop();
      },
      onLog: (level, msg) => {
        if (level === "error") console.error(`  [ERROR] ${msg}`);
        else if (level === "warn") console.warn(`  [WARN] ${msg}`);
      },
    };

    const engine = new WorkflowEngine(callbacks, { cel });
    engine.submit(workflow, opts.priority as "low" | "normal" | "high" | "critical");
    engine.start().catch((err) => {
      console.error(`Engine error: ${err}`);
      process.exit(1);
    });
  });
