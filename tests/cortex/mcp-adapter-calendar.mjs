#!/usr/bin/env node
/**
 * Smoke test for the Calendar adapter end-to-end through MCP.
 *
 * Lifecycle:
 *  - calendar.create_event creates a test event with a marker title
 *  - calendar.list_events surfaces the event
 *  - calendar.update_event renames it; list confirms the rename
 *  - calendar.delete_event removes it; list confirms it's gone
 *
 * Self-cleaning: the test event is deleted at the end. If the test crashes
 * mid-run, the stray event has the prefix "[CEL-TEST]" and a far-future
 * date (default 2026-12-25), making it easy to find and delete by hand.
 *
 * Calendar name:
 *  - Reads from CEL_TEST_CALENDAR env var, default "Home" (typical iCloud).
 *  - The named calendar must exist and be writable.
 *
 * PREREQUISITES:
 *   - macOS with Calendar.app, at least one writable calendar
 *   - Automation permission for Calendar granted to the host process
 *   - MCP server built: cd mcp-server && pnpm build
 *   - Native module built: make build-napi
 *   - Adapter binary built (ProcessDriver): make build-adapters
 *     (cortex spawns target/release/adapter-calendar via adapter.json)
 *
 * Run: node tests/cortex/mcp-adapter-calendar.mjs
 *      CEL_TEST_CALENDAR=Work node tests/cortex/mcp-adapter-calendar.mjs
 */

import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const TEST_CALENDAR = process.env.CEL_TEST_CALENDAR || "Home";
const TEST_DATE_START = "2026-12-25T10:00:00";
const TEST_DATE_END = "2026-12-25T11:00:00";
const RANGE_START = "2026-12-25T00:00:00";
const RANGE_END = "2026-12-26T00:00:00";
const TEST_TITLE = "[CEL-TEST] calendar adapter smoke";
const UPDATED_TITLE = "[CEL-TEST] calendar adapter renamed";

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

  return { send, callTool, kill: () => proc.kill("SIGTERM"), proc };
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

function parseAdapterData(resp) {
  const outer = parseText(resp);
  if (!outer?.success) return null;
  try {
    return typeof outer.result === "string" ? JSON.parse(outer.result) : outer.result;
  } catch {
    return outer.result;
  }
}

async function main() {
  console.log(`\n=== MCP Calendar Adapter Smoke Test (calendar="${TEST_CALENDAR}") ===\n`);

  const server = startMcpServer();
  // Wait for cortex boot + at least one tick (200ms) so the calendar adapter
  // is activated by the time we send the first cel_act call. Cortex boot is
  // sync but eager-deferred via setImmediate; 4s is comfortably past the
  // ~3s cold boot on the slowest test rig.
  await sleep(4000);

  let passed = 0;
  let failed = 0;
  let createdEventId = null;

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
      clientInfo: { name: "calendar-adapter-test", version: "1.0" },
    });

    // 2. create_event
    console.log("\n2. calendar.create_event");
    const createResp = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "calendar",
      adapter_op: "create_event",
      params: {
        calendar: TEST_CALENDAR,
        title: TEST_TITLE,
        start: TEST_DATE_START,
        end: TEST_DATE_END,
        notes: "Created by mcp-adapter-calendar smoke test.",
        location: "CEL Test Lab",
      },
    });
    const createData = parseAdapterData(createResp);
    assert(
      typeof createData?.event_id === "string" && createData.event_id.length > 0,
      `create_event returned event_id: ${(createData?.event_id ?? "").slice(0, 50)}`,
    );
    createdEventId = createData?.event_id;
    if (!createdEventId) {
      throw new Error("create_event failed — aborting remaining steps");
    }

    // 3. list_events — verify our event shows up
    console.log("\n3. calendar.list_events (covering test range)");
    const listResp = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "calendar",
      adapter_op: "list_events",
      params: {
        calendar: TEST_CALENDAR,
        start: RANGE_START,
        end: RANGE_END,
      },
    });
    const listData = parseAdapterData(listResp);
    const found = (listData?.events ?? []).find((e) => e.event_id === createdEventId);
    assert(
      !!found && found.title === TEST_TITLE,
      `list_events surfaced the test event with original title`,
    );

    // 4. update_event — change title, verify
    console.log("\n4. calendar.update_event (rename)");
    await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "calendar",
      adapter_op: "update_event",
      params: { event_id: createdEventId, title: UPDATED_TITLE },
    });
    const listResp2 = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "calendar",
      adapter_op: "list_events",
      params: {
        calendar: TEST_CALENDAR,
        start: RANGE_START,
        end: RANGE_END,
      },
    });
    const listData2 = parseAdapterData(listResp2);
    const found2 = (listData2?.events ?? []).find((e) => e.event_id === createdEventId);
    assert(
      !!found2 && found2.title === UPDATED_TITLE,
      `update_event renamed the event (now "${found2?.title}")`,
    );

    // 5. delete_event
    console.log("\n5. calendar.delete_event");
    await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "calendar",
      adapter_op: "delete_event",
      params: { event_id: createdEventId },
    });
    const listResp3 = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "calendar",
      adapter_op: "list_events",
      params: {
        calendar: TEST_CALENDAR,
        start: RANGE_START,
        end: RANGE_END,
      },
    });
    const listData3 = parseAdapterData(listResp3);
    const stillThere = (listData3?.events ?? []).some(
      (e) => e.event_id === createdEventId,
    );
    assert(!stillThere, `delete_event removed the event from list`);
    if (!stillThere) {
      // Clear the cleanup variable since we already removed it.
      createdEventId = null;
    }

    // 6. Unknown op — clean error
    console.log("\n6. unknown adapter op — clean error");
    const badResp = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "calendar",
      adapter_op: "nonsense_op",
      params: {},
    });
    const badText = badResp.result?.content?.[0]?.text ?? "";
    assert(
      badResp.result?.isError === true || badText.includes("does not expose"),
      `unknown op errored cleanly: ${badText.slice(0, 200)}`,
    );

    console.log(`\n=== Summary: ${passed} passed, ${failed} failed ===\n`);
  } finally {
    if (createdEventId) {
      console.log(`\nCleanup: deleting test event ${createdEventId.slice(0, 50)}...`);
      try {
        await server.callTool("cel_act", {
          action: "adapter_action",
          adapter: "calendar",
          adapter_op: "delete_event",
          params: { event_id: createdEventId },
        });
        console.log("Cleanup done.");
      } catch (e) {
        console.error("Cleanup failed:", e.message);
      }
    }
    server.kill();
  }

  process.exit(failed > 0 ? 1 : 0);
}

main().catch((err) => {
  console.error("\nTest crashed:", err);
  process.exit(2);
});
