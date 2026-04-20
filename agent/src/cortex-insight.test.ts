import { describe, expect, it } from "vitest";
import { deriveFreshnessAssessment, deriveSemanticInsight, deriveSourceSummary, enrichMentalModel } from "./cortex-insight.js";
import type { MentalModel } from "./types.js";

function makeModel(): MentalModel {
  return {
    currentContext: {
      app: "Google Chrome",
      window: "Checkout",
      timestamp_ms: Date.now() - 2400,
      elements: [
        {
          id: "field-email",
          element_type: "input",
          label: "Email",
          value: "",
          state: { focused: true, enabled: true, visible: true, selected: false },
          actions: ["type"],
          confidence: 0.98,
          source: "accessibility_tree",
        },
        {
          id: "btn-pay",
          element_type: "button",
          label: "Pay now",
          value: "",
          state: { focused: false, enabled: true, visible: true, selected: false },
          actions: ["click"],
          confidence: 0.94,
          source: "native_api",
        },
      ],
      network_events: [],
      http_events: [],
      window_list: [],
      running_apps: [],
      recent_files: [],
      transcripts: [],
    },
    focusedElement: { id: "field-email", label: "Email" },
    recentDiffs: [],
    temporal: {
      loading: null,
      errorPersisting: null,
      idleSince: null,
      focusTrail: ["Cart", "Email"],
      stagnantCycles: 0,
    },
    stability: {
      stable: new Set(),
      volatile: new Set(),
    },
    anomalyQueue: [],
    confidence: 0.72,
    visionNeeded: false,
    ageMs: 0,
    cycleCount: 1,
    uptimeMs: 1000,
    elementAdapterIndex: {
      "btn-pay": "browser",
    },
  };
}

describe("cortex-insight", () => {
  it("derives freshness from age and confidence when the raw model does not include it", () => {
    const freshness = deriveFreshnessAssessment(makeModel(), Date.now());
    expect(freshness.state).toBe("soft-stale");
    expect(freshness.causes).toContain("time");
    expect(freshness.causes).toContain("confidence");
  });

  it("derives semantic insight from focus, diffs, and blockers", () => {
    const insight = deriveSemanticInsight({
      ...makeModel(),
      anomalyQueue: [{ type: "dialog", description: "Payment confirmation modal", timestamp: Date.now() }],
      lastDiffSummary: { addedCount: 1, removedCount: 0, changedCount: 2, unchangedCount: 8 },
    });

    expect(insight.taskPhase).toBe("blocked");
    expect(insight.currentActivity).toContain("Google Chrome");
    expect(insight.recentTransition).toContain("Focus moved");
    expect(insight.likelyBlocker).toContain("Payment confirmation modal");
    expect(insight.suggestedNextStep).toContain("dialog");
  });

  it("summarizes source coverage, including adapter-backed elements", () => {
    const summary = deriveSourceSummary(makeModel());
    expect(summary.accessibility).toBe(1);
    expect(summary.nativeApi).toBe(1);
    expect(summary.adapterBacked).toBe(1);
  });

  it("enriches a mental model in place with derived metadata", () => {
    const model = makeModel();
    const enriched = enrichMentalModel(model, Date.now());
    expect(enriched.freshness?.state).toBe("soft-stale");
    expect(enriched.semantic?.taskPhase).toBe("input");
    expect(enriched.sourceSummary?.adapterBacked).toBe(1);
  });
});
