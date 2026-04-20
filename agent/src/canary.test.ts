import { describe, it, expect } from "vitest";
import {
  canaryCohort,
  resolveCanaryPercentage,
  applyCanaryOverride,
} from "./goal-runner/canary.js";

describe("canaryCohort", () => {
  it("returns control at 0%", () => {
    expect(canaryCohort("any-key", 0)).toBe("control");
  });

  it("returns enabled at 100%", () => {
    expect(canaryCohort("any-key", 100)).toBe("enabled");
  });

  it("is deterministic for the same key", () => {
    const a = canaryCohort("hotel-booking", 50);
    const b = canaryCohort("hotel-booking", 50);
    expect(a).toBe(b);
  });

  it("produces a roughly balanced distribution at 50%", () => {
    let enabled = 0;
    const N = 1000;
    for (let i = 0; i < N; i++) {
      if (canaryCohort(`workflow-${i}`, 50) === "enabled") enabled++;
    }
    // 50% target, 1000 samples → should be within 40-60%
    expect(enabled).toBeGreaterThan(400);
    expect(enabled).toBeLessThan(600);
  });

  it("produces a roughly 10% distribution at 10%", () => {
    let enabled = 0;
    const N = 1000;
    for (let i = 0; i < N; i++) {
      if (canaryCohort(`workflow-${i}`, 10) === "enabled") enabled++;
    }
    expect(enabled).toBeGreaterThan(60);
    expect(enabled).toBeLessThan(150);
  });
});

describe("resolveCanaryPercentage", () => {
  it("returns 0 when env var is unset", () => {
    delete process.env.TEST_CANARY_PCT;
    expect(resolveCanaryPercentage("TEST_CANARY_PCT")).toBe(0);
  });

  it("clamps negative values to 0", () => {
    process.env.TEST_CANARY_PCT = "-50";
    expect(resolveCanaryPercentage("TEST_CANARY_PCT")).toBe(0);
    delete process.env.TEST_CANARY_PCT;
  });

  it("clamps >100 values to 100", () => {
    process.env.TEST_CANARY_PCT = "999";
    expect(resolveCanaryPercentage("TEST_CANARY_PCT")).toBe(100);
    delete process.env.TEST_CANARY_PCT;
  });

  it("parses valid numeric values", () => {
    process.env.TEST_CANARY_PCT = "25";
    expect(resolveCanaryPercentage("TEST_CANARY_PCT")).toBe(25);
    delete process.env.TEST_CANARY_PCT;
  });

  it("treats non-numeric values as 0", () => {
    process.env.TEST_CANARY_PCT = "abc";
    expect(resolveCanaryPercentage("TEST_CANARY_PCT")).toBe(0);
    delete process.env.TEST_CANARY_PCT;
  });
});

describe("applyCanaryOverride", () => {
  it("leaves enableTierReplan alone when cohort=control", () => {
    const cfg = { goal: "x", enableTierReplan: false };
    const out = applyCanaryOverride(cfg, 0);
    expect(out.enableTierReplan).toBe(false);
  });

  it("overrides enableTierReplan=true when cohort=enabled", () => {
    const cfg = { goal: "x", enableTierReplan: false };
    const out = applyCanaryOverride(cfg, 100);
    expect(out.enableTierReplan).toBe(true);
  });

  it("respects an already-set enableTierReplan=true", () => {
    const cfg = { goal: "x", enableTierReplan: true };
    const out = applyCanaryOverride(cfg, 0);
    expect(out.enableTierReplan).toBe(true);
  });

  it("uses workflowName as the bucket key when provided", () => {
    const cfgA = { goal: "different-goal-1", workflowName: "wf", enableTierReplan: false };
    const cfgB = { goal: "different-goal-2", workflowName: "wf", enableTierReplan: false };
    const a = applyCanaryOverride(cfgA, 50);
    const b = applyCanaryOverride(cfgB, 50);
    // Same workflowName → same cohort
    expect(a.enableTierReplan).toBe(b.enableTierReplan);
  });

  it("does not mutate the input config", () => {
    const cfg = { goal: "x", enableTierReplan: false };
    applyCanaryOverride(cfg, 100);
    expect(cfg.enableTierReplan).toBe(false);
  });
});
