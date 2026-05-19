#!/usr/bin/env node
/**
 * Smoke test for the Messages adapter end-to-end through MCP.
 *
 * READ-ONLY: messages.list_threads, messages.read_thread, messages.search.
 * No artifacts are created — nothing to clean up.
 *
 * Handles the case where the user has no Messages history (clean Mac, never
 * signed into iMessage) by treating an empty thread list as a soft pass for
 * downstream steps that depend on a thread existing.
 *
 * PREREQUISITES:
 *   - macOS with Messages.app, ~/Library/Messages/chat.db reachable
 *   - Full Disk Access granted to the host process (otherwise SQLite cannot
 *     open chat.db). Grant via System Settings → Privacy & Security →
 *     Full Disk Access, then restart the host.
 *   - MCP server built: cd mcp-server && pnpm build
 *   - Native module built: make build-napi
 *   - Adapter binary built (ProcessDriver): make build-adapters
 *     (cortex spawns target/release/adapter-messages via adapter.json)
 *
 * Run: node tests/cortex/mcp-adapter-messages.mjs
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
  console.log("\n=== MCP Messages Adapter Smoke Test ===\n");

  const server = startMcpServer();
  // Wait for cortex boot + at least one tick (200ms) so adapters are
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
      clientInfo: { name: "messages-adapter-test", version: "1.0" },
    });

    // 2. list_threads — verify shape; tolerate empty history. Also detect
    //    Full Disk Access not granted and soft-skip the rest with a clear
    //    message, since the adapter cannot run without that permission.
    console.log("\n2. messages.list_threads (limit 5)");
    const listResp = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "messages",
      adapter_op: "list_threads",
      params: { limit: 5 },
    });
    const listErrText = listResp.result?.content?.[0]?.text ?? "";
    if (listErrText.includes("Full Disk Access")) {
      console.error(
        "  ⚠ Skipping — Full Disk Access not granted to host process.\n" +
          "    Grant it via System Settings → Privacy & Security → Full Disk Access, then restart the host.",
      );
      console.log(`\n=== Summary: ${passed} passed, ${failed} failed (skipped due to permissions) ===\n`);
      server.kill();
      process.exit(0);
    }
    const listData = parseAdapterData(listResp);
    const threads = listData?.threads ?? null;
    assert(
      Array.isArray(threads),
      `list_threads returned an array (got ${threads === null ? "null" : threads.length + " threads"})`,
    );

    // If we have any threads, verify the first one has the expected shape.
    let firstThreadId = null;
    if (Array.isArray(threads) && threads.length > 0) {
      const t = threads[0];
      const hasShape =
        typeof t.thread_id === "string" &&
        (t.last_message_at === null || typeof t.last_message_at === "string") &&
        Array.isArray(t.participants);
      assert(hasShape, `first thread has expected shape: ${JSON.stringify(Object.keys(t))}`);
      firstThreadId = t.thread_id;
    } else {
      console.log("  (no messages history — skipping read_thread step)");
    }

    // 3. read_thread (only if we found one)
    if (firstThreadId) {
      console.log("\n3. messages.read_thread (first thread, limit 5)");
      const readResp = await server.callTool("cel_act", {
        action: "adapter_action",
        adapter: "messages",
        adapter_op: "read_thread",
        params: { thread_id: firstThreadId, limit: 5 },
      });
      const readData = parseAdapterData(readResp);
      const messages = readData?.messages ?? null;
      assert(
        Array.isArray(messages),
        `read_thread returned an array (got ${messages === null ? "null" : messages.length + " messages"})`,
      );
      if (Array.isArray(messages) && messages.length > 0) {
        const m = messages[0];
        const hasShape =
          typeof m.from === "string" &&
          typeof m.is_outgoing === "boolean" &&
          (m.sent_at === null || typeof m.sent_at === "string");
        assert(hasShape, `first message has expected shape: ${JSON.stringify(Object.keys(m))}`);
      }

      // 3b. read_thread with bogus id — clean error
      console.log("\n3b. messages.read_thread (bogus id) — clean error");
      const badThreadResp = await server.callTool("cel_act", {
        action: "adapter_action",
        adapter: "messages",
        adapter_op: "read_thread",
        params: { thread_id: "iMessage;-;+15550000000-CEL-TEST-NONEXISTENT", limit: 1 },
      });
      const badThreadText = badThreadResp.result?.content?.[0]?.text ?? "";
      assert(
        badThreadResp.result?.isError === true || badThreadText.includes("not found"),
        `bogus thread_id errored cleanly: ${badThreadText.slice(0, 200)}`,
      );
    }

    // 4. search for a unique no-match marker
    console.log("\n4. messages.search (no-match marker)");
    const marker = `cel-test-unmatched-zzz-${Date.now()}`;
    const searchResp = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "messages",
      adapter_op: "search",
      params: { query: marker, limit: 5 },
    });
    const searchData = parseAdapterData(searchResp);
    assert(
      Array.isArray(searchData?.messages) && searchData.messages.length === 0,
      `search returned empty array for unique marker`,
    );

    // 5. Unknown op — clean error
    console.log("\n5. unknown adapter op — clean error");
    const badResp = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "messages",
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
    // No artifacts to clean — read-only adapter.
    server.kill();
  }

  process.exit(failed > 0 ? 1 : 0);
}

main().catch((err) => {
  console.error("\nTest crashed:", err);
  process.exit(2);
});
