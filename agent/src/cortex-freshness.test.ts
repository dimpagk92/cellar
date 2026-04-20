import { describe, expect, it } from "vitest";
import { Cortex } from "./cortex.js";
import type { CelEvent, ScreenContext } from "./types.js";

function makeContext(overrides?: Partial<ScreenContext>): ScreenContext {
  return {
    app: "Browser",
    window: "Test",
    elements: [],
    timestamp_ms: Date.now(),
    ...overrides,
  };
}

function makeDeps(events: CelEvent[] = []) {
  let queue = [...events];
  return {
    getContext: () => makeContext(),
    getQuickContext: () => makeContext(),
    getContextFocused: () => null,
    captureScreen: () => Buffer.from(""),
    listMonitors: () => [],
    listWindows: () => [],
    mousePosition: () => [0, 0] as [number, number],
    buildContextFromElements: () => makeContext(),
    makeReference: () => ({ element_type: "button" }),
    resolveReference: () => null,
    axGetMenuBar: () => [],
    axGetAllWindows: () => [],
    startWatchdog: () => {},
    pollEvents: () => {
      const result = queue;
      queue = [];
      return result;
    },
    stopWatchdog: () => {},
  };
}

describe("Cortex freshness", () => {
  it("starts fresh after boot", async () => {
    const cortex = new Cortex(makeDeps(), { tickIntervalMs: 1000 });
    await cortex.boot();
    expect(cortex.readFreshness().state).toBe("fresh");
    cortex.shutdown();
  });

  it("becomes soft-stale based on age threshold", async () => {
    const cortex = new Cortex(makeDeps(), { softStaleMs: 1, hardStaleMs: 1000 });
    await cortex.boot();
    await new Promise((resolve) => setTimeout(resolve, 5));
    expect(cortex.readFreshness().state).toBe("soft-stale");
    cortex.shutdown();
  });

  it("marks contradiction as hard-stale after a failed verified outcome", async () => {
    const cortex = new Cortex(makeDeps(), { softStaleMs: 1000, hardStaleMs: 5000 });
    await cortex.boot();
    cortex.ingestActionOutcome({
      action: "click",
      success: true,
      verified: false,
      contradiction: true,
    });
    const freshness = cortex.readFreshness();
    expect(freshness.state).toBe("hard-stale");
    expect(freshness.causes).toContain("event");
    cortex.shutdown();
  });
});
