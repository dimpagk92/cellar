#!/usr/bin/env node
/**
 * Unit-style test for `executeNavigateAction` payload shape.
 *
 * Stubs the `Cel` napi binding so the test runs entirely in-process —
 * no MCP server, no real cortex, no browser. The point is to lock the
 * canonical payload shape that `handleCelAct` sends through
 * `cel.canonicalExecuteStep` for the navigate action variant.
 *
 * If a future refactor renames `wait_until` to `waitUntil`, drops a
 * field, or starts sending it under a different `type`, the cortex's
 * `dispatch_navigate` (which deserializes via `PlannedAction::Navigate`)
 * would silently get wrong defaults. This test fails loudly first.
 *
 * Run: node tests/cortex/mcp-act-navigate-payload.mjs
 *   or: make test-cortex-mcp-navigate-payload (if wired)
 */

import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const projectRoot = resolve(__dirname, "../..");

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

/**
 * Build a stub Cel that records every `canonicalExecuteStep` call. The
 * boot/cortex methods are no-ops so `ensureCortexForCanonicalAction`
 * doesn't fan out to the napi bridge. The stub returns whatever
 * `nextResult` is set to, which the test can swap per-call to simulate
 * adapter / fallback / error responses.
 */
function makeStubCel() {
  const calls = [];
  let nextResult = { status: "ok", data: { url: "stub", final_url: "stub", redirected: false, load_ms: 0 } };
  return {
    cel: {
      isCortexRunning() { return true; },
      bootCortex() { /* no-op */ },
      async canonicalExecuteStep(step) {
        calls.push(step);
        return nextResult;
      },
      // axPermissionGuard reads `isAxPermissionGranted` at handler entry.
      // Pretend permissions are granted so the test runs deterministically
      // regardless of the host machine's real AX state.
      isAxPermissionGranted: true,
      requestAxPermission() { /* no-op */ },
    },
    calls,
    setNextResult(r) { nextResult = r; },
  };
}

async function main() {
  console.log("\n=== executeNavigateAction payload shape ===\n");

  const { handleCelAct } = await import(
    resolve(projectRoot, "mcp-server/dist/tools/cel-act.js")
  );

  // 1. Default args — every canonical knob must default to the
  //    documented value, not be omitted entirely. Omitting fields would
  //    let the Rust contract's serde defaults take over silently, which
  //    diverges from the schema's announced behaviour.
  console.log("1. default args produce a fully-populated canonical payload");
  {
    const stub = makeStubCel();
    const out = await handleCelAct(stub.cel, {
      action: "navigate",
      url: "https://example.com",
    });
    assert(stub.calls.length === 1, `exactly one canonicalExecuteStep call (got ${stub.calls.length})`);
    const step = stub.calls[0];
    assert(step?.kind === "deterministic", `step.kind === "deterministic" (got ${step?.kind})`);
    assert(step?.action?.type === "navigate", `action.type === "navigate" (got ${step?.action?.type})`);
    assert(step?.action?.url === "https://example.com", "url forwarded verbatim");
    assert(
      step?.action?.wait_until === "domcontentloaded",
      `wait_until defaults to "domcontentloaded" (got ${step?.action?.wait_until})`,
    );
    assert(step?.action?.timeout_ms === 30_000, `timeout_ms defaults to 30_000 (got ${step?.action?.timeout_ms})`);
    assert(
      step?.action?.dismiss_overlays === true,
      `dismiss_overlays defaults to true (got ${step?.action?.dismiss_overlays})`,
    );
    // The MCP wrapper formats data into a human-friendly string. The
    // stub's nextResult provides a minimal data shape; the formatter
    // must tolerate missing fields without throwing.
    const text = out?.content?.[0]?.text ?? "";
    assert(text.includes("Navigated to https://example.com"), `result mentions URL (got ${text.slice(0, 120)})`);
  }

  // 2. Explicit args round-trip — caller-supplied wait_until /
  //    timeout_ms / dismiss_overlays must reach the Rust contract
  //    unchanged.
  console.log("\n2. explicit args round-trip into the canonical payload");
  {
    const stub = makeStubCel();
    await handleCelAct(stub.cel, {
      action: "navigate",
      url: "https://example.com",
      wait_until: "load",
      timeout_ms: 5000,
      dismiss_overlays: false,
    });
    const action = stub.calls[0]?.action;
    assert(action?.wait_until === "load", "wait_until=load round-trips");
    assert(action?.timeout_ms === 5000, "timeout_ms=5000 round-trips");
    assert(action?.dismiss_overlays === false, "dismiss_overlays=false round-trips");
  }

  // 3. Error pass-through — when the canonical step returns status:
  //    "err", the MCP handler should surface it as an error result, not
  //    swallow it as success.
  console.log("\n3. canonical err response surfaces as MCP error");
  {
    const stub = makeStubCel();
    stub.setNextResult({ status: "err", message: "No CDP target available" });
    const out = await handleCelAct(stub.cel, {
      action: "navigate",
      url: "https://example.com",
    });
    const text = out?.content?.[0]?.text ?? "";
    assert(out?.isError === true, `isError === true (got ${out?.isError})`);
    assert(
      text.includes("No CDP target available"),
      `message surfaced (got ${text.slice(0, 120)})`,
    );
  }

  // 4. Rich data shape — when the cortex returns the parity payload,
  //    the formatter should pull final_url / load_ms into the human
  //    string. Locks the contract that both adapter + fallback paths
  //    surface the same data shape.
  console.log("\n4. rich data shape feeds the formatted result");
  {
    const stub = makeStubCel();
    stub.setNextResult({
      status: "ok",
      data: {
        url: "https://example.com",
        final_url: "https://www.example.com/",
        redirected: true,
        load_ms: 482,
        dismissed_overlays: false,
        wait_until: "domcontentloaded",
      },
    });
    const out = await handleCelAct(stub.cel, {
      action: "navigate",
      url: "https://example.com",
    });
    const text = out?.content?.[0]?.text ?? "";
    assert(text.includes("https://www.example.com/"), `formatter shows redirected final_url`);
    assert(text.includes("482ms"), `formatter shows load_ms (got ${text.slice(0, 200)})`);
  }

  console.log(`\n=== Results: ${passed} passed, ${failed} failed ===\n`);
  process.exit(failed > 0 ? 1 : 0);
}

main().catch((e) => {
  console.error("Test runner error:", e);
  process.exit(1);
});
