/**
 * Runtime Kernel tests — validates the route→execute→verify pipeline
 * in isolation, without any real adapter or browser.
 */

import { describe, it, expect, vi } from "vitest";
import { executePlannedAction, verifyActionOutcome } from "./kernel.js";
import type { KernelExecutionInput, AdapterCapabilities, KernelEvent } from "./types.js";
import type { ScreenContext, PlannedAction, ContextElement, ElementState } from "../types.js";

// ── Test Helpers ────────────────────────────────────────────────────────────

const defaultState: ElementState = {
  focused: false,
  enabled: true,
  visible: true,
  selected: false,
};

function makeElement(overrides: Partial<ContextElement> & { id: string }): ContextElement {
  return {
    element_type: "button",
    label: "Button",
    actions: ["press"],
    confidence: 1.0,
    source: "accessibility_tree",
    state: { ...defaultState },
    ...overrides,
  } as ContextElement;
}

function makeContext(overrides: Partial<ScreenContext> = {}): ScreenContext {
  return {
    app: "TestApp",
    window: "TestWindow",
    elements: [
      makeElement({ id: "btn1", label: "Submit" }),
      makeElement({ id: "input1", element_type: "textField", label: "Name", value: "", actions: ["set_value"] }),
    ],
    timestamp_ms: Date.now(),
    ...overrides,
  };
}

function makeChangedContext(base: ScreenContext): ScreenContext {
  return {
    ...base,
    elements: [
      ...base.elements,
      makeElement({ id: "new1", element_type: "button", label: "OK", actions: ["press"] }),
    ],
    timestamp_ms: Date.now(),
  };
}

function makeCapabilities(overrides: Partial<AdapterCapabilities> = {}): AdapterCapabilities {
  const baseContext = makeContext();
  const changedContext = makeChangedContext(baseContext);

  return {
    readContext: vi.fn().mockResolvedValue(changedContext),
    executeStructured: vi.fn().mockResolvedValue(true),
    resolveSemantic: vi.fn().mockResolvedValue({ type: "click", target_id: "btn1" } as PlannedAction),
    captureScreenshot: vi.fn().mockResolvedValue(Buffer.from("fake-screenshot")),
    ...overrides,
  };
}

function makeInput(overrides: Partial<KernelExecutionInput> = {}): KernelExecutionInput {
  return {
    action: { type: "click", target_id: "btn1" } as PlannedAction,
    context: makeContext(),
    capabilities: makeCapabilities(),
    readFreshness: () => null,
    ingestOutcome: vi.fn(),
    ...overrides,
  };
}

// ── verifyActionOutcome ─────────────────────────────────────────────────────

describe("verifyActionOutcome", () => {
  it("detects significant diff", () => {
    const before = makeContext();
    const after = makeChangedContext(before);
    const action = { type: "click", target_id: "btn1" } as PlannedAction;

    const result = verifyActionOutcome(action, before, after);

    expect(result.changed).toBe(true);
    expect(result.sideEffectSummary).toBeUndefined();
  });

  it("detects no diff", () => {
    const ctx = makeContext();
    const action = { type: "click", target_id: "btn1" } as PlannedAction;

    const result = verifyActionOutcome(action, ctx, ctx);

    expect(result.changed).toBe(false);
    expect(result.sideEffectSummary).toContain("No significant post-action diff");
  });

  it("detects cross-app shift", () => {
    const before = makeContext({ app: "Chrome", window: "Tab 1" });
    const after: ScreenContext = {
      ...makeChangedContext(before),
      app: "Mail",
      window: "Compose",
    };
    const action = { type: "click", target_id: "btn1" } as PlannedAction;

    const result = verifyActionOutcome(action, before, after);

    expect(result.changed).toBe(true);
    expect(result.crossAppShift).toBe(true);
    expect(result.sideEffectSummary).toContain("shifted context");
    expect(result.sideEffectSummary).toContain("Chrome");
    expect(result.sideEffectSummary).toContain("Mail");
  });

  it("confirms set_value landed", () => {
    const before = makeContext({
      elements: [makeElement({ id: "input1", element_type: "textField", label: "Name", value: "" })],
    });
    const after = makeContext({
      elements: [makeElement({ id: "input1", element_type: "textField", label: "Name", value: "John" })],
    });
    const action = { type: "set_value", target_id: "input1", value: "John" } as PlannedAction;

    const result = verifyActionOutcome(action, before, after);

    expect(result.valueConfirmed).toBe(true);
    expect(result.changed).toBe(true);
  });
});

