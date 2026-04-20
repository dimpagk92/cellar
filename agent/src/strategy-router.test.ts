import { describe, expect, it } from "vitest";
import { selectStrategyRoute } from "./strategy-router.js";
import type { FreshnessAssessment, PlannedAction, ScreenContext } from "./types.js";

const context: ScreenContext = {
  app: "Browser",
  window: "Test",
  elements: [],
  timestamp_ms: Date.now(),
};

function freshness(state: FreshnessAssessment["state"], causes: FreshnessAssessment["causes"] = []): FreshnessAssessment {
  return {
    state,
    causes,
    ageMs: state === "fresh" ? 100 : 2500,
    confidence: state === "hard-stale" ? 0.2 : state === "soft-stale" ? 0.6 : 0.95,
    lastUpdateMs: Date.now(),
    lastEventMs: null,
    lastSignificantEventMs: null,
  };
}

describe("selectStrategyRoute", () => {
  it("prefers structured execution on a fresh model", () => {
    const action: PlannedAction = { type: "click", target_id: "btn-1" };
    const result = selectStrategyRoute({ action, context, freshness: freshness("fresh") });
    expect(result.route).toBe("structured");
  });

  it("promotes semantic resolution on a soft-stale model", () => {
    const action: PlannedAction = { type: "type", text: "hello", target_id: "input-1" };
    const result = selectStrategyRoute({
      action,
      context,
      freshness: freshness("soft-stale", ["time"]),
    });
    expect(result.route).toBe("semantic");
  });

  it("promotes semantic resolution before acting when the target is ambiguous", () => {
    const action: PlannedAction = { type: "click", target_id: "btn-1" };
    const result = selectStrategyRoute({
      action,
      context,
      freshness: freshness("fresh"),
      ambiguity: {
        ambiguous: true,
        confidence: 0.82,
        reason: "Goal matches a different near-duplicate interactive target",
        preferredTargetId: "btn-2",
      },
    });
    expect(result.route).toBe("semantic");
    expect(result.reason).toContain("near-duplicate");
  });

  it("forces refresh on a hard-stale model", () => {
    const action: PlannedAction = { type: "click", target_id: "btn-1" };
    const result = selectStrategyRoute({
      action,
      context,
      freshness: freshness("hard-stale", ["event"]),
    });
    expect(result.route).toBe("refresh");
  });

  it("escalates from structured to semantic to vision", () => {
    const action: PlannedAction = { type: "click", target_id: "btn-1" };
    const semantic = selectStrategyRoute({
      action,
      context,
      freshness: freshness("fresh"),
      attempts: [{ route: "structured", success: false, verified: false }],
    });
    expect(semantic.route).toBe("semantic");

    const vision = selectStrategyRoute({
      action,
      context,
      freshness: freshness("fresh"),
      attempts: [
        { route: "structured", success: false, verified: false },
        { route: "semantic", success: false, verified: false },
      ],
    });
    expect(vision.route).toBe("vision");
  });

  it("returns terminal failure after vision has already failed", () => {
    const action: PlannedAction = { type: "click", target_id: "btn-1" };
    const result = selectStrategyRoute({
      action,
      context,
      freshness: freshness("fresh"),
      attempts: [
        { route: "structured", success: false, verified: false },
        { route: "semantic", success: false, verified: false },
        { route: "vision", success: false, verified: false },
      ],
    });
    expect(result.route).toBe("terminal_failure");
    expect(result.terminal).toBe(true);
  });
});
