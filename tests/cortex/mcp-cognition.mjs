#!/usr/bin/env node
/**
 * MCP cognition test: Verify cel_think modes work end-to-end via JSON-RPC.
 *
 * Spawns the MCP server as a subprocess and exercises the LLM-free cel_think
 * modes: memory_get/set, store_knowledge/search_knowledge, observe/
 * get_observations, and the run lifecycle (start/log_step/finish/history/steps).
 *
 * LLM-requiring modes (plan, plan_with_vision, run_goal, llm_complete) are not
 * covered here — they need CEL_LLM_API_KEY configured.
 *
 * PREREQUISITES:
 * 1. MCP server built: cd mcp-server && pnpm build
 * 2. Native module rebuilt
 *
 * Run: node tests/cortex/mcp-cognition.mjs
 */

import { spawn } from "node:child_process";
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
      // Ignore non-JSON lines
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
      }, 15000);
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

function parseResult(resp) {
  const text = resp.result?.content?.[0]?.text;
  if (!text) return null;
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}

async function main() {
  console.log("\n=== MCP cel_think Cognition Protocol Test ===\n");

  const server = startMcpServer();
  await sleep(3000);

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

  const stamp = Date.now();
  const workflowName = `cognition-test-${stamp}`;
  // search_knowledge wraps user queries in an FTS5 phrase, so hyphens (and
  // other FTS5-special characters) are now safe — see
  // `sanitize_fts5_query` in cel/cel-store/src/memory.rs.
  const sentinel = `sentinel${stamp}`;
  const knowledgeContent = `${sentinel}: cellar MCP cognition test sentinel`;

  try {
    // 1. Initialize
    console.log("1. Initialize MCP connection");
    const initResp = await server.send("initialize", {
      protocolVersion: "2024-11-05",
      capabilities: {},
      clientInfo: { name: "cognition-test", version: "1.0" },
    });
    assert(initResp.result?.serverInfo?.name === "cel", `Server name: ${initResp.result?.serverInfo?.name}`);

    // 2. Confirm cel_think is exposed
    console.log("\n2. cel_think tool registered");
    const toolsResp = await server.send("tools/list", {});
    const toolNames = (toolsResp.result?.tools ?? []).map((t) => t.name);
    assert(toolNames.includes("cel_think"), "cel_think tool available");

    // 3. memory_set / memory_get
    console.log("\n3. memory_set + memory_get round-trip");
    const setResp = await server.callTool("cel_think", {
      mode: "memory_set",
      workflow_name: workflowName,
      content: "step 1 complete; next: open Numbers",
    });
    const setData = parseResult(setResp);
    assert(setData?.success === true, `memory_set returned success: ${JSON.stringify(setData)}`);

    const getResp = await server.callTool("cel_think", {
      mode: "memory_get",
      workflow_name: workflowName,
    });
    const getData = parseResult(getResp);
    assert(
      getData?.content?.includes("open Numbers"),
      `memory_get returned the stored content: ${JSON.stringify(getData)}`,
    );

    // 4. store_knowledge / search_knowledge
    console.log("\n4. store_knowledge + search_knowledge round-trip");
    const storeResp = await server.callTool("cel_think", {
      mode: "store_knowledge",
      content: knowledgeContent,
      source: "cognition-test",
      tags: "cognition,sentinel",
    });
    const storeData = parseResult(storeResp);
    assert(
      storeData?.success === true && typeof storeData?.knowledge_id !== "undefined",
      `store_knowledge returned id: ${JSON.stringify(storeData)}`,
    );

    const searchResp = await server.callTool("cel_think", {
      mode: "search_knowledge",
      query: sentinel,
      limit: 5,
    });
    const searchData = parseResult(searchResp);
    const foundSentinel =
      Array.isArray(searchData) &&
      searchData.some((row) =>
        typeof row === "string"
          ? row.includes(sentinel)
          : JSON.stringify(row).includes(sentinel),
      );
    assert(foundSentinel, `search_knowledge found sentinel: ${JSON.stringify(searchData)?.slice(0, 200)}`);

    // Regression: hyphens, colons, parens, etc. in user queries used to be
    // forwarded raw to FTS5 MATCH and crash with "Database error: no such
    // column: …". After sanitization the query is wrapped as a phrase and
    // either matches or returns []. We check both shapes here.

    // (a) Hyphenated query that does NOT match anything: must succeed and
    //     return an empty result, not error.
    const hyphenMissResp = await server.callTool("cel_think", {
      mode: "search_knowledge",
      query: `cognition-test-fact-${stamp}`,
      limit: 5,
    });
    const hyphenMissText = hyphenMissResp.result?.content?.[0]?.text ?? "";
    const hyphenMissCrashes =
      hyphenMissResp.result?.isError === true ||
      hyphenMissText.includes("Database error") ||
      hyphenMissText.includes("no such column");
    assert(
      !hyphenMissCrashes,
      `search_knowledge now safely handles hyphenated queries (no FTS5 crash): ${hyphenMissText.slice(0, 150)}`,
    );
    const hyphenMissData = parseResult(hyphenMissResp);
    assert(
      Array.isArray(hyphenMissData) && hyphenMissData.length === 0,
      `unmatched hyphenated query returns empty array: ${JSON.stringify(hyphenMissData)?.slice(0, 200)}`,
    );

    // (b) Hyphenated query whose tokens ARE adjacent in stored content
    //     should still find the row — the stored content has
    //     "...cognition test sentinel", so the phrase "cognition-test"
    //     (FTS5-tokenized as ["cognition","test"]) matches.
    const hyphenHitResp = await server.callTool("cel_think", {
      mode: "search_knowledge",
      query: "cognition-test",
      limit: 5,
    });
    const hyphenHitData = parseResult(hyphenHitResp);
    const foundHyphenHit =
      Array.isArray(hyphenHitData) &&
      hyphenHitData.some((row) => JSON.stringify(row).includes(sentinel));
    assert(
      foundHyphenHit,
      `hyphenated query matches adjacent tokens in stored content: ${JSON.stringify(hyphenHitData)?.slice(0, 200)}`,
    );

    // 5. observe / get_observations
    console.log("\n5. observe + get_observations round-trip");
    const obsResp = await server.callTool("cel_think", {
      mode: "observe",
      workflow_name: workflowName,
      content: "Numbers app loses focus when Activity Monitor opens",
      priority: "high",
    });
    const obsData = parseResult(obsResp);
    assert(
      obsData?.success === true && typeof obsData?.observation_id !== "undefined",
      `observe returned id: ${JSON.stringify(obsData)}`,
    );

    const obsListResp = await server.callTool("cel_think", {
      mode: "get_observations",
      workflow_name: workflowName,
      limit: 10,
    });
    const obsList = parseResult(obsListResp);
    const foundObs =
      Array.isArray(obsList) &&
      obsList.some((row) => JSON.stringify(row).includes("Activity Monitor"));
    assert(foundObs, `get_observations returned the recorded entry: ${JSON.stringify(obsList)?.slice(0, 200)}`);

    // Regression: SQL-NULL columns must serialize as JSON `null`, not as
    // empty strings. The previous serializer collapsed Option<String>::None
    // to "" via unwrap_or_default(), which forced JS callers to special-case
    // both "" and null when checking for "no value". After the fix every
    // nullable column reports `null` consistently.
    const newRow = Array.isArray(obsList)
      ? obsList.find((r) => JSON.stringify(r).includes("Activity Monitor"))
      : null;
    assert(
      newRow && newRow.observed_at === null,
      `observed_at is JSON null, not "": ${JSON.stringify(newRow)?.slice(0, 200)}`,
    );
    assert(
      newRow && newRow.referenced_at === null,
      `referenced_at is JSON null, not "": ${JSON.stringify(newRow)?.slice(0, 200)}`,
    );

    // 6. Run lifecycle: start → log_step → finish → history → steps
    console.log("\n6. Run lifecycle (start → log_step → finish → history → steps)");
    const startRunResp = await server.callTool("cel_think", {
      mode: "run_start",
      workflow_name: workflowName,
      steps_total: 2,
    });
    const startRun = parseResult(startRunResp);
    assert(typeof startRun?.run_id !== "undefined", `run_start returned run_id: ${JSON.stringify(startRun)}`);
    const runId = startRun.run_id;

    const logResp = await server.callTool("cel_think", {
      mode: "run_log_step",
      run_id: runId,
      step_index: 0,
      step_id: "open-app",
      action: JSON.stringify({ kind: "ax_action", element: "DockTile:Numbers" }),
      success: true,
      confidence: 0.92,
    });
    const logData = parseResult(logResp);
    assert(
      typeof logData?.step_row_id !== "undefined",
      `run_log_step returned step_row_id: ${JSON.stringify(logData)}`,
    );

    // Regression: hosts whose MCP wire layer auto-encodes object literals
    // deliver `action` as a JSON object even when callers pass a string.
    // The schema used to reject these with a zod parse error. After the
    // fix, the schema accepts either form and coerces objects to JSON
    // strings before storage. Both forms must succeed and the persisted
    // row must carry a string `action`.
    const objActionResp = await server.callTool("cel_think", {
      mode: "run_log_step",
      run_id: runId,
      step_index: 1,
      step_id: "click-target",
      action: { kind: "ax_action", element: "Button:Submit" },
      success: true,
      confidence: 0.88,
    });
    const objActionData = parseResult(objActionResp);
    assert(
      typeof objActionData?.step_row_id !== "undefined",
      `run_log_step accepts object-form action: ${JSON.stringify(objActionData)?.slice(0, 200)}`,
    );

    const finishResp = await server.callTool("cel_think", {
      mode: "run_finish",
      run_id: runId,
      status: "completed",
    });
    const finishData = parseResult(finishResp);
    assert(finishData?.success === true, `run_finish succeeded: ${JSON.stringify(finishData)}`);

    const histResp = await server.callTool("cel_think", { mode: "run_history", limit: 5 });
    const hist = parseResult(histResp);
    const foundRun =
      Array.isArray(hist) &&
      hist.some((row) => row?.id === runId || row?.run_id === runId);
    assert(foundRun, `run_history contains the just-finished run: ${JSON.stringify(hist)?.slice(0, 200)}`);

    const stepsResp = await server.callTool("cel_think", { mode: "run_steps", run_id: runId });
    const steps = parseResult(stepsResp);
    assert(
      Array.isArray(steps) && steps.length >= 2,
      `run_steps returned both logged steps: ${JSON.stringify(steps)?.slice(0, 200)}`,
    );
    const objStep = Array.isArray(steps)
      ? steps.find((s) => s?.step_id === "click-target")
      : null;
    assert(
      objStep && typeof objStep.action === "string" && objStep.action.includes("Button:Submit"),
      `object-form action was JSON-stringified before storage: ${JSON.stringify(objStep)?.slice(0, 200)}`
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
