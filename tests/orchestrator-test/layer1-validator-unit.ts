/**
 * Layer 1: Validator Unit Tests
 *
 * Tests validateAction() with mock contexts simulating real BrowserGym/MiniWoB scenarios.
 * No LLM, no browser — pure function tests.
 *
 * Usage: npx tsx tests/orchestrator-test/layer1-validator-unit.ts
 */

import { validateAction, type ValidateActionParams } from "../../agent/src/goal-runner/validator.js";
import type { ScreenContext, ContextElement, PlannedStep, PlannedAction } from "../../agent/src/types.js";

// ── Helpers ──────────────────────────────────────────────────────────────────

function makeElement(id: string, type: string, label?: string, opts: Partial<ContextElement> = {}): ContextElement {
  return {
    id,
    element_type: type,
    label,
    state: { focused: false, enabled: true, visible: true, selected: false },
    confidence: 0.9,
    source: "merged",
    actions: ["click"],
    ...opts,
  };
}

function makeContext(elements: ContextElement[], extra: Partial<ScreenContext> = {}): ScreenContext {
  return {
    app: "Chromium",
    window: "Test Page",
    elements,
    timestamp_ms: Date.now(),
    ...extra,
  };
}

function makeStep(action: PlannedAction, confidence = 0.8): PlannedStep {
  return {
    reasoning: "test",
    action,
    expected_outcome: "test outcome",
    confidence,
  };
}

// ── Test runner ──────────────────────────────────────────────────────────────

let passed = 0;
let failed = 0;

async function test(name: string, fn: () => Promise<void>) {
  try {
    await fn();
    passed++;
    console.log(`  ✅ ${name}`);
  } catch (e) {
    failed++;
    console.log(`  ❌ ${name}: ${String(e).slice(0, 120)}`);
  }
}

function assert(condition: boolean, msg: string) {
  if (!condition) throw new Error(msg);
}

// ── BrowserGym-realistic contexts ────────────────────────────────────────────

// MiniWoB click-test: single button page
const CLICK_TEST_PRE = makeContext([
  makeElement("btn:1", "button", "Click Me!", { actions: ["click"] }),
]);
const CLICK_TEST_POST_SUCCESS = makeContext([
  makeElement("msg:1", "text", "Success! You clicked the button.", { actions: [] }),
]);
const CLICK_TEST_POST_UNCHANGED = makeContext([
  makeElement("btn:1", "button", "Click Me!", { actions: ["click"] }),
]);

// MiniWoB enter-text: text field + submit
const ENTER_TEXT_PRE = makeContext([
  makeElement("input:1", "textfield", "Text input", { actions: ["click"] }),
  makeElement("btn:1", "button", "Submit", { actions: ["click"] }),
]);
const ENTER_TEXT_POST_TYPED = makeContext([
  makeElement("input:1", "textfield", "Text input", { value: "hello world", state: { focused: true, enabled: true, visible: true, selected: false }, actions: ["click"] }),
  makeElement("btn:1", "button", "Submit", { actions: ["click"] }),
]);
const ENTER_TEXT_POST_SUBMITTED = makeContext([
  makeElement("msg:1", "text", "Submitted successfully", { actions: [] }),
]);

// MiniWoB login-user: username + password + login
const LOGIN_PRE = makeContext([
  makeElement("input:user", "textfield", "Username"),
  makeElement("input:pass", "textfield", "Password"),
  makeElement("btn:login", "button", "Login"),
]);
const LOGIN_POST_FILLED = makeContext([
  makeElement("input:user", "textfield", "Username", { value: "testuser" }),
  makeElement("input:pass", "textfield", "Password", { value: "pass123" }),
  makeElement("btn:login", "button", "Login"),
]);

// MiniWoB click-dialog: dialog with button
const DIALOG_PRE = makeContext([
  makeElement("dialog:1", "dialog", "Alert"),
  makeElement("btn:ok", "button", "OK", { actions: ["click"] }),
]);
const DIALOG_POST_DISMISSED = makeContext([
  makeElement("main:1", "text", "Main content"),
]);

// Error page scenario
const ERROR_PAGE = makeContext([
  makeElement("error:1", "text", "Error: Something went wrong", { state: { focused: false, enabled: true, visible: true, selected: false } }),
  makeElement("btn:retry", "button", "Retry"),
]);

