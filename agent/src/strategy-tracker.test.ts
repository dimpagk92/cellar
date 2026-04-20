import { describe, it, expect } from "vitest";
import { StrategyTracker } from "./goal-runner/strategy-tracker.js";

describe("StrategyTracker", () => {
  it("should register and track strategies", () => {
    const tracker = new StrategyTracker();
    const id = tracker.register("search", "click each field and type");
    expect(id).toContain("search");
    expect(tracker.attemptCount("search")).toBe(1);
    expect(tracker.currentStrategy("search")).toBe(id);
  });

  it("should record outcomes", () => {
    const tracker = new StrategyTracker();
    const id = tracker.register("search", "click and type");
    tracker.recordOutcome(id, "failed", "fields not responding", 5);

    const failed = tracker.getFailedStrategies("search");
    expect(failed).toHaveLength(1);
    expect(failed[0]).toContain("click and type");
    expect(failed[0]).toContain("fields not responding");
  });

  it("should enforce max 3 strategies per milestone", () => {
    const tracker = new StrategyTracker();

    tracker.register("form", "strategy 1");
    tracker.recordOutcome(tracker.currentStrategy("form")!, "failed");

    tracker.register("form", "strategy 2");
    tracker.recordOutcome(tracker.currentStrategy("form")!, "failed");

    tracker.register("form", "strategy 3");
    tracker.recordOutcome(tracker.currentStrategy("form")!, "failed");

    expect(tracker.canReplan("form")).toBe(false);
  });

  it("should enforce max 5 global replans", () => {
    const tracker = new StrategyTracker();

    for (let i = 0; i < 5; i++) {
      tracker.register(`milestone-${i}`, `strategy-${i}`);
    }

    expect(tracker.canReplanGlobal()).toBe(false);
  });

  it("should return failed strategies for prompt injection", () => {
    const tracker = new StrategyTracker();

    const id1 = tracker.register("nav", "use address bar");
    tracker.recordOutcome(id1, "failed", "URL blocked");

    const id2 = tracker.register("nav", "click link on page");
    tracker.recordOutcome(id2, "loop", "stuck in loop");

    const failed = tracker.getFailedStrategies("nav");
    expect(failed).toHaveLength(2);
    expect(failed[0]).toContain("address bar");
    expect(failed[1]).toContain("click link");
  });

  it("should not count success as a failed strategy", () => {
    const tracker = new StrategyTracker();
    const id = tracker.register("search", "direct navigation");
    tracker.recordOutcome(id, "success");

    const failed = tracker.getFailedStrategies("search");
    expect(failed).toHaveLength(0);
  });

  it("should return all failed strategies when no milestone specified", () => {
    const tracker = new StrategyTracker();

    const id1 = tracker.register("search", "approach A");
    tracker.recordOutcome(id1, "failed");

    const id2 = tracker.register("booking", "approach B");
    tracker.recordOutcome(id2, "failed");

    const allFailed = tracker.getFailedStrategies();
    expect(allFailed).toHaveLength(2);
  });

  it("should generate summary", () => {
    const tracker = new StrategyTracker();
    tracker.register("search", "click fields");
    tracker.recordOutcome(tracker.currentStrategy("search")!, "failed");
    tracker.register("search", "set_value API");

    const summary = tracker.toSummary();
    expect(summary).toContain("search:");
    expect(summary).toContain("failed");
    expect(summary).toContain("in_progress");
  });
});
