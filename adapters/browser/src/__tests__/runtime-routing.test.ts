import { describe, expect, it, vi } from "vitest";
import { assessActionAmbiguity, buildBrowserCallbacks } from "../callback-builder.js";
import type { ActionOutcome, PlannedAction, ScreenContext } from "@cellar/agent";

function makeContext(overrides: Partial<ScreenContext> = {}): ScreenContext {
  return {
    app: "Browser",
    window: "Main",
    elements: [
      {
        id: "dom:submit",
        element_type: "button",
        label: "Submit",
        state: { focused: false, enabled: true, visible: true, selected: false },
        actions: ["click"],
        confidence: 0.95,
        source: "native_api",
        properties: { css_selector: "#submit" },
      },
    ],
    timestamp_ms: Date.now(),
    ...overrides,
  };
}

function makeFakeAdapter(contexts: ScreenContext[]) {
  let index = 0;
  return {
    getPageUrl: () => "https://example.test",
    getContext: vi.fn(async () => contexts[Math.min(index, contexts.length - 1)]),
    getContextFast: vi.fn(async () => contexts[Math.min(index++, contexts.length - 1)]),
    getContextTier1: vi.fn(async () => contexts[0]),
    getContextTier2: vi.fn(async () => contexts[0]),
    waitForStable: vi.fn(async () => {}),
    dismissCookieConsent: vi.fn(async () => {}),
    screenshot: vi.fn(async () => Buffer.from("png")),
    evaluate: vi.fn(async () => {}),
    executeAction: vi.fn(async () => true),
    resolveSemanticAction: vi.fn(async (_action: PlannedAction) => ({ type: "click", target_id: "dom:submit" as const })),
  };
}