// ── executePlannedAction ────────────────────────────────────────────────────

describe("executePlannedAction", () => {
  it("structured success — executes and verifies", async () => {
    const input = makeInput();
    const outcome = await executePlannedAction(input);

    expect(outcome.success).toBe(true);
    expect(outcome.verified).toBe(true);
    expect(outcome.route).toBe("structured");
    expect(outcome.terminal).toBe(false);
    expect(outcome.durationMs).toBeGreaterThanOrEqual(0);
    expect(outcome.routeAttempts).toHaveLength(1);
    expect(outcome.routeAttempts[0].route).toBe("structured");
    expect(input.ingestOutcome).toHaveBeenCalled();
  });

  it("structured → semantic escalation", async () => {
    const baseCtx = makeContext();
    const changedCtx = makeChangedContext(baseCtx);
    let readCount = 0;

    const capabilities = makeCapabilities({
      executeStructured: vi.fn()
        .mockResolvedValueOnce(false)  // structured fails
        .mockResolvedValueOnce(true),  // semantic-resolved succeeds
      readContext: vi.fn().mockImplementation(async () => {
        readCount++;
        if (readCount === 1) return baseCtx;
        return changedCtx;
      }),
    });

    const input = makeInput({ capabilities });
    const outcome = await executePlannedAction(input);

    expect(outcome.success).toBe(true);
    expect(outcome.route).toBe("semantic");
    expect(outcome.routeAttempts.length).toBeGreaterThanOrEqual(2);
  });

  it("semantic → vision escalation", async () => {
    const baseCtx = makeContext();
    const changedCtx = makeChangedContext(baseCtx);
    let readCount = 0;

    const capabilities = makeCapabilities({
      executeStructured: vi.fn().mockResolvedValue(false),
      resolveSemantic: vi.fn().mockResolvedValue({ type: "click", target_id: "btn1" } as PlannedAction),
      readContext: vi.fn().mockImplementation(async () => {
        readCount++;
        if (readCount <= 2) return baseCtx;
        return changedCtx;
      }),
    });

    const input = makeInput({ capabilities });
    const outcome = await executePlannedAction(input);

    expect(outcome.success).toBe(true);
    expect(outcome.route).toBe("vision");
    expect(outcome.routeAttempts.some((a) => a.route === "structured")).toBe(true);
    expect(outcome.routeAttempts.some((a) => a.route === "semantic")).toBe(true);
    expect(outcome.routeAttempts.some((a) => a.route === "vision")).toBe(true);
  });

  it("terminal failure after vision ceiling", async () => {
    const baseCtx = makeContext();

    const capabilities = makeCapabilities({
      executeStructured: vi.fn().mockResolvedValue(false),
      resolveSemantic: vi.fn().mockResolvedValue({ type: "click", target_id: "btn1" } as PlannedAction),
      readContext: vi.fn().mockResolvedValue(baseCtx),
    });

    const input = makeInput({ capabilities });
    const outcome = await executePlannedAction(input);

    expect(outcome.success).toBe(false);
    expect(outcome.terminal).toBe(true);
    expect(outcome.route).toBe("terminal_failure");
    expect(outcome.routeAttempts.some((a) => a.route === "vision")).toBe(true);
  });

  it("hard-stale → refresh before execution", async () => {
    const staleCtx = makeContext();
    const freshCtx = makeContext({ timestamp_ms: Date.now() + 5000 });
    const changedCtx = makeChangedContext(freshCtx);
    let readCount = 0;
    let freshnessCallCount = 0;

    const capabilities = makeCapabilities({
      readContext: vi.fn().mockImplementation(async () => {
        readCount++;
        if (readCount === 1) return freshCtx;
        return changedCtx;
      }),
    });

    const input = makeInput({
      capabilities,
      context: staleCtx,
      // Return hard-stale only on first call (before refresh),
      // then null (fresh) after context is refreshed.
      readFreshness: () => {
        freshnessCallCount++;
        if (freshnessCallCount === 1) {
          return {
            state: "hard-stale" as const,
            causes: ["time" as const],
            ageMs: 10000,
            confidence: 0.3,
            lastUpdateMs: Date.now() - 10000,
            lastEventMs: null,
            lastSignificantEventMs: null,
          };
        }
        return null;
      },
    });

    const outcome = await executePlannedAction(input);

    expect(outcome.refreshTriggered).toBe(true);
    expect(outcome.success).toBe(true);
    expect(capabilities.readContext).toHaveBeenCalledTimes(2);
  });

  it("trusted execution — adapter success without diff change", async () => {
    const ctx = makeContext();

    const capabilities = makeCapabilities({
      executeStructured: vi.fn().mockResolvedValue(true),
      readContext: vi.fn().mockResolvedValue(ctx),
    });

    const input = makeInput({ capabilities, context: ctx });
    const outcome = await executePlannedAction(input);

    expect(outcome.success).toBe(true);
    expect(outcome.verified).toBe(true);
    expect(outcome.route).toBe("structured");
    expect(input.ingestOutcome).toHaveBeenCalledTimes(2);
  });

  it("records timing metadata", async () => {
    const input = makeInput();
    const before = Date.now();
    const outcome = await executePlannedAction(input);
    const after = Date.now();

    expect(outcome.timestamp).toBeGreaterThanOrEqual(before);
    expect(outcome.timestamp).toBeLessThanOrEqual(after);
    expect(outcome.durationMs).toBeGreaterThanOrEqual(0);
  });

  it("ingests outcome into cortex on every attempt", async () => {
    const ingestOutcome = vi.fn();
    const input = makeInput({ ingestOutcome });
    await executePlannedAction(input);

    expect(ingestOutcome).toHaveBeenCalled();
    const call = ingestOutcome.mock.calls[0][0];
    expect(call).toHaveProperty("action");
    expect(call).toHaveProperty("route");
    expect(call).toHaveProperty("success");
  });

  it("handles execution error and escalates", async () => {
    const baseCtx = makeContext();
    const changedCtx = makeChangedContext(baseCtx);
    let readCount = 0;

    const capabilities = makeCapabilities({
      executeStructured: vi.fn()
        .mockRejectedValueOnce(new Error("element detached"))
        .mockResolvedValueOnce(true),
      readContext: vi.fn().mockImplementation(async () => {
        readCount++;
        return readCount <= 1 ? baseCtx : changedCtx;
      }),
    });

    const ingestOutcome = vi.fn();
    const input = makeInput({ capabilities, ingestOutcome });
    const outcome = await executePlannedAction(input);

    expect(outcome.success).toBe(true);
    expect(outcome.route).toBe("semantic");
    const firstIngest = ingestOutcome.mock.calls[0][0];
    expect(firstIngest.success).toBe(false);
    expect(firstIngest.sideEffectSummary).toContain("element detached");
  });
});

