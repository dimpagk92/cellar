import { describe, it, expect } from "vitest";
import { CheckpointManager } from "./goal-runner/checkpoint-manager.js";
import type { NotebookEntry } from "./goal-runner/notebook.js";

describe("CheckpointManager", () => {
  const mockNotebook: NotebookEntry[] = [
    { key: "price", value: "$149", source: "step-3", category: "data", timestamp: 1000 },
  ];

  it("should capture a checkpoint", () => {
    const mgr = new CheckpointManager();
    const cp = mgr.capture("on_results_page", 5, "fp-123", "https://hotels.com/results", "Chrome — Hotels.com", mockNotebook);

    expect(cp.milestone).toBe("on_results_page");
    expect(cp.stepIndex).toBe(5);
    expect(cp.url).toBe("https://hotels.com/results");
    expect(cp.notebookSnapshot).toHaveLength(1);
    expect(mgr.count).toBe(1);
  });

  it("should get latest checkpoint", () => {
    const mgr = new CheckpointManager();
    mgr.capture("step_1", 2, "fp-1", null, "App — Win", []);
    mgr.capture("step_2", 8, "fp-2", "https://example.com", "App — Win", mockNotebook);

    const latest = mgr.getLatest();
    expect(latest?.milestone).toBe("step_2");
    expect(latest?.stepIndex).toBe(8);
  });

  it("should get previous checkpoint for backtracking", () => {
    const mgr = new CheckpointManager();
    mgr.capture("step_1", 2, "fp-1", null, "App", []);
    mgr.capture("step_2", 8, "fp-2", null, "App", mockNotebook);

    const prev = mgr.getPrevious();
    expect(prev?.milestone).toBe("step_1");
    expect(prev?.stepIndex).toBe(2);
  });

  it("should return first checkpoint when only one exists", () => {
    const mgr = new CheckpointManager();
    mgr.capture("only_one", 3, "fp", null, "App", []);

    expect(mgr.getPrevious()?.milestone).toBe("only_one");
    expect(mgr.getLatest()?.milestone).toBe("only_one");
  });

  it("should return null when no checkpoints", () => {
    const mgr = new CheckpointManager();
    expect(mgr.getLatest()).toBeNull();
    expect(mgr.getPrevious()).toBeNull();
  });

  it("should get checkpoint by milestone name", () => {
    const mgr = new CheckpointManager();
    mgr.capture("search", 2, "fp-1", null, "App", []);
    mgr.capture("results", 8, "fp-2", null, "App", []);
    mgr.capture("booking", 15, "fp-3", null, "App", mockNotebook);

    const results = mgr.getByMilestone("results");
    expect(results?.stepIndex).toBe(8);
  });

  it("should deep-copy notebook snapshot", () => {
    const mgr = new CheckpointManager();
    const original = [{ key: "a", value: "1", source: "s", category: "data" as const, timestamp: 0 }];
    mgr.capture("test", 0, "fp", null, "App", original);

    // Mutate original — checkpoint should not be affected
    original[0].value = "MUTATED";
    const cp = mgr.getLatest();
    expect(cp?.notebookSnapshot[0].value).toBe("1");
  });

  it("should generate summary", () => {
    const mgr = new CheckpointManager();
    mgr.capture("search", 2, "fp", "https://example.com", "Chrome", []);
    mgr.capture("results", 8, "fp", null, "App — Window", []);

    const summary = mgr.toSummary();
    expect(summary).toContain("[0] search at step 2");
    expect(summary).toContain("[1] results at step 8");
  });
});
