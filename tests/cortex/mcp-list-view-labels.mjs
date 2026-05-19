#!/usr/bin/env node
/**
 * MCP cel_see context test: verify the AX list-view row label cascade.
 *
 * Before this fix, `cel_see context` on a Finder window returned 100+
 * `table_row` and `table_cell` elements with `label: null`. The default
 * `AXTitle ?? AXDescription` cascade returns nothing for Finder rows
 * because the filename lives in `AXValue`. List-view workflows were
 * impossible without falling back to per-row `cel_see focused` calls.
 *
 * The fix adds a row-friendly cascade
 * (`AXLabel` → `AXValue` → `AXDescription` → `AXTitle` → `AXFilename`)
 * for `AXRow`, `AXCell`, `AXOutlineRow`, and `AXListRow` in
 * `cel-accessibility/src/macos.rs::build_element`.
 *
 * This test:
 * 1. Opens the cellar repo root in Finder so there are known files to label.
 * 2. Spawns the MCP server.
 * 3. Switches Finder to list view (Cmd-2) so AXRow elements are emitted.
 * 4. Calls `cel_see context` filtered to row-like element types.
 * 5. Asserts that at least one returned row has a non-null label that
 *    matches a known sentinel file (ACQUISITION_MEMO.md or CLAUDE.md).
 *
 * PREREQUISITES:
 *   1. macOS accessibility permissions granted to the host process
 *   2. MCP server built: cd mcp-server && pnpm build
 *   3. cel-napi rebuilt: make build-napi
 *
 * Run: node tests/cortex/mcp-list-view-labels.mjs
 */

import { spawn, execFileSync } from "node:child_process";
import { createInterface } from "node:readline";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

let nextId = 1;

function startMcpServer() {
  const projectRoot = resolve(__dirname, "../..");
  const serverPath = resolve(projectRoot, "mcp-server/dist/index.js");
  const proc = spawn(process.execPath, [serverPath], {
    stdio: ["pipe", "pipe", "pipe"],
    cwd: projectRoot,
  });

  const rl = createInterface({ input: proc.stdout });
  const pending = new Map();

  rl.on("line", (line) => {
    try {
      const msg = JSON.parse(line);
      if (msg.id !== undefined && pending.has(msg.id)) {
        const { resolve } = pending.get(msg.id);
        pending.delete(msg.id);
        resolve(msg);
      }
    } catch {
      // ignore non-JSON
    }
  });

  proc.stderr.on("data", (data) => {
    process.stderr.write(`[mcp] ${data}`);
  });

  async function send(method, params = {}) {
    const id = nextId++;
    const request = { jsonrpc: "2.0", id, method, params };
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject });
      proc.stdin.write(JSON.stringify(request) + "\n");
      setTimeout(() => {
        if (pending.has(id)) {
          pending.delete(id);
          reject(new Error(`Timeout waiting for response to ${method}`));
        }
      }, 20000);
    });
  }

  async function callTool(toolName, args) {
    return send("tools/call", { name: toolName, arguments: args });
  }

  function kill() {
    proc.kill("SIGTERM");
  }

  return { send, callTool, kill, proc };
}

function activate(app) {
  execFileSync("osascript", ["-e", `tell application "${app}" to activate`]);
}

function parseText(resp) {
  const text = resp.result?.content?.[0]?.text;
  if (!text) return null;
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}

/**
 * Walk the ScreenContext element tree and collect every row-like element
 * with a non-null label. The MCP context filter restricts the returned set
 * to row-like types but elements may still be nested under windows; this
 * makes the test robust to either flat or nested response shapes.
 */
function collectRowLabels(elements) {
  const labels = [];
  function visit(el) {
    if (!el) return;
    const t = el.element_type || el.role || "";
    if (
      (t === "table_row" || t === "table_cell" || t === "outline_row" || t === "list_row") &&
      typeof el.label === "string" &&
      el.label.length > 0
    ) {
      labels.push(el.label);
    }
    if (Array.isArray(el.children)) {
      for (const c of el.children) visit(c);
    }
  }
  if (Array.isArray(elements)) {
    for (const e of elements) visit(e);
  }
  return labels;
}

async function main() {
  console.log("\n=== MCP cel_see context List-View Row Label Test ===\n");

  // Setup: open the cellar repo in Finder, switch to list view
  const projectRoot = resolve(__dirname, "../..");
  execFileSync("open", [projectRoot]);
  await sleep(600);
  activate("Finder");
  await sleep(400);
  // Cmd-2 = "as List" view
  execFileSync("osascript", [
    "-e",
    'tell application "System Events" to keystroke "2" using command down',
  ]);
  await sleep(600);

  const server = startMcpServer();
  await sleep(2500);

  let passed = 0;
  let failed = 0;
  function assert(condition, message) {
    if (condition) {
      console.log(`  ✓ ${message}`);
      passed++;
    } else {
      console.error(`  ✗ ${message}`);
      failed++;
    }
  }

  try {
    console.log("1. Initialize MCP");
    await server.send("initialize", {
      protocolVersion: "2024-11-05",
      capabilities: {},
      clientInfo: { name: "list-view-labels-test", version: "1.0" },
    });

    console.log("\n2. cel_see context filtered to row-like elements");
    const ctxResp = await server.callTool("cel_see", {
      mode: "context",
      filter: { element_types: ["table_row", "table_cell"], detail: "compact" },
    });
    const ctx = parseText(ctxResp);
    assert(
      ctx && (Array.isArray(ctx.elements) || Array.isArray(ctx)),
      `cel_see context returned a context payload: ${JSON.stringify(ctx)?.slice(0, 200)}`,
    );

    const rowLabels = collectRowLabels(ctx?.elements ?? ctx);
    assert(
      rowLabels.length > 0,
      `Found at least one row with a non-null label (got ${rowLabels.length}). ` +
        `Sample: ${rowLabels.slice(0, 5).join(", ")}`,
    );

    // Sentinel files that exist at the cellar repo root. If Finder is
    // showing this folder in list view, at least one row should label
    // back to one of these names. Both are sufficiently unique that
    // they're unlikely to collide with the cortex's other element labels.
    const sentinels = ["ACQUISITION_MEMO.md", "CLAUDE.md", "AGENTS.md", "Cargo.toml"];
    const matched = sentinels.filter((s) =>
      rowLabels.some((l) => l.includes(s)),
    );
    assert(
      matched.length > 0,
      `At least one sentinel filename appears in the row labels (matched: ${matched.join(", ") || "none"}). ` +
        `First 10 labels: ${rowLabels.slice(0, 10).join(" | ")}`,
    );
  } catch (e) {
    console.error("\nTest error:", e.message);
    failed++;
  } finally {
    server.kill();
  }

  console.log(`\n=== Results: ${passed} passed, ${failed} failed ===\n`);
  process.exit(failed > 0 ? 1 : 0);
}

main();
