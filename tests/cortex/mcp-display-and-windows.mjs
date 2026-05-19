#!/usr/bin/env node
/**
 * MCP cel_see screenshot + windows test: verify the multi-display fixes.
 *
 * Two changes are exercised:
 * 1. `cel_see windows` reads `kCGWindowIsOnscreen` as a CFBoolean instead of
 *    a CFNumber. Before the fix, every visible window came back with
 *    `is_on_screen: false` because the dict value is a CFBoolean and
 *    `get_dict_i32` silently returned None → defaulted to 0 → false.
 * 2. `cel_see screenshot` accepts an optional `display_id`. With no id it
 *    auto-selects the display containing the frontmost app's key window.
 *    With an explicit id it captures that monitor.
 *
 * The tests are tolerant of single-display dev machines — the multi-display
 * branch is documented in tests/cortex/README.md for manual verification.
 *
 * PREREQUISITES:
 *   1. macOS accessibility permissions granted
 *   2. MCP server built: cd mcp-server && pnpm build
 *   3. cel-napi rebuilt: make build-napi
 *
 * Run: node tests/cortex/mcp-display-and-windows.mjs
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

  proc.stderr.on("data", (data) => process.stderr.write(`[mcp] ${data}`));

  async function send(method, params = {}) {
    const id = nextId++;
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject });
      proc.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
      setTimeout(() => {
        if (pending.has(id)) {
          pending.delete(id);
          reject(new Error(`Timeout waiting for response to ${method}`));
        }
      }, 60000);
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
  console.log("\n=== MCP cel_see Display + Window Enumeration Test ===\n");

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
      clientInfo: { name: "display-windows-test", version: "1.0" },
    });

    console.log("\n2. cel_see windows reports at least one on-screen window");
    const winResp = await server.callTool("cel_see", { mode: "windows" });
    const winList = parseResult(winResp);
    assert(
      Array.isArray(winList) && winList.length > 0,
      `windows mode returns a non-empty list: ${winList?.length ?? 0} windows`,
    );
    const onScreenCount = Array.isArray(winList)
      ? winList.filter((w) => w.is_on_screen === true).length
      : 0;
    assert(
      onScreenCount > 0,
      `at least one window has is_on_screen: true (got ${onScreenCount} of ${winList?.length ?? 0}). ` +
        `Before the CFBoolean fix this was always 0.`,
    );

    console.log("\n3. cel_see monitors lists at least one display");
    const monResp = await server.callTool("cel_see", { mode: "monitors" });
    const monList = parseResult(monResp);
    assert(
      Array.isArray(monList) && monList.length > 0,
      `monitors mode returns a non-empty list: ${monList?.length ?? 0} monitors`,
    );

    console.log("\n4. cel_see screenshot (default — auto-selects display)");
    const shotResp = await server.callTool("cel_see", { mode: "screenshot" });
    const img = shotResp.result?.content?.[0];
    assert(
      img && img.type === "image" && img.mimeType === "image/png",
      `screenshot returned a PNG image content block`,
    );
    const b64Default = img?.data ?? "";
    assert(
      typeof b64Default === "string" && b64Default.length > 1000,
      `screenshot base64 is plausibly non-trivial (${b64Default.length} chars; > 1000 expected)`,
    );

    console.log("\n5. cel_see screenshot with explicit display_id");
    const firstId = Array.isArray(monList) && monList.length > 0 ? monList[0].id : 0;
    const shotResp2 = await server.callTool("cel_see", {
      mode: "screenshot",
      display_id: firstId,
    });
    const img2 = shotResp2.result?.content?.[0];
    assert(
      img2 && img2.type === "image" && img2.mimeType === "image/png",
      `screenshot with display_id=${firstId} returned a PNG`,
    );
    const b64Explicit = img2?.data ?? "";
    assert(
      typeof b64Explicit === "string" && b64Explicit.length > 1000,
      `explicit-display screenshot base64 is plausibly non-trivial (${b64Explicit.length} chars)`,
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
