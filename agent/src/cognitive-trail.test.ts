import { describe, it, expect, vi } from "vitest";
import { CognitiveTrail, type TrailEvent } from "./goal-runner/cognitive-trail.js";

describe("CognitiveTrail", () => {
  it("should add entries and retrieve them", () => {
    const trail = new CognitiveTrail();
    trail.add(0, "THINK", "I see a search form");
    trail.add(0, "ACT_OK", "click(Search)");
    trail.add(1, "NOTE", "cheapest=$149");

    const recent = trail.recent();
    expect(recent).toHaveLength(3);
    expect(recent[0].type).toBe("THINK");
    expect(recent[1].type).toBe("ACT_OK");
    expect(recent[2].type).toBe("NOTE");
    expect(recent[2].content).toBe("cheapest=$149");
  });

  it("should compact after threshold", () => {
    const trail = new CognitiveTrail();
    // Add 16 entries (threshold is 15)
    for (let i = 0; i < 16; i++) {
      trail.add(i, i % 2 === 0 ? "ACT_OK" : "ACT_FAIL", `action-${i}`);
    }
    // After compaction, only recent 8 should remain
    const recent = trail.recent();
    expect(recent.length).toBeLessThanOrEqual(8);
  });

  it("should generate prompt context with compacted summary", () => {
    const trail = new CognitiveTrail();
    for (let i = 0; i < 20; i++) {
      trail.add(i, "ACT_OK", `click-btn-${i}`);
    }
    trail.add(20, "MILESTONE", "on_results_page");

    const prompt = trail.toPromptContext();
    expect(prompt).toContain("Steps");
    expect(prompt).toContain("OK");
    expect(prompt).toContain("MILESTONE");
  });

  it("should snapshot and restore", () => {
    const trail = new CognitiveTrail();
    trail.add(0, "THINK", "thinking...");
    trail.add(1, "NOTE", "data=42");

    const snap = trail.snapshot();
    trail.add(2, "ACT_FAIL", "crashed");

    expect(trail.recent()).toHaveLength(3);
    trail.restoreFromSnapshot(snap);
    expect(trail.recent()).toHaveLength(2);
    expect(trail.recent()[1].content).toBe("data=42");
  });

  it("should generate human-readable summary", () => {
    const trail = new CognitiveTrail();
    trail.add(0, "THINK", "Navigating to site");
    trail.add(0, "ACT_OK", "navigate_url");
    trail.add(1, "INTERRUPT", "Cookie banner dismissed");

    const summary = trail.toSummary();
    expect(summary).toContain("THINK");
    expect(summary).toContain("ACT_OK");
    expect(summary).toContain("INTERRUPT");
    expect(summary).toContain("Cookie banner");
  });

  it("should record HEAL entries and include them in prompt context", () => {
    const trail = new CognitiveTrail();
    trail.add(0, "ACT_OK", "click(Search)");
    trail.add(1, "HEAL", '"click on btn-1" failed (element not found) → repaired to "click on btn-2" [context shifted]');
    trail.add(2, "ACT_OK", "type(query)");

    const recent = trail.recent();
    expect(recent).toHaveLength(3);
    expect(recent[1].type).toBe("HEAL");
    expect(recent[1].content).toContain("context shifted");

    const prompt = trail.toPromptContext();
    expect(prompt).toContain("HEAL:");
    expect(prompt).toContain("repaired to");
  });

  it("should filter HEAL entries from trail", () => {
    const trail = new CognitiveTrail();
    trail.add(0, "ACT_OK", "click(Search)");
    trail.add(1, "HEAL", "healed action");
    trail.add(2, "HEAL", "healed another [context shifted]");
    trail.add(3, "ACT_OK", "type(query)");

    const healEntries = trail.recent().filter(e => e.type === "HEAL");
    expect(healEntries).toHaveLength(2);

    const shiftEntries = healEntries.filter(e => e.content.includes("[context shifted]"));
    expect(shiftEntries).toHaveLength(1);
  });

  describe("subscribe (event envelope)", () => {
    it("delivers a trail.add event to each subscriber", () => {
      const trail = new CognitiveTrail();
      const events: TrailEvent[] = [];
      trail.subscribe(e => events.push(e));
      trail.add(3, "THINK", "reasoning", "planner");
      expect(events).toHaveLength(1);
      expect(events[0].type).toBe("trail.add");
      expect(events[0].entry.step).toBe(3);
      expect(events[0].entry.type).toBe("THINK");
      expect(events[0].entry.content).toBe("reasoning");
      expect(events[0].entry.ns).toBe("planner");
    });

    it("omits ns on the entry when not provided", () => {
      const trail = new CognitiveTrail();
      const events: TrailEvent[] = [];
      trail.subscribe(e => events.push(e));
      trail.add(0, "ACT_OK", "click(Search)");
      expect(events[0].entry.ns).toBeUndefined();
    });

    it("returns an unsubscribe function that stops further events", () => {
      const trail = new CognitiveTrail();
      const listener = vi.fn();
      const off = trail.subscribe(listener);
      trail.add(0, "THINK", "first");
      off();
      trail.add(1, "THINK", "second");
      expect(listener).toHaveBeenCalledTimes(1);
    });

    it("a throwing listener does not abort add()", () => {
      const trail = new CognitiveTrail();
      trail.subscribe(() => { throw new Error("boom"); });
      expect(() => trail.add(0, "THINK", "safe")).not.toThrow();
      expect(trail.recent()).toHaveLength(1);
    });
  });

  it("should preserve milestones in compacted summary", () => {
    const trail = new CognitiveTrail();
    // Add milestone early, then many more entries to trigger compaction
    trail.add(0, "MILESTONE", "search_done");
    for (let i = 1; i < 20; i++) {
      trail.add(i, "ACT_OK", `action-${i}`);
    }

    const prompt = trail.toPromptContext();
    // Milestone should survive in compacted summary or recent entries
    expect(prompt).toContain("search_done");
  });
});
