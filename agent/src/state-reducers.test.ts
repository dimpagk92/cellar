import { describe, it, expect } from "vitest";
import {
  appendReducer,
  mergeDictReducer,
  overwriteReducer,
  initialPersistentState,
  initialEphemeralState,
  resetEphemeralForReplan,
} from "./goal-runner/state.js";

describe("state.ts reducers", () => {
  it("appendReducer concatenates in order", () => {
    expect(appendReducer([1, 2], [3, 4])).toEqual([1, 2, 3, 4]);
    expect(appendReducer<number>([], [1])).toEqual([1]);
    expect(appendReducer([1], [])).toEqual([1]);
  });

  it("appendReducer does not mutate inputs", () => {
    const a = [1, 2];
    const b = [3];
    appendReducer(a, b);
    expect(a).toEqual([1, 2]);
    expect(b).toEqual([3]);
  });

  it("mergeDictReducer overlays update onto current", () => {
    expect(mergeDictReducer({ a: 1, b: 2 }, { b: 3, c: 4 }))
      .toEqual({ a: 1, b: 3, c: 4 });
  });

  it("mergeDictReducer does not mutate inputs", () => {
    const current = { a: 1 };
    const update = { b: 2 };
    mergeDictReducer(current, update);
    expect(current).toEqual({ a: 1 });
    expect(update).toEqual({ b: 2 });
  });

  it("overwriteReducer returns the update verbatim", () => {
    expect(overwriteReducer("old", "new")).toBe("new");
    expect(overwriteReducer(1, 2)).toBe(2);
  });
});

describe("state.ts factories", () => {
  it("initialPersistentState produces empty containers", () => {
    const s = initialPersistentState();
    expect(s.trail).toEqual([]);
    expect(s.notebook).toEqual({});
    expect(s.strategyAttempts).toEqual({});
    expect(s.checkpoints).toEqual([]);
    expect(s.history).toEqual([]);
  });

  it("initialEphemeralState starts with default milestone and zero counters", () => {
    const s = initialEphemeralState();
    expect(s.currentMilestone).toBe("default");
    expect(s.consecutiveFailures).toBe(0);
    expect(s.sameClickCount).toBe(0);
    expect(s.consecutiveScrolls).toBe(0);
    expect(s.consecutiveNotebookWrites).toBe(0);
    expect(s.lastClickTarget).toBe("");
    expect(s.loopWarning).toBeNull();
  });

  it("resetEphemeralForReplan clears counters and warning but preserves milestone", () => {
    const s = {
      ...initialEphemeralState(),
      currentMilestone: "on_results_page",
      consecutiveFailures: 4,
      sameClickCount: 3,
      consecutiveScrolls: 2,
      consecutiveNotebookWrites: 1,
      lastClickTarget: "btn-foo",
      loopWarning: "stuck",
    };
    const reset = resetEphemeralForReplan(s);
    expect(reset.currentMilestone).toBe("on_results_page");
    expect(reset.consecutiveFailures).toBe(0);
    expect(reset.sameClickCount).toBe(0);
    expect(reset.consecutiveScrolls).toBe(0);
    expect(reset.consecutiveNotebookWrites).toBe(0);
    expect(reset.lastClickTarget).toBe("");
    expect(reset.loopWarning).toBeNull();
  });

  it("resetEphemeralForReplan does not mutate input", () => {
    const s = { ...initialEphemeralState(), consecutiveFailures: 3 };
    resetEphemeralForReplan(s);
    expect(s.consecutiveFailures).toBe(3);
  });
});
