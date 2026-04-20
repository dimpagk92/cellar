import { Command } from "commander";
import * as path from "node:path";
import {
  listWorkflows,
  exportWorkflow as exportWf,
  importWorkflow as importWf,
  saveWorkflow,
} from "@cellar/agent";

export const workflowCommand = new Command("workflow")
  .description("Manage workflows");

workflowCommand
  .command("install <name>")
  .description("Install a workflow from the registry")
  .action((name: string) => {
    console.log(`Installing workflow: ${name}...`);
    // TODO: RegistryClient.download + install
    console.log("Registry not yet available. Use 'workflow import' with a .dilipod file.");
  });

workflowCommand
  .command("export <name>")
  .description("Export a workflow to a portable .dilipod file")
  .option("-o, --output <path>", "Output file path")
  .action((name: string, opts: { output?: string }) => {
    const workflows = listWorkflows();
    const workflow = workflows.find((w) => w.name === name);
    if (!workflow) {
      console.error(`Workflow '${name}' not found in ~/.cellar/workflows/`);
      process.exit(1);
    }
    const output = opts.output ?? `./${name}.dilipod`;
    exportWf(workflow, path.resolve(output));
    console.log(`Exported '${name}' to ${output}`);
  });

workflowCommand
  .command("import <file>")
  .description("Import a workflow from a .dilipod file")
  .action((file: string) => {
    try {
      const workflow = importWf(path.resolve(file));
      const saved = saveWorkflow(workflow);
      console.log(`Imported '${workflow.name}' to ${saved}`);
    } catch (err) {
      console.error(`Import failed: ${err}`);
      process.exit(1);
    }
  });

workflowCommand
  .command("list")
  .description("List installed workflows")
  .action(() => {
    const workflows = listWorkflows();
    if (workflows.length === 0) {
      console.log("No workflows installed. Use 'workflow import' to add one.");
      return;
    }
    console.log("Installed workflows:");
    for (const w of workflows) {
      console.log(`  ${w.name} — ${w.steps.length} steps (${w.app})`);
    }
  });
