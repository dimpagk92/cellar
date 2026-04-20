#!/usr/bin/env node
/**
 * Smoke test: Verify the Rust Cortex works through NAPI bindings.
 *
 * PREREQUISITES:
 * 1. macOS accessibility permissions must be granted to the terminal
 *    (System Settings → Privacy & Security → Accessibility → enable your terminal)
 * 2. Native module must be rebuilt: cd cel/cel-napi && npx napi build --release
 *    Then: cp cel-napi.node cel-napi.darwin-arm64.node
 *
 * Run: node tests/cortex/napi-smoke.mjs
 *   or: make test-cortex-napi
 */

import { createRequire } from "node:module";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
const require = createRequire(import.meta.url);

const __dirname = dirname(fileURLToPath(import.meta.url));
const projectRoot = resolve(__dirname, "../..");
const native = require(process.env.CELLAR_NAPI_PATH || resolve(projectRoot, "cel/cel-napi/index.js"));

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

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

async function main() {
  console.log("\n=== Rust Cortex NAPI Smoke Test ===\n");

  // Verify the cortex functions are exported
  console.log("0. Verify NAPI exports");
  const cortexFuncs = [
    "bootCortex", "isCortexRunning", "readCortexModel", "stopCortex",
    "notifyCortexAction", "consumeCortexAnomalies",
    "reportCortexActionFailure", "reportCortexActionSuccess",
  ];
  for (const fn of cortexFuncs) {
    assert(typeof native[fn] === "function", `${fn} is exported`);
  }

  // 1. Should not be running initially
  console.log("\n1. Initial state");
  assert(!native.isCortexRunning(), "Cortex is not running initially");

  // 2. Boot the Cortex
  console.log("\n2. Boot Cortex");
  try {
    native.bootCortex();
    assert(true, "bootCortex() succeeded");
  } catch (e) {
    assert(false, `bootCortex() failed: ${e.message}`);
    console.log("\n=== Aborting — Cortex failed to boot ===\n");
    process.exit(1);
  }

  assert(native.isCortexRunning(), "Cortex is running after boot");

  // 3. Wait for perception ticks
  console.log("\n3. Wait for perception ticks (1000ms)...");
  await sleep(1000);

  // 4. Read the mental model
  console.log("\n4. Read mental model");
  let model;
  try {
    const json = native.readCortexModel();
    model = JSON.parse(json);
    assert(true, "readCortexModel() returned valid JSON");
  } catch (e) {
    assert(false, `readCortexModel() failed: ${e.message}`);
  }

  if (model) {
    assert(typeof model.current_context === "object", "Model has current_context");
    assert(typeof model.current_context.app === "string", `App: "${model.current_context.app}"`);
    assert(typeof model.current_context.window === "string", `Window: "${model.current_context.window}"`);
    assert(Array.isArray(model.current_context.elements), `Elements: ${model.current_context.elements.length}`);
    assert(model.confidence === 1.0, `Confidence: ${model.confidence}`);
    assert(model.cycle_count > 0, `Cycle count: ${model.cycle_count}`);
    assert(model.uptime_ms > 0, `Uptime: ${model.uptime_ms}ms`);
    assert(typeof model.temporal === "object", "Has temporal flags");
    assert(typeof model.stability === "object", "Has stability classification");

    const stableCount = Array.isArray(model.stability?.stable) ? model.stability.stable.length : 0;
    const volatileCount = Array.isArray(model.stability?.volatile) ? model.stability.volatile.length : 0;
    console.log(`\n   Stability: ${stableCount} stable, ${volatileCount} volatile`);
    console.log(`   Temporal: stagnant=${model.temporal?.stagnant_cycles}, idle_since=${model.temporal?.idle_since}`);
  }

  // 5. Notify action
  console.log("\n5. Notify action");
  try {
    native.notifyCortexAction("test click on button");
    assert(true, "notifyCortexAction() succeeded");
  } catch (e) {
    assert(false, `notifyCortexAction() failed: ${e.message}`);
  }

  // 6. Consume anomalies
  console.log("\n6. Consume anomalies");
  try {
    const anomaliesJson = native.consumeCortexAnomalies();
    const anomalies = JSON.parse(anomaliesJson);
    assert(Array.isArray(anomalies), `Anomalies: ${anomalies.length} consumed`);
  } catch (e) {
    assert(false, `consumeCortexAnomalies() failed: ${e.message}`);
  }

  // 7. Report action success/failure
  console.log("\n7. Action reporting");
  try {
    native.reportCortexActionSuccess();
    assert(true, "reportCortexActionSuccess() ok");
    native.reportCortexActionFailure();
    assert(true, "reportCortexActionFailure() ok");
  } catch (e) {
    assert(false, `Action reporting failed: ${e.message}`);
  }

  // 8. Double boot should fail
  console.log("\n8. Double boot guard");
  try {
    native.bootCortex();
    assert(false, "Double boot should have thrown");
  } catch (e) {
    assert(true, `Double boot correctly rejected: ${e.message}`);
  }

  // 9. Stop the Cortex
  console.log("\n9. Stop Cortex");
  try {
    native.stopCortex();
    assert(true, "stopCortex() succeeded");
  } catch (e) {
    assert(false, `stopCortex() failed: ${e.message}`);
  }

  await sleep(300);
  assert(!native.isCortexRunning(), "Cortex is not running after stop");

  // Summary
  console.log(`\n=== Results: ${passed} passed, ${failed} failed ===\n`);
  process.exit(failed > 0 ? 1 : 0);
}

main().catch((e) => {
  console.error("Fatal error:", e);
  process.exit(1);
});
