#!/usr/bin/env node
/**
 * check-agent-boundary.mjs
 *
 * Enforces the agent-backend boundary: packages that are themselves agent
 * backends (MCP server, future LangGraph package, future Mastra package, future
 * in-house planner) may ONLY import from `@cellar/agent/<subpath>` — never from
 * the bare `@cellar/agent` root.
 *
 * Why: the bare root re-exports the built-in TypeScript agent backend
 * (`WorkflowEngine`, `runGoal`, `orchestrate`, LangGraph driver, strategy
 * router, self-healer, etc.). Those are one *specific* backend among many;
 * importing them from a different backend crosses the boundary and turns the
 * platform into a tangle.
 *
 * The runtime primitive surface every backend can consume is exported from
 * `@cellar/agent/runtime`. If a backend needs something that isn't there, the
 * fix is to promote it into the runtime surface, not to reach across.
 *
 * See `docs/adapters-cel-agents.md` § "Layer 3: Agents".
 */

import { readdir, readFile } from "node:fs/promises";
import { join, relative, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const REPO_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

// Packages whose `src/` must stay on the agent-backend side of the boundary.
// Add a new entry here when introducing a new agent backend.
const AGENT_BACKEND_PACKAGES = [
  "mcp-server",
  "examples/agent-skeleton",
  // "adapters/agent-mastra",     // planned
  // "adapters/agent-langgraph",  // planned (post Stage 3 split)
];

// Packages that may contain a few legacy/built-in-agent commands, but should
// otherwise use `@cellar/agent/runtime` for CEL primitives.
const RUNTIME_CONSUMER_PACKAGES = [
  "cli",
  "adapters/browser",
];

// File-level exceptions where the bare root is intentional because the file is
// opting into built-in agent features (WorkflowEngine, runGoal, LangGraph, or
// workflow IO), not just the CEL runtime primitive surface.
const ROOT_IMPORT_ALLOWLIST = new Map([
  ["cli/src/commands/run.ts", "uses WorkflowEngine / workflow IO"],
  ["cli/src/commands/run-goal-langgraph.ts", "uses LangGraph agent backend"],
  ["cli/src/commands/train.ts", "uses workflow IO saveWorkflow"],
  ["cli/src/commands/workflow.ts", "uses workflow IO"],
  ["adapters/browser/src/cel-run.ts", "uses TS runGoal fallback and caches"],
  ["adapters/browser/src/callback-builder.ts", "uses GoalRunnerCallbacks type"],
]);

// Matches imports of the bare root, in any of:
//   import ... from "@cellar/agent"
//   import "@cellar/agent"
//   import("@cellar/agent")
// Does NOT match subpath imports like "@cellar/agent/runtime" because the
// closing quote sits immediately after `agent` in the forbidden form.
const FORBIDDEN_IMPORT = /(?:from\s+|import\s*\(?\s*)["']@cellar\/agent["']/;

/**
 * Single-line comment detector. We skip lines whose trimmed start is `//`,
 * `*` (jsdoc continuation), or `/*` (one-line block comment) so the script
 * doesn't false-positive on docstrings that *describe* the forbidden import.
 *
 * This is a heuristic, not a full parser: it misses the body of multi-line
 * `/* ... *​/` blocks. In practice that body is also prefixed with ` * `, so
 * it's still caught.
 */
function isCommentLine(line) {
  const trimmed = line.trimStart();
  return (
    trimmed.startsWith("//") ||
    trimmed.startsWith("*") ||
    trimmed.startsWith("/*")
  );
}

/** Recursively list .ts / .tsx files under `dir`, skipping node_modules / dist. */
async function listTsFiles(dir) {
  const out = [];
  let entries;
  try {
    entries = await readdir(dir, { withFileTypes: true });
  } catch (err) {
    if (err.code === "ENOENT") return out;
    throw err;
  }
  for (const entry of entries) {
    if (entry.name === "node_modules" || entry.name === "dist") continue;
    const full = join(dir, entry.name);
    if (entry.isDirectory()) {
      out.push(...(await listTsFiles(full)));
    } else if (entry.name.endsWith(".ts") || entry.name.endsWith(".tsx")) {
      out.push(full);
    }
  }
  return out;
}

function checkFile(file, content, mode) {
  let violations = 0;
  const rel = relative(REPO_ROOT, file);
  const lines = content.split("\n");
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    if (isCommentLine(line)) continue;
    if (!FORBIDDEN_IMPORT.test(line)) continue;

    if (mode === "allowlisted-runtime-consumer" && ROOT_IMPORT_ALLOWLIST.has(rel)) {
      continue;
    }

    console.error(
      `${rel}:${i + 1}: imports from bare "@cellar/agent" — use "@cellar/agent/runtime" instead`,
    );
    console.error(`    ${line.trim()}`);
    if (mode === "allowlisted-runtime-consumer") {
      console.error(
        `    If this file intentionally uses built-in agent APIs, add it to ROOT_IMPORT_ALLOWLIST with a reason.`,
      );
    }
    violations += 1;
  }
  return violations;
}

let violations = 0;
let filesScanned = 0;

for (const pkg of AGENT_BACKEND_PACKAGES) {
  const srcDir = join(REPO_ROOT, pkg, "src");
  const files = await listTsFiles(srcDir);
  for (const file of files) {
    filesScanned += 1;
    const content = await readFile(file, "utf-8");
    violations += checkFile(file, content, "strict-agent-backend");
  }
}

for (const pkg of RUNTIME_CONSUMER_PACKAGES) {
  const srcDir = join(REPO_ROOT, pkg, "src");
  const files = await listTsFiles(srcDir);
  for (const file of files) {
    filesScanned += 1;
    const content = await readFile(file, "utf-8");
    violations += checkFile(file, content, "allowlisted-runtime-consumer");
  }
}

if (violations > 0) {
  console.error("");
  console.error(`${violations} agent-backend boundary violation(s) across ${filesScanned} file(s).`);
  console.error("");
  console.error(`Rule: agent-backend packages (${AGENT_BACKEND_PACKAGES.join(", ")}) may only`);
  console.error(`import from @cellar/agent/<subpath> (e.g. @cellar/agent/runtime).`);
  console.error("");
  console.error("Why: the bare @cellar/agent root re-exports the built-in TypeScript agent backend");
  console.error("(WorkflowEngine, runGoal, LangGraph driver, ...). Importing those from a different");
  console.error("backend crosses the boundary. If you need a symbol that isn't yet in the runtime");
  console.error("surface, promote it into agent/src/runtime-surface.ts.");
  console.error("");
  console.error("See docs/adapters-cel-agents.md § \"Layer 3: Agents\".");
  process.exit(1);
}

console.log(
  `OK: ${AGENT_BACKEND_PACKAGES.length} agent-backend package(s) and ${RUNTIME_CONSUMER_PACKAGES.length} runtime-consumer package(s), ${filesScanned} file(s) respect the runtime boundary.`,
);
