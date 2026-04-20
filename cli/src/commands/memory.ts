import { Command } from "commander";
import { Cel } from "@cellar/agent";

export const memoryCommand = new Command("memory")
  .description("Inspect and manage the agent memory layer");

memoryCommand
  .command("show")
  .description("Show working memory for a workflow")
  .argument("<workflow>", "Workflow name")
  .action((workflowName: string) => {
    const cel = new Cel();
    const wm = cel.getWorkingMemory(workflowName);
    if (!wm) {
      console.log(`No working memory for "${workflowName}".`);
      return;
    }
    console.log(`\nWorking memory for "${workflowName}":\n`);
    console.log(wm);
  });

memoryCommand
  .command("observations")
  .description("List observations for a workflow")
  .argument("<workflow>", "Workflow name")
  .option("-n, --limit <count>", "Max observations to show", "20")
  .action((workflowName: string, opts: { limit: string }) => {
    const cel = new Cel();
    const obs = cel.getObservations(workflowName, parseInt(opts.limit, 10));
    if (obs.length === 0) {
      console.log(`No observations for "${workflowName}".`);
      return;
    }
    console.log(`\nObservations for "${workflowName}" (${obs.length}):\n`);
    for (const o of obs) {
      const priority = o.priority.toUpperCase().padEnd(6);
      console.log(`  [${priority}] ${o.content}`);
      console.log(`           created: ${o.created_at}  runs: ${o.source_run_ids}`);
    }
  });

memoryCommand
  .command("search")
  .description("Search the knowledge store")
  .argument("<query>", "Search query")
  .option("-w, --workflow <name>", "Scope to a specific workflow")
  .option("-n, --limit <count>", "Max results", "5")
  .action((query: string, opts: { workflow?: string; limit: string }) => {
    const cel = new Cel();
    const results = cel.searchKnowledge(query, opts.workflow, parseInt(opts.limit, 10));
    if (results.length === 0) {
      console.log("No knowledge found.");
      return;
    }
    console.log(`\nKnowledge results for "${query}":\n`);
    for (const r of results) {
      const scope = r.workflow_scope ? ` [${r.workflow_scope}]` : " [global]";
      console.log(`  (${r.score.toFixed(2)})${scope} ${r.content}`);
      console.log(`          source: ${r.source}  created: ${r.created_at}`);
    }
  });

memoryCommand
  .command("evict")
  .description("Run TTL eviction to clean old data")
  .option("--run-days <days>", "Keep runs newer than N days", "90")
  .option("--knowledge-days <days>", "Keep knowledge newer than N days", "365")
  .action((opts: { runDays: string; knowledgeDays: string }) => {
    const cel = new Cel();
    const result = cel.runEviction(
      parseInt(opts.runDays, 10),
      parseInt(opts.knowledgeDays, 10),
    );
    const total = result.superseded_observations + result.old_runs + result.old_knowledge;
    if (total === 0) {
      console.log("Nothing to evict.");
      return;
    }
    console.log(`\nEviction complete:`);
    if (result.old_runs > 0) console.log(`  Runs deleted: ${result.old_runs}`);
    if (result.superseded_observations > 0) console.log(`  Superseded observations deleted: ${result.superseded_observations}`);
    if (result.old_knowledge > 0) console.log(`  Old knowledge deleted: ${result.old_knowledge}`);
    console.log(`  Total: ${total} records`);
  });

memoryCommand
  .command("reset")
  .description("Clear working memory for a workflow")
  .argument("<workflow>", "Workflow name")
  .action((workflowName: string) => {
    const cel = new Cel();
    cel.updateWorkingMemory(workflowName, "");
    console.log(`Working memory cleared for "${workflowName}".`);
  });
