#!/usr/bin/env node
/**
 * Smoke test for the Reminders adapter end-to-end through MCP.
 *
 * Lifecycle:
 *  - reminders.add creates a reminder with a marker title
 *  - reminders.list surfaces it
 *  - reminders.update renames it
 *  - reminders.complete marks it done
 *  - reminders.delete removes it
 *
 * Self-cleaning: the test reminder is deleted at the end. Stray reminders
 * from crashed runs have the "[CEL-TEST]" prefix.
 *
 * List name:
 *  - Reads from CEL_TEST_REMINDERS_LIST env var, default "Reminders".
 *  - The named list must exist (case-sensitive).
 *
 * PREREQUISITES:
 *   - macOS with Reminders.app, the named list exists
 *   - Automation permission for Reminders granted to the host process
 *   - MCP server built: cd mcp-server && pnpm build
 *   - Native module built: make build-napi
 *   - Adapter binary built (ProcessDriver): make build-adapters
 *     (cortex spawns target/release/adapter-reminders via adapter.json)
 *
 * Run: node tests/cortex/mcp-adapter-reminders.mjs
 *      CEL_TEST_REMINDERS_LIST=Tasks node tests/cortex/mcp-adapter-reminders.mjs
 */

import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const TEST_LIST = process.env.CEL_TEST_REMINDERS_LIST || "Reminders";
const TEST_TITLE = "[CEL-TEST] reminders adapter smoke";
const UPDATED_TITLE = "[CEL-TEST] reminders adapter renamed";

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
      }, 60000);
      // Reminders.list iterates AppleScript records — even with bulk
      // `properties of every reminder`, this can take 5–10s for lists with
      // ~20 items. Bump to 60s so tests pass on machines with longer lists.
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
  console.log(`\n=== MCP Reminders Adapter Smoke Test (list="${TEST_LIST}") ===\n`);

  const server = startMcpServer();
  // Wait for cortex boot + at least one tick (200ms) so the reminders
  // adapter is activated by the time we send the first cel_act call. Cortex
  // boot is sync but eager-deferred via setImmediate; 4s is comfortably past
  // the ~3s cold boot on the slowest test rig.
  await sleep(4000);

  let passed = 0;
  let failed = 0;
  let createdReminderId = null;

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
      clientInfo: { name: "reminders-adapter-test", version: "1.0" },
    });

    // 2. add
    console.log("\n2. reminders.add");
    const addResp = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "reminders",
      adapter_op: "add",
      params: {
        list: TEST_LIST,
        title: TEST_TITLE,
        notes: "Created by mcp-adapter-reminders smoke test.",
      },
    });
    const addData = parseAdapterData(addResp);
    assert(
      typeof addData?.reminder_id === "string" && addData.reminder_id.length > 0,
      `add returned reminder_id: ${(addData?.reminder_id ?? "").slice(0, 60)}`,
    );
    createdReminderId = addData?.reminder_id;
    if (!createdReminderId) {
      throw new Error("add failed — aborting remaining steps");
    }

    // 3. list — find the new reminder
    console.log("\n3. reminders.list (completed=false)");
    const listResp = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "reminders",
      adapter_op: "list",
      params: { list: TEST_LIST, completed: false, limit: 100 },
    });
    const listData = parseAdapterData(listResp);
    const found = (listData?.reminders ?? []).find(
      (r) => r.reminder_id === createdReminderId,
    );
    assert(
      !!found && found.title === TEST_TITLE && found.completed === false,
      `list surfaced the new reminder with original title (completed=false)`,
    );

    // 4. update — rename
    console.log("\n4. reminders.update (rename)");
    await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "reminders",
      adapter_op: "update",
      params: { reminder_id: createdReminderId, title: UPDATED_TITLE },
    });
    const listResp2 = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "reminders",
      adapter_op: "list",
      params: { list: TEST_LIST, completed: false, limit: 100 },
    });
    const found2 = (parseAdapterData(listResp2)?.reminders ?? []).find(
      (r) => r.reminder_id === createdReminderId,
    );
    assert(
      !!found2 && found2.title === UPDATED_TITLE,
      `update renamed the reminder (now "${found2?.title}")`,
    );

    // 5. complete
    console.log("\n5. reminders.complete");
    await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "reminders",
      adapter_op: "complete",
      params: { reminder_id: createdReminderId },
    });
    const listResp3 = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "reminders",
      adapter_op: "list",
      params: { list: TEST_LIST, completed: true, limit: 100 },
    });
    const found3 = (parseAdapterData(listResp3)?.reminders ?? []).find(
      (r) => r.reminder_id === createdReminderId,
    );
    assert(
      !!found3 && found3.completed === true,
      `complete flipped the reminder to completed=true`,
    );

    // 6. delete
    console.log("\n6. reminders.delete");
    await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "reminders",
      adapter_op: "delete",
      params: { reminder_id: createdReminderId },
    });
    // List both completed and not — the deleted reminder must be in neither.
    const finalListResp = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "reminders",
      adapter_op: "list",
      params: { list: TEST_LIST, completed: true, limit: 100 },
    });
    const finalListResp2 = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "reminders",
      adapter_op: "list",
      params: { list: TEST_LIST, completed: false, limit: 100 },
    });
    const stillThere =
      (parseAdapterData(finalListResp)?.reminders ?? []).some(
        (r) => r.reminder_id === createdReminderId,
      ) ||
      (parseAdapterData(finalListResp2)?.reminders ?? []).some(
        (r) => r.reminder_id === createdReminderId,
      );
    assert(!stillThere, `delete removed the reminder entirely`);
    if (!stillThere) {
      createdReminderId = null;
    }

    // 7. Unknown op — clean error
    console.log("\n7. unknown adapter op — clean error");
    const badResp = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "reminders",
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
    if (createdReminderId) {
      console.log(`\nCleanup: deleting test reminder ${createdReminderId.slice(0, 60)}...`);
      try {
        await server.callTool("cel_act", {
          action: "adapter_action",
          adapter: "reminders",
          adapter_op: "delete",
          params: { reminder_id: createdReminderId },
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