// ── Kernel Events ──────────────────────────────────────────────────────────

describe("kernel events", () => {
  it("emits route_selected on each attempt", async () => {
    const events: KernelEvent[] = [];
    const input = makeInput({ onEvent: (e) => events.push(e) });
    await executePlannedAction(input);

    const routeEvents = events.filter((e) => e.type === "route_selected");
    expect(routeEvents.length).toBeGreaterThanOrEqual(1);
    expect(routeEvents[0].route).toBe("structured");
    expect(routeEvents[0].action).toBe("click");
    expect(routeEvents[0].timestamp).toBeGreaterThan(0);
  });

  it("emits verification_result after execution", async () => {
    const events: KernelEvent[] = [];
    const input = makeInput({ onEvent: (e) => events.push(e) });
    await executePlannedAction(input);

    const verifyEvents = events.filter((e) => e.type === "verification_result");
    expect(verifyEvents.length).toBeGreaterThanOrEqual(1);
    expect(verifyEvents[0].success).toBeDefined();
    expect(verifyEvents[0].verified).toBeDefined();
  });

  it("emits terminal_failure when ceiling reached", async () => {
    const baseCtx = makeContext();
    const events: KernelEvent[] = [];

    const capabilities = makeCapabilities({
      executeStructured: vi.fn().mockResolvedValue(false),
      resolveSemantic: vi.fn().mockResolvedValue({ type: "click", target_id: "btn1" } as PlannedAction),
      readContext: vi.fn().mockResolvedValue(baseCtx),
    });

    const input = makeInput({ capabilities, onEvent: (e) => events.push(e) });
    await executePlannedAction(input);

    const terminalEvents = events.filter((e) => e.type === "terminal_failure");
    expect(terminalEvents).toHaveLength(1);
    expect(terminalEvents[0].terminal).toBe(true);
    expect(terminalEvents[0].success).toBe(false);
  });

  it("emits refresh_triggered on hard-stale context", async () => {
    const staleCtx = makeContext();
    const freshCtx = makeContext({ timestamp_ms: Date.now() + 5000 });
    const changedCtx = makeChangedContext(freshCtx);
    let readCount = 0;
    let freshnessCallCount = 0;
    const events: KernelEvent[] = [];

    const capabilities = makeCapabilities({
      readContext: vi.fn().mockImplementation(async () => {
        readCount++;
        if (readCount === 1) return freshCtx;
        return changedCtx;
      }),
    });

    const input = makeInput({
      capabilities,
      context: staleCtx,
      onEvent: (e) => events.push(e),
      readFreshness: () => {
        freshnessCallCount++;
        if (freshnessCallCount === 1) {
          return {
            state: "hard-stale" as const,
            causes: ["time" as const],
            ageMs: 10000,
            confidence: 0.3,
            lastUpdateMs: Date.now() - 10000,
            lastEventMs: null,
            lastSignificantEventMs: null,
          };
        }
        return null;
      },
    });

    await executePlannedAction(input);

    const refreshEvents = events.filter((e) => e.type === "refresh_triggered");
    expect(refreshEvents).toHaveLength(1);
    expect(refreshEvents[0].freshnessState).toBe("hard-stale");
    expect(refreshEvents[0].causes).toContain("time");
  });

  it("emits trusted_execution when adapter succeeds without diff", async () => {
    const ctx = makeContext();
    const events: KernelEvent[] = [];

    const capabilities = makeCapabilities({
      executeStructured: vi.fn().mockResolvedValue(true),
      readContext: vi.fn().mockResolvedValue(ctx),
    });

    const input = makeInput({ capabilities, context: ctx, onEvent: (e) => events.push(e) });
    await executePlannedAction(input);

    const trustedEvents = events.filter((e) => e.type === "trusted_execution");
    expect(trustedEvents).toHaveLength(1);
    expect(trustedEvents[0].success).toBe(true);
    expect(trustedEvents[0].verified).toBe(true);
  });

  it("emits execution_result on error", async () => {
    const baseCtx = makeContext();
    const changedCtx = makeChangedContext(baseCtx);
    let readCount = 0;
    const events: KernelEvent[] = [];

    const capabilities = makeCapabilities({
      executeStructured: vi.fn()
        .mockRejectedValueOnce(new Error("target detached"))
        .mockResolvedValueOnce(true),
      readContext: vi.fn().mockImplementation(async () => {
        readCount++;
        return readCount <= 1 ? baseCtx : changedCtx;
      }),
    });

    const input = makeInput({ capabilities, onEvent: (e) => events.push(e) });
    await executePlannedAction(input);

    const errorEvents = events.filter((e) => e.type === "execution_result" && e.success === false);
    expect(errorEvents.length).toBeGreaterThanOrEqual(1);
    expect(errorEvents[0].sideEffectSummary).toContain("target detached");
  });

  it("emits side_effect on cross-app shift", async () => {
    const before = makeContext({ app: "Chrome", window: "Tab 1" });
    const after: ScreenContext = {
      ...makeChangedContext(before),
      app: "Mail",
      window: "Compose",
    };
    const events: KernelEvent[] = [];

    const capabilities = makeCapabilities({
      readContext: vi.fn().mockResolvedValue(after),
    });

    const input = makeInput({ capabilities, context: before, onEvent: (e) => events.push(e) });
    await executePlannedAction(input);

    const sideEffects = events.filter((e) => e.type === "side_effect");
    expect(sideEffects.length).toBeGreaterThanOrEqual(1);
    expect(sideEffects[0].sideEffectSummary).toContain("shifted context");
    expect(sideEffects[0].details?.crossAppShift).toBe(true);
  });

  it("does not throw when onEvent is not provided", async () => {
    const input = makeInput(); // no onEvent
    const outcome = await executePlannedAction(input);
    expect(outcome.success).toBe(true);
  });
});
