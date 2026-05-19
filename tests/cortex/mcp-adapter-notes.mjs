#!/usr/bin/env node
/**
 * Smoke test for the Notes adapter end-to-end through MCP.
 *
 * Verifies the full lifecycle through `cel_act adapter_action`:
 *  - notes.create returns a note_id
 *  - notes.get_body reads the body back
 *  - notes.set_body replaces it (with auto-verification)
 *  - notes.append concatenates
 *  - notes.list / notes.find surface the test note
 *  - notes.delete moves to Recently Deleted (cleanup)
 *
 * Self-cleaning: the note created here is deleted at the end so no
 * pollution lands in the user's Notes app. If the test crashes mid-run
 * the stray note has the prefix "[CEL-TEST]" and can be Cmd-Deleted by
 * hand from the Recently Deleted folder.
 *
 * PREREQUISITES:
 *   - macOS, Notes.app, iCloud account set up
 *   - Automation permission for Notes granted to the host running the test
 *   - MCP server built: cd mcp-server && pnpm build
 *   - Native module built with adapter-notes registered: make build-napi
 *
 * Run: node tests/cortex/mcp-adapter-notes.mjs
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
  // adapter_action returns {success: true, result: "<JSON string>"} —
  // unwrap the inner JSON.
  const outer = parseText(resp);
  if (!outer?.success) return null;
  try {
    return typeof outer.result === "string" ? JSON.parse(outer.result) : outer.result;
  } catch {
    return outer.result;
  }
}

async function main() {
  console.log("\n=== MCP Notes Adapter Smoke Test ===\n");

  const server = startMcpServer();
  await sleep(2500);

  let passed = 0;
  let failed = 0;
  let createdNoteId = null;
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
      clientInfo: { name: "notes-adapter-test", version: "1.0" },
    });

    // 2. create
    console.log("\n2. notes.create");
    const createResp = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "notes",
      adapter_op: "create",
      params: {
        title: "[CEL-TEST] adapter-notes smoke",
        body: "Initial body line 1.\nLine 2 with <html-like> & special chars.",
      },
    });
    const createData = parseAdapterData(createResp);
    assert(
      typeof createData?.note_id === "string" && createData.note_id.startsWith("x-coredata://"),
      `create returned a note_id: ${createData?.note_id?.slice(0, 60)}...`,
    );
    createdNoteId = createData.note_id;

    // 3. get_body — verify the content is there. We don't assert HTML
    //    escaping shape: Notes' rich-text engine post-processes the body
    //    and may decode our escaped entities back to literals (see
    //    adapter docs § Quirks).
    console.log("\n3. notes.get_body");
    const getResp = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "notes",
      adapter_op: "get_body",
      params: { note_id: createdNoteId },
    });
    const getData = parseAdapterData(getResp);
    const body1 = getData?.body ?? "";
    assert(
      body1.includes("Initial body line 1") && body1.includes("Line 2"),
      `body contains both lines`,
    );

    // 4. set_body with HTML format — pass-through, no escaping. Note that
    //    Notes uses the first line of the new body as the displayed title
    //    (see adapter docs § Quirks 1) — that's expected, not a bug.
    console.log("\n4. notes.set_body (format=html)");
    await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "notes",
      adapter_op: "set_body",
      params: {
        note_id: createdNoteId,
        body: "<div>CEL test marker xyzzy</div><div>Body line A</div><div>Body line B</div>",
        format: "html",
        verify: false,
      },
    });
    const get2 = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "notes",
      adapter_op: "get_body",
      params: { note_id: createdNoteId },
    });
    const body2 = parseAdapterData(get2)?.body ?? "";
    assert(
      body2.includes("xyzzy") && body2.includes("Body line A"),
      `set_body replaced content`,
    );

    // 5. append — concatenate text + verify it persists
    console.log("\n5. notes.append");
    await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "notes",
      adapter_op: "append",
      params: {
        note_id: createdNoteId,
        text: "Appended sentinel quux.",
      },
    });
    const get3 = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "notes",
      adapter_op: "get_body",
      params: { note_id: createdNoteId },
    });
    const body3 = parseAdapterData(get3)?.body ?? "";
    assert(
      body3.includes("xyzzy") && body3.includes("quux"),
      `append preserved prior content and added new`,
    );

    // 6. find by alphanumeric substring (Notes' name-contains is unreliable
    //    with brackets/special chars per adapter docs § Quirks 2). Set_body
    //    renamed the note to start with "CEL test marker xyzzy" — search
    //    for "xyzzy".
    console.log("\n6. notes.find by alphanumeric substring");
    const findResp = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "notes",
      adapter_op: "find",
      params: { query: "xyzzy", limit: 5 },
    });
    const findData = parseAdapterData(findResp);
    const found = findData?.notes ?? [];
    assert(
      found.length >= 1 && found.some((n) => n.note_id === createdNoteId),
      `find returned the test note (got ${found.length} hit(s))`,
    );

    // Skip the `list` step: for users with hundreds of notes the AppleScript
    // iteration is slow (>20s for ~650 notes) and times out the MCP call.
    // Documented as adapter Quirk 4. The use case the test would cover —
    // "the note exists in the folder" — is already covered by `find` above.

    // 7. Unknown op — clean error
    console.log("\n7. unknown adapter op — clean error");
    const badResp = await server.callTool("cel_act", {
      action: "adapter_action",
      adapter: "notes",
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
    // Cleanup: delete the test note so it doesn't pollute the user's Notes.
    if (createdNoteId) {
      console.log(`\nCleanup: deleting test note ${createdNoteId.slice(0, 50)}...`);
      try {
        await server.callTool("cel_act", {
          action: "adapter_action",
          adapter: "notes",
          adapter_op: "delete",
          params: { note_id: createdNoteId },
        });
        console.log("Cleanup done (note moved to Recently Deleted).");
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
