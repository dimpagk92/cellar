import { describe, expect, it } from "vitest";
import { normalizeCortexAnomalies, normalizeCortexModel } from "./cortex-normalize.js";

describe("normalizeCortexModel", () => {
  it("converts snake_case Rust cortex payloads into the TS shape", () => {
    const normalized = normalizeCortexModel({
      current_context: {
        app: "Browser",
        window: "Main",
        elements: [],
        timestamp_ms: 123,
      },
      focused_element: { id: "dom:submit", label: "Submit" },
      recent_diffs: [{ addedCount: 1, removedCount: 0, changedCount: 0, unchangedCount: 5, addedLabels: [], changedLabels: [] }],
      last_diff_summary: {
        added_count: 2,
        removed_count: 1,
        changed_count: 3,
        unchanged_count: 4,
      },
      vision_needed: true,
      age_ms: 10,
      cycle_count: 2,
      uptime_ms: 30,
      temporal: {
        idle_since: 50,
        focus_trail: ["Submit"],
        stagnant_cycles: 1,
        error_persisting: { detected: true, duration_ms: 300, message: "Oops" },
      },
      freshness: {
        state: "soft-stale",
        causes: ["time"],
        ageMs: 10,
        confidence: 0.6,
        last_update_ms: 100,
        last_event_ms: 110,
        last_significant_event_ms: 0,
      },
      stability: {
        stable: ["dom:submit"],
        volatile: [],
      },
    });

    expect(normalized?.currentContext.app).toBe("Browser");
    expect(normalized?.focusedElement?.id).toBe("dom:submit");
    expect(normalized?.visionNeeded).toBe(true);
    expect(normalized?.ageMs).toBe(10);
    expect(normalized?.cycleCount).toBe(2);
    expect(normalized?.uptimeMs).toBe(30);
    expect(normalized?.temporal.idleSince).toBe(50);
    expect(normalized?.temporal.errorPersisting?.durationMs).toBe(300);
    expect(normalized?.freshness?.lastUpdateMs).toBe(100);
    expect(normalized?.lastDiffSummary?.addedCount).toBe(2);
    expect(normalized?.lastDiffSummary?.removedCount).toBe(1);
    expect(normalized?.lastDiffSummary?.changedCount).toBe(3);
    expect(normalized?.lastDiffSummary?.unchangedCount).toBe(4);
    expect(normalized?.stability.stable.has("dom:submit")).toBe(true);
    expect(normalized?.semantic?.currentActivity).toContain("Browser");
    expect(normalized?.sourceSummary?.accessibility).toBe(0);
  });

  it("normalizes anomaly arrays from raw connector payloads", () => {
    const anomalies = normalizeCortexAnomalies(JSON.stringify([
      { type: "dialog", description: "Consent modal", timestamp: 1 },
    ]));
    expect(anomalies).toHaveLength(1);
    expect(anomalies[0]?.description).toBe("Consent modal");
  });
});
