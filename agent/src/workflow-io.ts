import * as fs from "node:fs";
import * as path from "node:path";
import type { Workflow } from "./types.js";

/** Default directory for workflow storage. */
const WORKFLOWS_DIR =
  process.env.CELLAR_WORKFLOWS_DIR ??
  path.join(process.env.HOME ?? ".", ".cellar", "workflows");

/** Ensure the workflows directory exists. */
function ensureDir(dir: string): void {
  if (!fs.existsSync(dir)) {
    fs.mkdirSync(dir, { recursive: true });
  }
}

/** Save a workflow to disk as JSON. */
export function saveWorkflow(workflow: Workflow, dir = WORKFLOWS_DIR): string {
  ensureDir(dir);
  const filePath = path.join(dir, `${workflow.name}.json`);
  fs.writeFileSync(filePath, JSON.stringify(workflow, null, 2), "utf-8");
  return filePath;
}

/** Load a workflow from a JSON file. */
export function loadWorkflow(filePath: string): Workflow {
  const raw = fs.readFileSync(filePath, "utf-8");
  return JSON.parse(raw) as Workflow;
}

/** List all workflows in the directory. */
export function listWorkflows(dir = WORKFLOWS_DIR): Workflow[] {
  ensureDir(dir);
  const files = fs.readdirSync(dir).filter((f) => f.endsWith(".json"));
  return files.map((f) => loadWorkflow(path.join(dir, f)));
}

/** Delete a workflow by name. */
export function deleteWorkflow(name: string, dir = WORKFLOWS_DIR): boolean {
  const filePath = path.join(dir, `${name}.json`);
  if (fs.existsSync(filePath)) {
    fs.unlinkSync(filePath);
    return true;
  }
  return false;
}

/**
 * Export a workflow to a portable .dilipod file.
 * A .dilipod file is just a JSON file with metadata.
 */
export function exportWorkflow(workflow: Workflow, outputPath: string): void {
  const exportData = {
    format: "dilipod-workflow",
    version: "1.0",
    exported_at: new Date().toISOString(),
    workflow,
  };
  fs.writeFileSync(outputPath, JSON.stringify(exportData, null, 2), "utf-8");
}

/**
 * Import a workflow from a .dilipod file.
 */
export function importWorkflow(filePath: string): Workflow {
  const raw = fs.readFileSync(filePath, "utf-8");
  const data = JSON.parse(raw);
  if (data.format !== "dilipod-workflow") {
    throw new Error(`Invalid .dilipod file: expected format 'dilipod-workflow', got '${data.format}'`);
  }
  return data.workflow as Workflow;
}
