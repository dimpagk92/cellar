import { describe, it, expect, vi, beforeEach } from "vitest";
import { CortexBridge } from "./goal-runner/cortex-bridge.js";
import type { MentalModel, Anomaly, ScreenContext, ElementStability } from "./types.js";

// Mock Cortex with controllable mental model
function mockCortex(model: Partial<MentalModel> = {}) {
  const defaultModel: MentalModel = {
    currentContext: {
      app: "Browser", window: "Test", elements: [], timestamp_ms: Date.now(),
    } as ScreenContext,
    focusedElement: null,
    recentDiffs: [],
    temporal: {
      loading: null,
      errorPersisting: null,
      idleSince: Date.now() - 5000,
      focusTrail: [],
      stagnantCycles: 5,
    },
    stability: { stable: new Set(), volatile: new Set() },
    anomalyQueue: [],
    confidence: 1.0,
    visionNeeded: false,
    ageMs: 0,
    cycleCount: 10,
    uptimeMs: 10000,
    ...model,
  };

  return {
    isRunning: vi.fn().mockReturnValue(true),
    model: defaultModel,
    consumeAnomalies: vi.fn().mockImplementation(() => {
      const anomalies = [...defaultModel.anomalyQueue];
      defaultModel.anomalyQueue = [];
      return anomalies;
    }),
    notifyAction: vi.fn(),
    shutdown: vi.fn(),
  } as any;
}

function mockCel() {
  return {
    click: vi.fn(),
  } as any;
}

describe("CortexBridge", () => {
  describe("poll", () => {
    it("should return empty when cortex not running", () => {
      const cortex = mockCortex();
      cortex.isRunning.mockReturnValue(false);
      const bridge = new CortexBridge(cortex, mockCel());

      const signals = bridge.poll();
      expect(signals).toEqual([]);
    });

    it("should convert anomalies to signals", () => {
      const cortex = mockCortex({
        anomalyQueue: [
          { type: "dialog", description: "Cookie consent", timestamp: Date.now() } as Anomaly,
        ],
      });
      const bridge = new CortexBridge(cortex, mockCel());

      const signals = bridge.poll();
      expect(signals.length).toBeGreaterThanOrEqual(1);
      const dialogSignal = signals.find(s => s.type === "dialog");
      expect(dialogSignal).toBeDefined();
      expect(dialogSignal!.description).toBe("Cookie consent");
      expect(dialogSignal!.actionRequired).toBe(true);
    });

    it("should detect loading stalls", () => {
      const cortex = mockCortex({
        temporal: {
          loading: { detected: true, durationMs: 6000 },
          errorPersisting: null,
          idleSince: null,
          focusTrail: [],
          stagnantCycles: 0,
        },
      });
      const bridge = new CortexBridge(cortex, mockCel());

      const signals = bridge.poll();
      const loadingSignal = signals.find(s => s.type === "loading_stall");
      expect(loadingSignal).toBeDefined();
      expect(loadingSignal!.actionRequired).toBe(false);
    });

    it("should detect persistent errors", () => {
      const cortex = mockCortex({
        temporal: {
          loading: null,
          errorPersisting: { detected: true, durationMs: 4000, message: "404 Not Found" },
          idleSince: null,
          focusTrail: [],
          stagnantCycles: 0,
        },
      });
      const bridge = new CortexBridge(cortex, mockCel());

      const signals = bridge.poll();
      const errorSignal = signals.find(s => s.type === "error_persisting");
      expect(errorSignal).toBeDefined();
      expect(errorSignal!.actionRequired).toBe(true);
      expect(errorSignal!.description).toContain("404 Not Found");
    });

    it("should detect idle state", () => {
      const cortex = mockCortex({
        temporal: {
          loading: null,
          errorPersisting: null,
          idleSince: Date.now() - 3000,
          focusTrail: [],
          stagnantCycles: 5,
        },
      });
      const bridge = new CortexBridge(cortex, mockCel());

      const signals = bridge.poll();
      const idleSignal = signals.find(s => s.type === "idle");
      expect(idleSignal).toBeDefined();
    });
  });

  describe("isSettled", () => {
    it("should return true when idle and not loading", () => {
      const cortex = mockCortex({
        temporal: {
          loading: null,
          errorPersisting: null,
          idleSince: Date.now() - 2000,
          focusTrail: [],
          stagnantCycles: 3,
        },
      });
      const bridge = new CortexBridge(cortex, mockCel());
      expect(bridge.isSettled()).toBe(true);
    });

    it("should return false when loading", () => {
      const cortex = mockCortex({
        temporal: {
          loading: { detected: true, durationMs: 1000 },
          errorPersisting: null,
          idleSince: null,
          focusTrail: [],
          stagnantCycles: 0,
        },
      });
      const bridge = new CortexBridge(cortex, mockCel());
      expect(bridge.isSettled()).toBe(false);
    });

    it("should return false when not idle", () => {
      const cortex = mockCortex({
        temporal: {
          loading: null,
          errorPersisting: null,
          idleSince: null,
          focusTrail: [],
          stagnantCycles: 0,
        },
      });
      const bridge = new CortexBridge(cortex, mockCel());
      expect(bridge.isSettled()).toBe(false);
    });

    it("should return true when cortex not running", () => {
      const cortex = mockCortex();
      cortex.isRunning.mockReturnValue(false);
      const bridge = new CortexBridge(cortex, mockCel());
      expect(bridge.isSettled()).toBe(true);
    });
  });

  describe("getPromptSignals", () => {
    it("should format non-actionable signals for prompt", () => {
      const bridge = new CortexBridge(mockCortex(), mockCel());
      const signals = [
        { type: "loading_stall" as const, description: "Loading for 6s", actionRequired: false },
        { type: "dialog" as const, description: "Cookie consent", actionRequired: true },
        { type: "idle" as const, description: "Page idle for 3s", actionRequired: false },
      ];

      const text = bridge.getPromptSignals(signals);
      expect(text).toContain("Loading for 6s");
      expect(text).toContain("Page idle for 3s");
      expect(text).not.toContain("Cookie consent"); // actionRequired = true → not in prompt
    });

    it("should return empty string when no informational signals", () => {
      const bridge = new CortexBridge(mockCortex(), mockCel());
      const text = bridge.getPromptSignals([
        { type: "dialog" as const, description: "Dialog", actionRequired: true },
      ]);
      expect(text).toBe("");
    });
  });

  describe("isVisionNeeded", () => {
    it("should reflect cortex visionNeeded flag", () => {
      const cortex = mockCortex({ visionNeeded: true });
      const bridge = new CortexBridge(cortex, mockCel());
      expect(bridge.isVisionNeeded()).toBe(true);
    });

    it("should return false when cortex not running", () => {
      const cortex = mockCortex({ visionNeeded: true });
      cortex.isRunning.mockReturnValue(false);
      const bridge = new CortexBridge(cortex, mockCel());
      expect(bridge.isVisionNeeded()).toBe(false);
    });
  });
});