describe("browser runtime routing", () => {
  it("detects a goal-matching near-duplicate target before the first click", () => {
    const ambiguousContext = makeContext({
      elements: [
        {
          id: "dom:james",
          element_type: "button",
          label: "Remove editor James Rodriguez",
          parent_id: "row:james",
          state: { focused: false, enabled: true, visible: true, selected: false },
          actions: ["click"],
          confidence: 0.95,
          source: "native_api",
          properties: { css_selector: "#james" },
        },
        {
          id: "dom:jamie",
          element_type: "button",
          label: "Remove viewer Jamie Rodriguez",
          parent_id: "row:jamie",
          state: { focused: false, enabled: true, visible: true, selected: false },
          actions: ["click"],
          confidence: 0.95,
          source: "native_api",
          properties: { css_selector: "#jamie" },
        },
      ],
    });

    const result = assessActionAmbiguity(
      { type: "click", target_id: "dom:james" },
      ambiguousContext,
      `Remove the user "Jamie Rodriguez" who is a viewer with email "jamie.rodriguez@acme.io".`,
    );

    expect(result?.ambiguous).toBe(true);
    expect(result?.preferredTargetId).toBe("dom:jamie");
  });

  it("uses row context and avoids substring surname matches like Rodriguez-Smith", () => {
    const ambiguousContext = makeContext({
      elements: [
        {
          id: "row:jamie",
          element_type: "table_row",
          label: "Jamie Rodriguez jamie.rodriguez@acme.io viewer 5 days ago Remove",
          bounds: { x: 0, y: 100, width: 1000, height: 40 },
          state: { focused: false, enabled: true, visible: true, selected: false },
          actions: [],
          confidence: 0.9,
          source: "native_api",
        },
        {
          id: "dom:jamie",
          element_type: "button",
          label: "Remove",
          bounds: { x: 900, y: 108, width: 76, height: 24 },
          state: { focused: false, enabled: true, visible: true, selected: false },
          actions: ["click"],
          confidence: 0.95,
          source: "native_api",
          properties: { css_selector: "#jamie" },
        },
        {
          id: "row:smith",
          element_type: "table_row",
          label: "Jamie Rodriguez-Smith jamie.rs@acme.io editor 4 hours ago Remove",
          bounds: { x: 0, y: 150, width: 1000, height: 40 },
          state: { focused: false, enabled: true, visible: true, selected: false },
          actions: [],
          confidence: 0.9,
          source: "native_api",
        },
        {
          id: "dom:smith",
          element_type: "button",
          label: "Remove",
          bounds: { x: 900, y: 158, width: 76, height: 24 },
          state: { focused: false, enabled: true, visible: true, selected: false },
          actions: ["click"],
          confidence: 0.95,
          source: "native_api",
          properties: { css_selector: "#smith" },
        },
      ],
    });

    const result = assessActionAmbiguity(
      { type: "click", target_id: "dom:smith" },
      ambiguousContext,
      `Remove the user "Jamie Rodriguez" who is a viewer with email "jamie.rodriguez@acme.io".`,
    );

    expect(result?.ambiguous).toBe(true);
    expect(result?.preferredTargetId).toBe("dom:jamie");
  });

  it("uses semantic routing immediately when the goal clearly matches a different duplicate target", async () => {
    const ambiguousContext = makeContext({
      elements: [
        {
          id: "dom:james",
          element_type: "button",
          label: "Remove editor James Rodriguez",
          parent_id: "row:james",
          state: { focused: false, enabled: true, visible: true, selected: false },
          actions: ["click"],
          confidence: 0.95,
          source: "native_api",
          properties: { css_selector: "#james" },
        },
        {
          id: "dom:jamie",
          element_type: "button",
          label: "Remove viewer Jamie Rodriguez",
          parent_id: "row:jamie",
          state: { focused: false, enabled: true, visible: true, selected: false },
          actions: ["click"],
          confidence: 0.95,
          source: "native_api",
          properties: { css_selector: "#jamie" },
        },
      ],
    });
    const adapter = makeFakeAdapter([
      ambiguousContext,
      makeContext({
        elements: [
          {
            id: "result",
            element_type: "text",
            label: "Correct!",
            state: { focused: false, enabled: true, visible: true, selected: false },
            actions: [],
            confidence: 0.9,
            source: "native_api",
          },
        ],
      }),
    ]);
    const outcomes: ActionOutcome[] = [];
    const callbacks = buildBrowserCallbacks({
      adapter: adapter as any,
      cel: {} as any,
      goal: `Remove the user "Jamie Rodriguez" who is a viewer with email "jamie.rodriguez@acme.io".`,
      cortex: {
        model: { freshness: { state: "fresh", causes: [], ageMs: 0, confidence: 1, lastUpdateMs: Date.now(), lastEventMs: null, lastSignificantEventMs: null } },
        ingestActionOutcome: (outcome) => outcomes.push(outcome),
      },
    });

    const success = await callbacks.executeAction?.({ type: "click", target_id: "dom:james" }, ambiguousContext);

    expect(success).toBe(true);
    expect(adapter.executeAction).toHaveBeenCalledWith("click", expect.objectContaining({ css_selector: "#jamie" }));
    expect(adapter.resolveSemanticAction).not.toHaveBeenCalled();
    expect(outcomes[0]?.route).toBe("semantic");
  });

  it("escalates to terminal failure after vision cannot verify the action", async () => {
    const initial = makeContext({
      elements: [
        {
          id: "dom:submit",
          element_type: "button",
          label: "Submit",
          state: { focused: false, enabled: true, visible: true, selected: false },
          actions: ["click"],
          confidence: 0.95,
          source: "native_api",
          properties: {},
        },
      ],
    });
    const adapter = makeFakeAdapter([initial, initial, initial, initial]);
    adapter.executeAction = vi.fn(async () => false);
    const outcomes: ActionOutcome[] = [];
    const callbacks = buildBrowserCallbacks({
      adapter: adapter as any,
      cel: {} as any,
      cortex: {
        model: { freshness: { state: "fresh", causes: [], ageMs: 0, confidence: 1, lastUpdateMs: Date.now(), lastEventMs: null, lastSignificantEventMs: null } },
        ingestActionOutcome: (outcome) => outcomes.push(outcome),
      },
    });

    const success = await callbacks.executeAction?.({ type: "click", target_id: "dom:submit" }, initial);

    expect(success).toBe(false);
    expect(adapter.resolveSemanticAction).toHaveBeenCalledTimes(1);
    expect(adapter.screenshot).toHaveBeenCalledTimes(1);
    expect(outcomes.map((o) => o.route)).toEqual(["structured", "semantic", "vision", "terminal_failure"]);
    expect(outcomes.at(-1)?.sideEffectSummary).toContain("Escalation ceiling reached");
    expect(outcomes.some((o) => (o.sideEffectSummary ?? "").includes("No significant post-action diff"))).toBe(true);
  });

  it("records a side-effect summary when a browser action jumps into a desktop app", async () => {
    const browserContext = makeContext();
    const desktopContext = makeContext({
      app: "TextEdit",
      window: "Untitled",
      elements: [
        {
          id: "ax:text",
          element_type: "textarea",
          label: "Document",
          state: { focused: true, enabled: true, visible: true, selected: false },
          actions: ["set"],
          confidence: 0.9,
          source: "accessibility_tree",
        },
      ],
    });
    const adapter = makeFakeAdapter([desktopContext]);
    const outcomes: ActionOutcome[] = [];
    const callbacks = buildBrowserCallbacks({
      adapter: adapter as any,
      cel: {} as any,
      cortex: {
        model: { freshness: { state: "fresh", causes: [], ageMs: 0, confidence: 1, lastUpdateMs: Date.now(), lastEventMs: null, lastSignificantEventMs: null } },
        ingestActionOutcome: (outcome) => outcomes.push(outcome),
      },
    });

    const success = await callbacks.executeAction?.({ type: "click", target_id: "dom:submit" }, browserContext);

    expect(success).toBe(true);
    expect(outcomes[0]?.verified).toBe(true);
    expect(outcomes[0]?.sideEffectSummary).toContain("shifted context from Browser/Main to TextEdit/Untitled");
  });
});
