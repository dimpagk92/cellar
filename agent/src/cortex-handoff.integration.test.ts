import { afterEach, describe, expect, it } from "vitest";
import { Cortex } from "./cortex.js";
import type { CelEvent, ScreenContext } from "./types.js";

function makeContext(overrides: Partial<ScreenContext> = {}): ScreenContext {
  return {
    app: "Browser",
    window: "Dashboard",
    elements: [],
    timestamp_ms: Date.now(),
    ...overrides,
  };
}

function wait(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

describe("Cortex browser-to-desktop handoff", () => {
  const active: Cortex[] = [];

  afterEach(() => {
    for (const cortex of active.splice(0)) {
      cortex.shutdown();
    }
  });

  it("rebuilds the mental model when the active app shifts from browser to desktop", async () => {
    let currentContext = makeContext();
    let queuedEvents: CelEvent[] = [];
    const deps = {
      getContext: () => currentContext,
      getQuickContext: () => currentContext,
      getContextFocused: () => null,
      captureScreen: () => Buffer.alloc(0),
      listMonitors: () => [],
      listWindows: () => [],
      mousePosition: () => [0, 0] as [number, number],
      buildContextFromElements: () => currentContext,
      makeReference: () => ({ element_type: "button" }),
      resolveReference: () => null,
      axGetMenuBar: () => [],
      axGetAllWindows: () => [],
      startWatchdog: () => {},
      pollEvents: () => {
        const events = queuedEvents;
        queuedEvents = [];
        return events;
      },
      stopWatchdog: () => {},
    };

    const cortex = new Cortex(deps as any, { tickIntervalMs: 20 });
    active.push(cortex);
    await cortex.boot();

    currentContext = makeContext({
      app: "TextEdit",
      window: "Untitled",
      elements: [
        {
          id: "ax:document",
          element_type: "textarea",
          label: "Document",
          state: { focused: true, enabled: true, visible: true, selected: false },
          actions: ["set"],
          confidence: 0.92,
          source: "accessibility_tree",
        },
      ],
    });
    queuedEvents = [{ type: "AppActivated", app_name: "TextEdit" }];

    await wait(60);

    expect(cortex.model.currentContext.app).toBe("TextEdit");
    expect(cortex.model.currentContext.window).toBe("Untitled");
    expect(cortex.model.focusedElement?.id).toBe("ax:document");

    expect(cortex.model.currentContext.elements[0]?.element_type).toBe("textarea");
    expect(cortex.model.recentDiffs.length).toBeGreaterThanOrEqual(0);
    expect(cortex.readFreshness().state).toBe("fresh");
  });
});
