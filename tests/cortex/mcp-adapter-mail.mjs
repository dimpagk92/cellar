#!/usr/bin/env node
/**
 * Smoke test for the Mail adapter end-to-end through MCP.
 *
 * Verifies via `cel_act adapter_action`:
 *  - mail.compose creates an in-memory outgoing message and returns draft_id
 *  - mail.search runs (with a marker guaranteed not to match)
 *  - unknown op surfaces a clean error
 *
 * Deliberately does NOT call mail.send_draft — that would actually send mail.
 * The compose op creates an outgoing message in Mail.app's memory but does
 * not save to Drafts mailbox (no `save` call); the orphaned outgoing
 * message is reclaimed by Mail on next launch or after the compose window
 * is closed.
 *
 * PREREQUISITES:
 *   - macOS with Mail.app
 *   - Automation permission for Mail granted to the host process
 *   - MCP server built: cd mcp-server && pnpm build
 *   - Native module built: make build-napi
 *   - Adapter binary built (ProcessDriver): make build-adapters
 *     (cortex spawns target/release/adapter-mail via adapter.json)
 *
 * Run: node tests/cortex/mcp-adapter-mail.mjs
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
  console.log("\n=== MCP Mail Adapter Smoke Test ===\n");

  const server = startMcpServer();
  // Wait for cortex boot + at least one tick (200ms) so the mail adapter is
  // activated by the time we send the first cel_act call. Cortex boot is
  // sync but eager-deferred via setImmediate; 4s is comfortably past the
  // ~3s cold boot on the slowest test rig.
  await sleep(4000);

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
      clientInfo: { name: "mail-adapter-test", version: "1.0" },
    });

    // 2. compose — creates an in-memory outgoing message. We never send.
    console.log("\n2. mail.compose (visible:false, no send)");
    const composeResp = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "mail",
      adapter_op: "compose",
      params: {
        to: ["cel-test-do-not-reply@example.invalid"],
        subject: "[CEL-TEST] mail adapter smoke",
        body: "Test marker: cel-mail-test-xyzzy-aaaa.\nThis draft must not be sent.",
        visible: false,
      },
    });
    const composeData = parseAdapterData(composeResp);
    const draftId = composeData?.draft_id ?? "";
    assert(
      typeof draftId === "string" && /^\d+$/.test(draftId),
      `compose returned numeric draft_id: ${draftId}`,
    );
    assert(
      composeData?.to_count === 1,
      `compose echoed to_count=1 (got ${composeData?.to_count})`,
    );

    // 3. search with a guaranteed-no-match marker — verifies the search
    //    machinery runs and returns the expected shape without exposing the
    //    user's actual inbox content.
    console.log("\n3. mail.search (no-match marker)");
    const noMatch = `cel-test-unmatched-marker-zzz-${Date.now()}`;
    const searchResp = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "mail",
      adapter_op: "search",
      params: { query: noMatch, limit: 5 },
    });
    const searchData = parseAdapterData(searchResp);
    assert(
      Array.isArray(searchData?.messages) && searchData.messages.length === 0,
      `search returned empty messages array for unique marker`,
    );

    // 4. Unknown op — clean error
    console.log("\n4. unknown adapter op — clean error");
    const badResp = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "mail",
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
    // No cleanup needed: compose without save creates an in-memory outgoing
    // message in Mail.app that's reclaimed on next Mail relaunch. Document
    // this in the adapter quirks.
    server.kill();
  }

  process.exit(failed > 0 ? 1 : 0);
}

main().catch((err) => {
  console.error("\nTest crashed:", err);
  process.exit(2);
});
