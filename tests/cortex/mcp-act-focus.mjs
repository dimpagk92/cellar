#!/usr/bin/env node
/**
 * MCP cel_act target_app test: verify the keystroke-focus fix.
 *
 * Before this fix, `cel_act type` used CGEventPost which routes to the
 * system-wide focused element. If focus oscillated between when the caller
 * queried the screen and when the keystroke fired, characters would land in
 * the wrong app (e.g. into the MCP host's prompt input). The fix adds an
 * optional `target_app` field that activates the requested app and polls
 * until it's macOS-frontmost before firing.
 *
 * This test:
 * 1. Opens Finder via `open` (legitimate setup; not what we're testing).
 * 2. Spawns the MCP server and connects via stdio JSON-RPC.
 * 3. Activates a DIFFERENT app first (Notes), so Finder is NOT frontmost.
 * 4. Calls cel_act type with target_app="Finder" — fix should activate
 *    Finder before firing the keystroke.
 * 5. Verifies the response includes the focus diagnostic ("activated Finder").
 * 6. Tests the failure path with a bogus target_app — must error cleanly.
 *
 * PREREQUISITES:
 *   1. macOS accessibility permissions granted
 *   2. MCP server built: cd mcp-server && pnpm build
 *
 * Run: node tests/cortex/mcp-act-focus.mjs
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

function getFrontmost() {
  return execFileSync(
    "osascript",
    [
      "-e",
      'tell application "System Events" to get name of first application process whose frontmost is true',
    ],
    { encoding: "utf8" },
  ).trim();
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

async function main() {
  console.log("\n=== MCP cel_act target_app Focus Test ===\n");

  // Setup: open a Finder window so there's something to receive keystrokes
  execFileSync("open", ["/Users/dimitriospagkratis/dilipod/cellar"]);
  await sleep(400);

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
      clientInfo: { name: "act-focus-test", version: "1.0" },
    });

    // 2. Force Finder OFF-frontmost so the fix has work to do
    console.log("\n2. Steal focus away from Finder (activate Notes)");
    activate("Notes");
    await sleep(400);
    const beforeFront = getFrontmost();
    assert(
      beforeFront !== "Finder",
      `Pre-state: frontmost is "${beforeFront}" (not Finder, so the fix must activate it)`,
    );

    // 3. Fire type WITH target_app — fix should activate Finder, type, return diagnostic
    console.log("\n3. cel_act type with target_app=Finder");
    const typeResp = await server.callTool("cel_act", {
      action: "type",
      text: "a",
      target_app: "Finder",
    });
    const typeData = parseText(typeResp);
    assert(
      typeData?.success === true,
      `cel_act returned success: ${JSON.stringify(typeData)?.slice(0, 200)}`,
    );
    const resultStr = String(typeData?.result ?? "");
    assert(
      resultStr.includes("Typed"),
      `Result mentions the typed text: ${resultStr}`,
    );
    assert(
      resultStr.includes("focus:"),
      `Result includes focus diagnostic: ${resultStr}`,
    );
    assert(
      resultStr.includes("activated Finder"),
      `Result confirms Finder was activated by the helper: ${resultStr}`,
    );

    // 4. Confirm Finder is now frontmost (the fix didn't bail out silently)
    const afterFront = getFrontmost();
    assert(
      afterFront === "Finder",
      `Post-state: Finder is frontmost (was "${afterFront}")`,
    );

    // 5. Bogus target_app — must error cleanly, not type into wrong window
    console.log("\n4. cel_act type with bogus target_app");
    const bogusResp = await server.callTool("cel_act", {
      action: "type",
      text: "x",
      target_app: "ThisAppDoesNotExist_zzz",
    });
    const bogusText = bogusResp.result?.content?.[0]?.text ?? "";
    const bogusIsError =
      bogusResp.result?.isError === true ||
      bogusText.includes("never became frontmost") ||
      bogusText.includes("Action aborted") ||
      bogusText.includes("Keystroke aborted") ||
      bogusText.includes("error"); // osascript may fail the activate step first
    assert(
      bogusIsError,
      `Bogus target_app produced an error: ${bogusText.slice(0, 200)}`,
    );

    // 6. No target_app — legacy behavior, just types
    console.log("\n5. cel_act type without target_app (legacy)");
    const legacyResp = await server.callTool("cel_act", {
      action: "type",
      text: "z",
    });
    const legacyData = parseText(legacyResp);
    assert(
      legacyData?.success === true,
      `Legacy call still works: ${JSON.stringify(legacyData)?.slice(0, 150)}`,
    );
    const legacyResult = String(legacyData?.result ?? "");
    assert(
      !legacyResult.includes("focus:"),
      `Legacy result does NOT include focus diagnostic: ${legacyResult}`,
    );

    // 7. Coord-based action (mouse_move) with target_app — same focus-race
    //    fix should apply, not just keystrokes. The CGEventPost path is
    //    shared between keystroke and mouse events.
    console.log("\n6. cel_act mouse_move with target_app=Finder (coord-based focus fix)");
    activate("Notes");
    await sleep(400);
    const beforeMove = getFrontmost();
    assert(
      beforeMove !== "Finder",
      `Pre-state: frontmost is "${beforeMove}" (not Finder)`,
    );
    const moveResp = await server.callTool("cel_act", {
      action: "mouse_move",
      x: 400,
      y: 400,
      target_app: "Finder",
    });
    const moveData = parseText(moveResp);
    assert(
      moveData?.success === true,
      `mouse_move with target_app succeeded: ${JSON.stringify(moveData)?.slice(0, 200)}`,
    );
    const moveResult = String(moveData?.result ?? "");
    assert(
      moveResult.includes("Moved mouse"),
      `mouse_move result mentions the move: ${moveResult}`,
    );
    assert(
      moveResult.includes("focus:") && moveResult.includes("activated Finder"),
      `mouse_move result includes focus diagnostic: ${moveResult}`,
    );

    // 8. Click with target_app — same shape, exercises the click branch
    console.log("\n7. cel_act click with target_app=Finder");
    activate("Notes");
    await sleep(400);
    const clickResp = await server.callTool("cel_act", {
      action: "click",
      x: 400,
      y: 400,
      target_app: "Finder",
    });
    const clickData = parseText(clickResp);
    assert(
      clickData?.success === true,
      `click with target_app succeeded: ${JSON.stringify(clickData)?.slice(0, 200)}`,
    );
    const clickResult = String(clickData?.result ?? "");
    assert(
      clickResult.includes("Clicked") && clickResult.includes("focus:"),
      `click result includes focus diagnostic: ${clickResult}`,
    );

    // 9. Coord-based action without target_app — legacy behavior, no diagnostic
    console.log("\n8. cel_act mouse_move without target_app (legacy)");
    const moveLegacyResp = await server.callTool("cel_act", {
      action: "mouse_move",
      x: 500,
      y: 500,
    });
    const moveLegacyData = parseText(moveLegacyResp);
    assert(
      moveLegacyData?.success === true,
      `Legacy mouse_move still works: ${JSON.stringify(moveLegacyData)?.slice(0, 150)}`,
    );
    const moveLegacyResult = String(moveLegacyData?.result ?? "");
    assert(
      !moveLegacyResult.includes("focus:"),
      `Legacy mouse_move result does NOT include focus diagnostic: ${moveLegacyResult}`,
    );

    // 10. cel_perceive feed surfaces landedInWrongApp when the action visibly
    //     changed nothing AND the system frontmost disagrees with the cortex's
    //     tracked app. Sets up the mismatch by booting a Finder-tracking
    //     perception session, then activating Notes before firing a no-op
    //     type. The cortex sees no Finder diff (since Notes ate the keys),
    //     and the diagnostic should report Notes as the actual frontmost.
    console.log("\n9. cel_perceive feed surfaces landed_in_wrong_app");
    activate("Finder");
    await sleep(400);
    await server.callTool("cel_perceive", {
      mode: "start",
      goal: "test feed wrong-app diagnostic",
      enable_suggestions: false,
    });
    await sleep(800);
    activate("Notes");
    await sleep(400);
    await server.callTool("cel_act", { action: "type", text: "wrongplace" });
    const feedResp = await server.callTool("cel_perceive", {
      mode: "feed",
      action: "type 'wrongplace'",
      target: "Finder filter field",
      expected: "Finder shows filter results",
    });
    const feedData = parseText(feedResp);
    assert(
      feedData && feedData.actionLanded === false,
      `feed reports actionLanded: false (no Finder diff): ${JSON.stringify(feedData)?.slice(0, 200)}`,
    );
    assert(
      feedData && feedData.landedInWrongApp && feedData.landedInWrongApp.actual === "Notes",
      `feed surfaces landedInWrongApp with actual="Notes": ${JSON.stringify(feedData?.landedInWrongApp)}`,
    );
    await server.callTool("cel_perceive", { mode: "stop" });
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
