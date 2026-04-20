import { describe, it, expect } from "vitest";
import { Notebook } from "./goal-runner/notebook.js";

describe("Notebook", () => {
  it("should write and read entries", () => {
    const nb = new Notebook();
    nb.write("price", "$149", "step-3", "data");
    expect(nb.read("price")).toBe("$149");
    expect(nb.size).toBe(1);
    expect(nb.isEmpty).toBe(false);
  });

  it("should upsert on duplicate key", () => {
    const nb = new Notebook();
    nb.write("price", "$149", "step-3", "data");
    nb.write("price", "$179", "step-5", "data");
    expect(nb.read("price")).toBe("$179");
    expect(nb.size).toBe(1);
  });

  it("should evict oldest when exceeding max entries", () => {
    const nb = new Notebook();
    for (let i = 0; i < 12; i++) {
      nb.write(`key-${i}`, `val-${i}`, "step-0", "data");
    }
    // Max is 10, so 2 oldest should be evicted
    expect(nb.size).toBe(10);
    // key-0 and key-1 should be evicted (oldest timestamps)
    expect(nb.read("key-0")).toBeUndefined();
    expect(nb.read("key-1")).toBeUndefined();
    expect(nb.read("key-11")).toBe("val-11");
  });

  it("should generate compact prompt context", () => {
    const nb = new Notebook();
    nb.write("cheapest", "$149", "step-3", "data");
    nb.write("dates", "Mar 15-17", "step-2", "data");

    const prompt = nb.toPromptContext();
    expect(prompt).toContain("SAVED DATA:");
    expect(prompt).toContain("cheapest=$149");
    expect(prompt).toContain("dates=Mar 15-17");
  });

  it("should return empty string for empty notebook", () => {
    const nb = new Notebook();
    expect(nb.toPromptContext()).toBe("");
    expect(nb.toSummary()).toBe("");
  });

  it("should generate summary grouped by category", () => {
    const nb = new Notebook();
    nb.write("price", "$149", "step-3", "data");
    nb.write("url", "https://hotels.com", "step-1", "url");
    nb.write("note", "site requires login", "step-0", "observation");

    const summary = nb.toSummary();
    expect(summary).toContain("Data:");
    expect(summary).toContain("Url:");
    expect(summary).toContain("Observation:");
  });

  it("should snapshot and restore", () => {
    const nb = new Notebook();
    nb.write("a", "1", "s0", "data");
    nb.write("b", "2", "s1", "data");

    const snap = nb.snapshot();
    nb.write("c", "3", "s2", "data");
    expect(nb.size).toBe(3);

    nb.restoreFromSnapshot(snap);
    expect(nb.size).toBe(2);
    expect(nb.read("c")).toBeUndefined();
    expect(nb.read("a")).toBe("1");
  });

  it("should clear all entries", () => {
    const nb = new Notebook();
    nb.write("x", "y", "s0", "data");
    nb.clear();
    expect(nb.isEmpty).toBe(true);
    expect(nb.size).toBe(0);
  });
});
