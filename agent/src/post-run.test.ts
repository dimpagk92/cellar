import { describe, it, expect, vi, beforeEach } from "vitest";
import { processPostRun } from "./post-run.js";
import type { StepResult } from "./context-assembly.js";
import { CognitiveTrail } from "./goal-runner/cognitive-trail.js";

function makeCel() {
  return {
    addObservation: vi.fn().mockReturnValue(1),
    getWorkingMemory: vi.fn().mockReturnValue(""),
    updateWorkingMemory: vi.fn(),
    addScopedKnowledge: vi.fn().mockReturnValue(1),
  } as any;
}

function makeSteps(overrides: Partial<StepResult>[] = []): StepResult[] {
  const defaults: StepResult[] = [
    { stepIndex: 0, stepId: "s1", description: "Open form", success: true, confidence: 0.95 },
    { stepIndex: 1, stepId: "s2", description: "Enter data", success: true, confidence: 0.88 },
    { stepIndex: 2, stepId: "s3", description: "Submit", success: true, confidence: 0.92 },
  ];
  return defaults.map((d, i) => ({ ...d, ...overrides[i] }));
}

describe("processPostRun", () => {
  it("should create observations from a perfect run", () => {
    const cel = makeCel();
    const result = processPostRun(cel, "daily-po", 1, makeSteps());

    expect(result.observationsCreated).toBeGreaterThan(0);
    // Perfect run → low priority observation
    expect(cel.addObservation).toHaveBeenCalledWith(
      "daily-po",
      expect.stringContaining("perfectly"),
      "low",
      [1],
    );
  });

  it("should create high priority observation on failure", () => {
    const cel = makeCel();
    const steps = makeSteps([{}, {}, { success: false, confidence: 0.3 }]);
    const result = processPostRun(cel, "daily-po", 1, steps);

    expect(result.observationsCreated).toBeGreaterThan(0);
    expect(cel.addObservation).toHaveBeenCalledWith(
      "daily-po",
      expect.stringContaining("failed"),
      "high",
      [1],
    );
  });

  it("should create medium priority observation for low confidence steps", () => {
    const cel = makeCel();
    const steps = makeSteps([
      { confidence: 0.95 },
      { confidence: 0.4 },
      { confidence: 0.5 },
    ]);
    const result = processPostRun(cel, "daily-po", 1, steps);

    expect(cel.addObservation).toHaveBeenCalledWith(
      "daily-po",
      expect.stringContaining("Low confidence"),
      "medium",
      [1],
    );
  });

  it("should extract failure knowledge", () => {
    const cel = makeCel();
    const steps = makeSteps([{}, { success: false }, {}]);
    const result = processPostRun(cel, "daily-po", 1, steps);

    expect(result.knowledgeAdded).toBe(1);
    expect(cel.addScopedKnowledge).toHaveBeenCalledWith(
      expect.stringContaining("s2"),
      "auto:post-run",
      "daily-po",
      "failure,auto-extracted",
    );
  });

  it("should update working memory with run summary", () => {
    const cel = makeCel();
    processPostRun(cel, "daily-po", 42, makeSteps());

    expect(cel.updateWorkingMemory).toHaveBeenCalledWith(
      "daily-po",
      expect.stringContaining("Run #42"),
    );
    expect(cel.updateWorkingMemory).toHaveBeenCalledWith(
      "daily-po",
      expect.stringContaining("3/3 steps"),
    );
  });

  it("should append to existing working memory", () => {
    const cel = makeCel();
    cel.getWorkingMemory.mockReturnValue("# Field Mappings\n- Vendor X → 10045");
    processPostRun(cel, "daily-po", 1, makeSteps());

    const call = cel.updateWorkingMemory.mock.calls[0];
    expect(call[1]).toContain("Field Mappings");
    expect(call[1]).toContain("Recent Runs");
  });

  it("should keep only last 5 runs in working memory", () => {
    const cel = makeCel();
    cel.getWorkingMemory.mockReturnValue(
      "# Recent Runs\n" +
      "- Run #5 (2024-01-05): 3/3 steps, avg 90% confidence\n" +
      "- Run #4 (2024-01-04): 3/3 steps, avg 90% confidence\n" +
      "- Run #3 (2024-01-03): 3/3 steps, avg 90% confidence\n" +
      "- Run #2 (2024-01-02): 3/3 steps, avg 90% confidence\n" +
      "- Run #1 (2024-01-01): 3/3 steps, avg 90% confidence"
    );
    processPostRun(cel, "daily-po", 6, makeSteps());

    const call = cel.updateWorkingMemory.mock.calls[0];
    const runLines = call[1].split("\n").filter((l: string) => l.startsWith("- Run #"));
    expect(runLines).toHaveLength(5);
    expect(runLines[0]).toContain("Run #6"); // newest first
    expect(runLines[4]).toContain("Run #2"); // oldest kept (Run #1 evicted)
  });

  it("should return correct result counts", () => {
    const cel = makeCel();
    const steps = makeSteps([{}, { success: false }, {}]);
    const result = processPostRun(cel, "wf", 1, steps);

    expect(result.observationsCreated).toBeGreaterThan(0);
    expect(result.workingMemoryUpdated).toBe(true);
    expect(result.knowledgeAdded).toBe(1);
  });

  it("should handle empty step list gracefully", () => {
    const cel = makeCel();
    const result = processPostRun(cel, "wf", 1, []);

    expect(result.workingMemoryUpdated).toBe(true);
    expect(result.knowledgeAdded).toBe(0);
  });

  it("should extract healing observations from cognitive trail", () => {
    const cel = makeCel();
    const trail = new CognitiveTrail();
    trail.add(0, "ACT_OK", "click(Search)");
    trail.add(1, "HEAL", '"click on btn-1" failed (element not found) → repaired to "click on btn-2"');
    trail.add(2, "ACT_OK", "type(query)");

    const result = processPostRun(cel, "wf", 1, makeSteps(), undefined, trail);

    // Should have observations from steps + healing
    expect(result.observationsCreated).toBeGreaterThan(0);
    expect(cel.addObservation).toHaveBeenCalledWith(
      "wf",
      expect.stringContaining("self-heal"),
      "medium",
      [1],
    );
  });

  it("should create high priority observation for context shifts during healing", () => {
    const cel = makeCel();
    const trail = new CognitiveTrail();
    trail.add(0, "HEAL", '"click on btn-1" failed → repaired to "click on btn-2" [context shifted]');

    const result = processPostRun(cel, "wf", 1, makeSteps(), undefined, trail);

    expect(cel.addObservation).toHaveBeenCalledWith(
      "wf",
      expect.stringContaining("context shift"),
      "high",
      [1],
    );
  });

  it("should store healing patterns as scoped knowledge", () => {
    const cel = makeCel();
    const trail = new CognitiveTrail();
    trail.add(0, "HEAL", '"click on btn-1" failed → repaired to "click on btn-2"');

    const result = processPostRun(cel, "wf", 1, makeSteps(), undefined, trail);

    expect(cel.addScopedKnowledge).toHaveBeenCalledWith(
      expect.stringContaining("repaired"),
      "auto:self-heal",
      "wf",
      "healing,auto-extracted",
    );
    expect(result.knowledgeAdded).toBeGreaterThan(0);
  });

  it("should not create healing observations when no trail is provided", () => {
    const cel = makeCel();
    const result = processPostRun(cel, "wf", 1, makeSteps());

    // Only step-based observations, no healing ones
    const healCalls = cel.addObservation.mock.calls.filter(
      (c: any[]) => String(c[1]).includes("self-heal"),
    );
    expect(healCalls).toHaveLength(0);
  });
});
