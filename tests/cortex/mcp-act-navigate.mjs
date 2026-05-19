#!/usr/bin/env node
/**
 * MCP cel_act navigate test: verify the CDP Page.navigate plumbing.
 *
 * Before this, the only documented MCP path for changing a browser tab's URL
 * was `cel_act cdp_eval` with `window.location.href = ...`. In practice that
 * produced stray about:blank tabs/windows in the user's view, because Chrome's
 * about:blank target sometimes inherited the assignment instead of the focused
 * CEL tab. CEL's internal goal-runner already used `cel.cdpNavigate(url)`
 * (Page.navigate, navigates in place); this test exercises the new MCP-facing
 * `navigate` action that wraps that binding.
 *
 * Schema-level checks always run. Live CDP check only runs when a Chromium
 * browser with --remote-debugging-port is reachable via `cel_see cdp_status`.
 *
 * PREREQUISITES:
 *   1. macOS accessibility permissions granted (for any cel_* call)
 *   2. MCP server built: cd mcp-server && pnpm build
 *   3. For the live CDP check: `dilipod browser ensure --url https://example.com`
 *      OR any Chromium with --remote-debugging-port=9222 (or 9333 for CEL-owned)
 *
 * Run: node tests/cortex/mcp-act-navigate.mjs
 *   or: make test-cortex-mcp-navigate
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
  console.log("\n=== MCP cel_act navigate Test ===\n");

  const server = startMcpServer();
  await sleep(2500);

  let passed = 0;
  let failed = 0;
  let skipped = 0;
  function assert(condition, message) {
    if (condition) {
      console.log(`  ✓ ${message}`);
      passed++;
    } else {
      console.error(`  ✗ ${message}`);
      failed++;
    }
  }
  function skip(message) {
    console.log(`  ⊘ ${message} (skipped)`);
    skipped++;
  }

  try {
    console.log("1. Initialize MCP");
    await server.send("initialize", {
      protocolVersion: "2024-11-05",
      capabilities: {},
      clientInfo: { name: "act-navigate-test", version: "1.0" },
    });

    // 2. Tool description advertises the navigate action. The MCP SDK doesn't
    //    serialize Zod discriminated unions into JSON Schema (inputSchema comes
    //    out as `{type:"object", properties:{}}`), so the description is the
    //    actual surface area clients see at tools/list time. The schema-error
    //    paths below cover that the action is wired at runtime.
    console.log("\n2. tools/list advertises the navigate action in the description");
    const toolsList = await server.send("tools/list", {});
    const celAct = toolsList.result?.tools?.find((t) => t.name === "cel_act");
    assert(celAct, "cel_act tool present");
    const description = celAct?.description ?? "";
    assert(
      description.includes("navigate"),
      "cel_act description mentions navigate",
    );

    // 3. Schema validation — missing `url` must be rejected. This path doesn't
    //    require AX permission or CDP — Zod rejects before reaching the handler.
    console.log("\n3. cel_act navigate without url → schema error");
    const missingUrlResp = await server.callTool("cel_act", { action: "navigate" });
    const missingUrlText = missingUrlResp.result?.content?.[0]?.text ?? "";
    assert(
      missingUrlResp.result?.isError === true || missingUrlText.toLowerCase().includes("invalid"),
      `Missing url is rejected: ${missingUrlText.slice(0, 200)}`,
    );

    // 4. Schema validation — non-string url must be rejected.
    console.log("\n4. cel_act navigate with non-string url → schema error");
    const badUrlResp = await server.callTool("cel_act", { action: "navigate", url: 42 });
    const badUrlText = badUrlResp.result?.content?.[0]?.text ?? "";
    assert(
      badUrlResp.result?.isError === true || badUrlText.toLowerCase().includes("invalid"),
      `Non-string url is rejected: ${badUrlText.slice(0, 200)}`,
    );

    // 4b. Schema validation — bogus wait_until must be rejected. Pins
    //     the new canonical knob: only the documented enum values pass.
    //     Without this check, a typo silently degrades to "domcontentloaded"
    //     and callers think they configured something they didn't.
    console.log("\n4b. cel_act navigate with bogus wait_until → schema error");
    const badWaitResp = await server.callTool("cel_act", {
      action: "navigate",
      url: "https://example.com",
      wait_until: "domreadyish",
    });
    const badWaitText = badWaitResp.result?.content?.[0]?.text ?? "";
    assert(
      badWaitResp.result?.isError === true || badWaitText.toLowerCase().includes("invalid"),
      `Bogus wait_until is rejected: ${badWaitText.slice(0, 200)}`,
    );

    // 5. Live CDP check — only run if a browser target is reachable. The
    //    cdp_status mode returns { connected: bool, targets: [...] } so we can
    //    branch cleanly without trying to launch Chrome from the test harness.
    console.log("\n5. cel_act navigate against a live CDP target");
    let cdpAvailable = false;
    try {
      const statusResp = await server.callTool("cel_see", { mode: "cdp_status" });
      const status = parseText(statusResp);
      cdpAvailable =
        status?.connected === true ||
        (Array.isArray(status?.targets) && status.targets.length > 0);
    } catch (e) {
      // cdp_status might fail in schema-only mode — that's fine, just skip.
    }

    if (!cdpAvailable) {
      skip("No CDP target reachable — run `dilipod browser ensure` to exercise the live path");
    } else {
      const navResp = await server.callTool("cel_act", {
        action: "navigate",
        url: "https://example.com",
      });
      const navData = parseText(navResp);
      assert(
        navData?.success === true,
        `navigate returned success: ${JSON.stringify(navData)?.slice(0, 200)}`,
      );
      const resultStr = String(navData?.result ?? "");
      assert(
        resultStr.includes("Navigated to") && resultStr.includes("https://example.com"),
        `Result mentions the navigated URL: ${resultStr}`,
      );
      // The canonical navigate path waits for domcontentloaded by
      // default, so a follow-on cdp_page should immediately see the
      // loaded content. This is the regression test that proves the
      // architectural change actually waits — the prior bare-cdpNavigate
      // path returned before the page content was reachable.
      const pageResp = await server.callTool("cel_see", { mode: "cdp_page" });
      const pageText = JSON.stringify(parseText(pageResp) ?? {}).slice(0, 4000);
      assert(
        pageText.toLowerCase().includes("example domain"),
        `Loaded page content reachable after navigate: ${pageText.slice(0, 200)}`,
      );

      // 5b. dismiss_overlays: false should still return success on a
      //     page with no overlay (example.com). We can't observe the
      //     "did not run" property without instrumentation, so this is
      //     mainly a smoke test that the flag round-trips through the
      //     schema → contract → cortex pipeline without breaking.
      console.log("\n5b. cel_act navigate with dismiss_overlays: false");
      const noDismissResp = await server.callTool("cel_act", {
        action: "navigate",
        url: "https://example.com",
        dismiss_overlays: false,
        wait_until: "load",
      });
      const noDismissData = parseText(noDismissResp);
      assert(
        noDismissData?.success === true,
        `navigate with dismiss_overlays=false succeeded: ${JSON.stringify(noDismissData)?.slice(0, 200)}`,
      );

      // 5c. Result string carries the formatter's final_url + load_ms.
      //     Both dispatch paths (TS adapter + cortex fallback) must
      //     produce a non-empty `final:` and a load_ms reading. Pre-fix,
      //     the TS adapter returned plain `{success:true}` with no
      //     `data`, so the formatter showed "(final: <input-url>, 0ms)"
      //     regardless of where the page actually landed — this test
      //     pins the parity contract from gap-2 of the canonical
      //     promotion. We can't strictly assert load_ms > 0 (a cached
      //     page on localhost can plausibly load in 0ms), but the
      //     formatter must surface a real `https://...` final URL.
      console.log("\n5c. result string surfaces a real final_url");
      const parityResp = await server.callTool("cel_act", {
        action: "navigate",
        url: "https://example.com",
        wait_until: "load",
      });
      const parityResult = String(parseText(parityResp)?.result ?? "");
      assert(
        /final: https?:\/\/[^,]+/.test(parityResult),
        `final_url is a real URL, not an empty/literal placeholder (got "${parityResult}")`,
      );
      assert(
        /\d+ms\)/.test(parityResult),
        `load_ms suffix present (got "${parityResult}")`,
      );
    }
  } catch (e) {
    console.error("\nTest error:", e.message);
    failed++;
  } finally {
    server.kill();
  }

  console.log(
    `\n=== Results: ${passed} passed, ${failed} failed, ${skipped} skipped ===\n`,
  );
  process.exit(failed > 0 ? 1 : 0);
}

main();