// ── Layer 1 Tests ────────────────────────────────────────────────────────────

async function main() {
  console.log("=== Layer 1: Validator Unit Tests ===\n");

  // ── Fast paths ─────────────────────────────────────────────────────────

  console.log("Fast paths:");

  await test("Execution error → failure", async () => {
    const result = await validateAction({
      goal: "Click the button",
      step: makeStep({ type: "click", target_id: "btn:1" }),
      preContext: CLICK_TEST_PRE,
      postContext: CLICK_TEST_PRE,
      executionError: "Element not found in DOM",
    });
    assert(result.verdict === "failure", `Expected failure, got ${result.verdict}`);
    assert(result.confidence >= 0.9, `Low confidence: ${result.confidence}`);
    assert(result.stateChanged === false, "State should not change on error");
    assert(result.reasoning.includes("Execution error"), `Reasoning: ${result.reasoning}`);
  });

  await test("Planner fallback (confidence 0.3 + extract) → failure", async () => {
    const result = await validateAction({
      goal: "Do something",
      step: makeStep({ type: "extract", goal: "Extract visible data", data: "" }, 0.3),
      preContext: CLICK_TEST_PRE,
      postContext: CLICK_TEST_PRE,
    });
    assert(result.verdict === "failure", `Expected failure, got ${result.verdict}`);
    assert(result.suggestedRecovery === "planning_failed", `Recovery: ${result.suggestedRecovery}`);
  });

  await test("Normal extract (confidence 0.7) → success", async () => {
    const result = await validateAction({
      goal: "Extract data",
      step: makeStep({ type: "extract", goal: "Get table data", data: "some data" }, 0.7),
      preContext: ENTER_TEXT_PRE,
      postContext: ENTER_TEXT_PRE,
    });
    assert(result.verdict === "success", `Expected success, got ${result.verdict}`);
  });

  // ── Done/Fail actions ──────────────────────────────────────────────────

  console.log("\nDone/Fail actions:");

  await test("Done with no errors → success", async () => {
    const result = await validateAction({
      goal: "Click the button",
      step: makeStep({ type: "done", summary: "Clicked the button" }),
      preContext: CLICK_TEST_PRE,
      postContext: CLICK_TEST_POST_SUCCESS,
    });
    assert(result.verdict === "success", `Expected success, got ${result.verdict}`);
    assert(result.confidence >= 0.9, `Low confidence: ${result.confidence}`);
  });

  await test("Done with error element visible → failure", async () => {
    const result = await validateAction({
      goal: "Submit the form",
      step: makeStep({ type: "done", summary: "Form submitted" }),
      preContext: ENTER_TEXT_PRE,
      postContext: ERROR_PAGE,
    });
    assert(result.verdict === "failure", `Expected failure, got ${result.verdict}`);
    assert(result.reasoning.includes("Done claim rejected"), `Reasoning: ${result.reasoning}`);
  });

  await test("Fail action → failure with agent_gave_up", async () => {
    const result = await validateAction({
      goal: "Do something impossible",
      step: makeStep({ type: "fail", reason: "Cannot find element" }),
      preContext: CLICK_TEST_PRE,
      postContext: CLICK_TEST_PRE,
    });
    assert(result.verdict === "failure", `Expected failure, got ${result.verdict}`);
    assert(result.suggestedRecovery === "agent_gave_up", `Recovery: ${result.suggestedRecovery}`);
  });

  // ── Tier 1: Fingerprint heuristics ─────────────────────────────────────

  console.log("\nTier 1 (Fingerprints):");

  await test("Click with state change (fingerprints differ) → success", async () => {
    const result = await validateAction({
      goal: "Click the button",
      step: makeStep({ type: "click", target_id: "btn:1" }),
      preContext: CLICK_TEST_PRE,
      postContext: CLICK_TEST_POST_SUCCESS,
      preFingerprint: "page-state-1",
      postFingerprint: "page-state-2",
    });
    assert(result.verdict === "success", `Expected success, got ${result.verdict}`);
    assert(result.stateChanged === true, "State should have changed");
    assert(!result.plannerHint, "No hint needed for success");
  });

  await test("Click with NO state change (fingerprints same) → failure", async () => {
    const result = await validateAction({
      goal: "Click the button",
      step: makeStep({ type: "click", target_id: "btn:1" }),
      preContext: CLICK_TEST_PRE,
      postContext: CLICK_TEST_POST_UNCHANGED,
      preFingerprint: "page-state-1",
      postFingerprint: "page-state-1",
    });
    assert(result.verdict === "failure", `Expected failure, got ${result.verdict}`);
    assert(result.stateChanged === false, "State should not have changed");
    assert(result.suggestedRecovery === "retry_different_approach", `Recovery: ${result.suggestedRecovery}`);
  });

  await test("Click with no fingerprints (uncertain) → falls through to Tier 2", async () => {
    // With significant diff, should resolve to success
    const result = await validateAction({
      goal: "Click the dialog button",
      step: makeStep({ type: "click", target_id: "btn:ok" }),
      preContext: DIALOG_PRE,
      postContext: DIALOG_POST_DISMISSED,
      // No fingerprints
    });
    // Tier 1 stays uncertain, Tier 2 sees diff (added main:1, removed dialog:1, btn:ok)
    assert(result.verdict === "success", `Expected success from diff, got ${result.verdict}`);
    assert(result.stateChanged === true, "Diff shows state changed");
  });

  // ── Non-transition actions ─────────────────────────────────────────────

  console.log("\nNon-transition actions:");

  await test("Wait action → always success", async () => {
    const result = await validateAction({
      goal: "Wait for page",
      step: makeStep({ type: "wait", ms: 1000 }),
      preContext: CLICK_TEST_PRE,
      postContext: CLICK_TEST_PRE,
      preFingerprint: "same",
      postFingerprint: "same",
    });
    assert(result.verdict === "success", `Expected success, got ${result.verdict}`);
  });

  await test("Scroll action → always success", async () => {
    const result = await validateAction({
      goal: "Scroll down",
      step: makeStep({ type: "scroll", dx: 0, dy: -3 }),
      preContext: CLICK_TEST_PRE,
      postContext: CLICK_TEST_PRE,
      preFingerprint: "same",
      postFingerprint: "same",
    });
    assert(result.verdict === "success", `Expected success, got ${result.verdict}`);
  });

  // ── Tier 2: Context diff ───────────────────────────────────────────────

  console.log("\nTier 2 (Context Diff):");

  await test("Type action: value changed in context → success", async () => {
    const result = await validateAction({
      goal: "Type hello world",
      step: makeStep({ type: "type", target_id: "input:1", text: "hello world" }),
      preContext: ENTER_TEXT_PRE,
      postContext: ENTER_TEXT_POST_TYPED,
      // No fingerprints — forces diff tier
    });
    // input:1 changed (value added, state changed to focused) → diff significant
    assert(result.verdict === "success", `Expected success, got ${result.verdict}`);
  });

  await test("Click with no diff and no fingerprints → failure", async () => {
    const result = await validateAction({
      goal: "Click the button",
      step: makeStep({ type: "click", target_id: "btn:1" }),
      preContext: CLICK_TEST_PRE,
      postContext: CLICK_TEST_PRE, // identical
      // No fingerprints
    });
    // Tier 1: uncertain (no fingerprints), Tier 2: no diff → failure
    assert(result.verdict === "failure", `Expected failure, got ${result.verdict}`);
    assert(result.suggestedRecovery === "retry_different_approach", `Recovery: ${result.suggestedRecovery}`);
  });

  await test("Click that opens dialog (elements added) → success", async () => {
    const preCtx = makeContext([
      makeElement("btn:settings", "button", "Settings"),
    ]);
    const postCtx = makeContext([
      makeElement("btn:settings", "button", "Settings"),
      makeElement("dialog:1", "dialog", "Settings Dialog"),
      makeElement("toggle:dark", "checkbox", "Dark Mode", { actions: ["click"] }),
      makeElement("btn:close", "button", "Close", { actions: ["click"] }),
    ]);
    const result = await validateAction({
      goal: "Open settings",
      step: makeStep({ type: "click", target_id: "btn:settings" }),
      preContext: preCtx,
      postContext: postCtx,
      // No fingerprints — diff tier kicks in
    });
    assert(result.verdict === "success", `Expected success, got ${result.verdict}`);
    assert(result.stateChanged === true, "New elements appeared");
    assert(result.confidence >= 0.85, `High confidence expected, got ${result.confidence}`);
  });

  // ── Failure metadata ─────────────────────────────────────────────────

  console.log("\nFailure metadata:");

  await test("Failed click has suggestedRecovery and reasoning", async () => {
    const result = await validateAction({
      goal: "Click submit",
      step: makeStep({ type: "click", target_id: "btn:1" }),
      preContext: CLICK_TEST_PRE,
      postContext: CLICK_TEST_PRE,
      preFingerprint: "same",
      postFingerprint: "same",
    });
    assert(result.verdict === "failure", `Expected failure, got ${result.verdict}`);
    assert(result.suggestedRecovery === "retry_different_approach", `Recovery: ${result.suggestedRecovery}`);
    assert(result.reasoning.length > 0, "Should have reasoning");
  });

  await test("Successful action has no suggestedRecovery", async () => {
    const result = await validateAction({
      goal: "Click button",
      step: makeStep({ type: "click", target_id: "btn:1" }),
      preContext: CLICK_TEST_PRE,
      postContext: CLICK_TEST_POST_SUCCESS,
      preFingerprint: "state-1",
      postFingerprint: "state-2",
    });
    assert(result.suggestedRecovery === undefined, `Should have no recovery, got: ${result.suggestedRecovery}`);
  });

  // ── MiniWoB-specific scenarios ─────────────────────────────────────────

  console.log("\nMiniWoB scenarios:");

  await test("login-user: type into both fields → success (values changed)", async () => {
    const result = await validateAction({
      goal: "Enter username and password",
      step: makeStep({ type: "type", target_id: "input:user", text: "testuser" }),
      preContext: LOGIN_PRE,
      postContext: LOGIN_POST_FILLED,
    });
    assert(result.verdict === "success", `Expected success, got ${result.verdict}`);
  });

  await test("click-dialog: dismiss dialog → success (elements replaced)", async () => {
    const result = await validateAction({
      goal: "Close the dialog",
      step: makeStep({ type: "click", target_id: "btn:ok" }),
      preContext: DIALOG_PRE,
      postContext: DIALOG_POST_DISMISSED,
      preFingerprint: "dialog-open",
      postFingerprint: "dialog-closed",
    });
    assert(result.verdict === "success", `Expected success, got ${result.verdict}`);
    assert(result.stateChanged === true, "Dialog should be gone");
  });

  await test("enter-text: type + submit → success after submit changes page", async () => {
    const result = await validateAction({
      goal: "Submit the text",
      step: makeStep({ type: "click", target_id: "btn:1" }),
      preContext: ENTER_TEXT_POST_TYPED,
      postContext: ENTER_TEXT_POST_SUBMITTED,
      preFingerprint: "form-filled",
      postFingerprint: "form-submitted",
    });
    assert(result.verdict === "success", `Expected success, got ${result.verdict}`);
  });

  await test("click-checkboxes: check a box → state changes (checked)", async () => {
    const preCtx = makeContext([
      makeElement("cb:a", "checkbox", "Option A", { state: { focused: false, enabled: true, visible: true, selected: false, checked: false } }),
      makeElement("cb:b", "checkbox", "Option B", { state: { focused: false, enabled: true, visible: true, selected: false, checked: false } }),
      makeElement("btn:submit", "button", "Submit"),
    ]);
    const postCtx = makeContext([
      makeElement("cb:a", "checkbox", "Option A", { state: { focused: false, enabled: true, visible: true, selected: false, checked: true } }),
      makeElement("cb:b", "checkbox", "Option B", { state: { focused: false, enabled: true, visible: true, selected: false, checked: false } }),
      makeElement("btn:submit", "button", "Submit"),
    ]);
    const result = await validateAction({
      goal: "Check Option A",
      step: makeStep({ type: "click", target_id: "cb:a" }),
      preContext: preCtx,
      postContext: postCtx,
    });
    // Tier 2: checked changed → significant diff
    assert(result.verdict === "success", `Expected success, got ${result.verdict}`);
  });

  // ── Summary ────────────────────────────────────────────────────────────

  console.log(`\n=== ${passed} passed, ${failed} failed ===`);
  if (failed === 0) {
    console.log("✅ All validator unit tests pass.");
  } else {
    console.log("❌ Some tests failed.");
    process.exit(1);
  }
}

main().catch((e) => {
  console.error("Fatal:", e);
  process.exit(1);
});
