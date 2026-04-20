#!/usr/bin/env node
/**
 * MCP protocol test: Verify cel_perceive works end-to-end via JSON-RPC.
 *
 * Spawns the MCP server as a subprocess and sends cel_perceive commands
 * via stdin, reads responses from stdout.
 *
 * PREREQUISITES:
 * 1. macOS accessibility permissions granted
 * 2. MCP server built: cd mcp-server && pnpm build
 * 3. Native module rebuilt with Cortex bindings
 *
 * Run: node tests/cortex/mcp-protocol.mjs
 *   or: make test-cortex-mcp
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
    // MCP server logs to stderr — show for debugging
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

async function main() {
  console.log("\n=== MCP cel_perceive Protocol Test ===\n");

  const server = startMcpServer();

  // Wait for server to initialize
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

  try {
    // 1. Initialize MCP
    console.log("1. Initialize MCP connection");
    const initResp = await server.send("initialize", {
      protocolVersion: "2024-11-05",
      capabilities: {},
      clientInfo: { name: "test-client", version: "1.0" },
    });
    assert(initResp.result?.serverInfo?.name === "cel", `Server name: ${initResp.result?.serverInfo?.name}`);

    // 2. List tools
    console.log("\n2. List tools");
    const toolsResp = await server.send("tools/list", {});
    const toolNames = (toolsResp.result?.tools ?? []).map((t) => t.name);
    assert(toolNames.includes("cel_perceive"), "cel_perceive tool available");
    assert(toolNames.includes("cel_see"), "cel_see tool available");

    // 3. cel_perceive start
    console.log("\n3. cel_perceive start");
    const startResp = await server.callTool("cel_perceive", {
      mode: "start",
      goal: "Test the Rust Cortex perception engine",
    });
    const startContent = startResp.result?.content?.[0]?.text;
    if (startContent) {
      const startData = JSON.parse(startContent);
      assert(startData.success === true, "Start succeeded");
      assert(startData.initialContext?.app, `App: ${startData.initialContext?.app}`);
      console.log(`   Elements: ${startData.initialContext?.elementCount}`);
    } else {
      assert(false, `Start failed: ${JSON.stringify(startResp)}`);
    }

    // 4. Wait for ticks
    console.log("\n4. Wait for perception ticks (1s)...");
    await sleep(1000);

    // 5. cel_perceive read
    console.log("\n5. cel_perceive read");
    const readResp = await server.callTool("cel_perceive", { mode: "read" });
    const readContent = readResp.result?.content?.[0]?.text;
    if (readContent) {
      const readData = JSON.parse(readContent);
      assert(readData.contextSummary, "Read returned contextSummary");
      assert(readData.contextSummary?.elementCount > 0, `Elements: ${readData.contextSummary?.elementCount}`);
      console.log(`   App: ${readData.contextSummary?.app}`);
      console.log(`   Actionable: ${readData.contextSummary?.actionableCount}`);
      console.log(`   Screenshot needed: ${readData.screenshotNeeded}`);
    } else {
      assert(false, `Read failed: ${JSON.stringify(readResp)}`);
    }

    // 6. cel_perceive feed
    console.log("\n6. cel_perceive feed");
    const feedResp = await server.callTool("cel_perceive", {
      mode: "feed",
      action: "test action — no real UI change",
      target: "test-element",
    });
    const feedContent = feedResp.result?.content?.[0]?.text;
    if (feedContent) {
      const feedData = JSON.parse(feedContent);
      assert(typeof feedData.actionLanded === "boolean", `Action landed: ${feedData.actionLanded}`);
      assert(Array.isArray(feedData.anomalies), `Anomalies: ${feedData.anomalies?.length}`);
    } else {
      assert(false, `Feed failed: ${JSON.stringify(feedResp)}`);
    }

    // 7. cel_perceive status
    console.log("\n7. cel_perceive status");
    const statusResp = await server.callTool("cel_perceive", { mode: "status" });
    const statusContent = statusResp.result?.content?.[0]?.text;
    if (statusContent) {
      const statusData = JSON.parse(statusContent);
      assert(statusData.active === true, "Status: active");
      assert(statusData.cycleCount > 0, `Cycles: ${statusData.cycleCount}`);
      assert(statusData.confidence === 1.0, `Confidence: ${statusData.confidence}`);
      console.log(`   Uptime: ${statusData.uptimeMs}ms`);
    } else {
      assert(false, `Status failed: ${JSON.stringify(statusResp)}`);
    }

    // 8. cel_perceive stop
    console.log("\n8. cel_perceive stop");
    const stopResp = await server.callTool("cel_perceive", { mode: "stop" });
    const stopContent = stopResp.result?.content?.[0]?.text;
    if (stopContent) {
      const stopData = JSON.parse(stopContent);
      assert(stopData.durationMs > 0, `Duration: ${stopData.durationMs}ms`);
      assert(typeof stopData.totalActions === "number", `Actions: ${stopData.totalActions}`);
    } else {
      assert(false, `Stop failed: ${JSON.stringify(stopResp)}`);
    }
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
