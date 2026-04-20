import { describe, it, expect, beforeEach, afterEach } from "vitest";
import * as fs from "node:fs";
import * as path from "node:path";
import * as os from "node:os";
import { RunTranscript } from "./transcript.js";
import type { AssembledContext } from "./context-assembly.js";

let tempDir: string;

beforeEach(() => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "cellar-test-"));
});

afterEach(() => {
  fs.rmSync(tempDir, { recursive: true, force: true });
});

function makeContext(): AssembledContext {
  return {
    workflow: { name: "test", description: "test", app: "TestApp", currentStep: 0, totalSteps: 3 },
    workingMemory: "some memory",
    observations: [{ id: 1, content: "obs", priority: "high", workflow_name: "test", source_run_ids: "[]", observed_at: "", referenced_at: null, superseded_by: null, created_at: "" }],
    knowledge: [{ id: 1, content: "fact", source: "test", workflow_scope: null, score: 0.9, created_at: "" }],
    screen: { app: "TestApp", window: "Main", elements: [{ id: "btn", element_type: "button", confidence: 0.95, source: "accessibility_tree", state: { focused: false, enabled: true, visible: true, selected: false } }], timestamp_ms: 1000 },
    recentSteps: [],
    currentStep: { id: "s1", description: "Click button", action: { type: "click", target: "btn" } },
  };
}

describe("RunTranscript", () => {
  it("should create transcript file on first write", () => {
    const t = new RunTranscript(1, tempDir);
    t.logRunStart("test-wf", 5);

    const entries = t.read();
    expect(entries).toHaveLength(1);
    expect(entries[0].entry_type).toBe("run_start");
    expect(entries[0].data.workflow).toBe("test-wf");
  });

  it("should append multiple entries", () => {
    const t = new RunTranscript(1, tempDir);
    t.logRunStart("test-wf", 3);
    t.logContextCapture(0, "s1", makeContext());
    t.logActionExecuted(0, "s1", { type: "click" }, 0.95);
    t.logStepComplete(0, "s1", 0.95);
    t.logRunComplete("completed", [
      { stepIndex: 0, stepId: "s1", description: "Click", success: true, confidence: 0.95 },
    ]);

    const entries = t.read();
    expect(entries).toHaveLength(5);
    expect(entries.map((e) => e.entry_type)).toEqual([
      "run_start",
      "context_capture",
      "action_executed",
      "step_complete",
      "run_complete",
    ]);
  });

  it("should log context capture with memory stats", () => {
    const t = new RunTranscript(1, tempDir);
    t.logContextCapture(0, "s1", makeContext());

    const entries = t.read();
    expect(entries[0].data.observations_count).toBe(1);
    expect(entries[0].data.knowledge_count).toBe(1);
    expect(entries[0].data.has_working_memory).toBe(true);
  });

  it("should log failures with error messages", () => {
    const t = new RunTranscript(1, tempDir);
    t.logStepFailed(2, "s3", "Element not found", 0.3);

    const entries = t.read();
    expect(entries[0].entry_type).toBe("step_failed");
    expect(entries[0].data.error).toBe("Element not found");
    expect(entries[0].step_index).toBe(2);
  });

  it("should log pauses", () => {
    const t = new RunTranscript(1, tempDir);
    t.logPaused(1, "s2", 0.2);

    const entries = t.read();
    expect(entries[0].entry_type).toBe("paused");
  });

  it("should log interventions", () => {
    const t = new RunTranscript(1, tempDir);
    t.logIntervention(1, "s2", { type: "click", x: 100, y: 200 });

    const entries = t.read();
    expect(entries[0].entry_type).toBe("intervention");
    expect(entries[0].data.user_action).toEqual({ type: "click", x: 100, y: 200 });
  });

  it("should log observation generation", () => {
    const t = new RunTranscript(1, tempDir);
    t.logObservationGenerated(42, "Step 3 always fails on Mondays", "high");

    const entries = t.read();
    expect(entries[0].entry_type).toBe("observation_generated");
    expect(entries[0].data.observation_id).toBe(42);
  });

  it("should compute run stats on completion", () => {
    const t = new RunTranscript(1, tempDir);
    t.logRunComplete("failed", [
      { stepIndex: 0, stepId: "s1", description: "A", success: true, confidence: 0.9 },
      { stepIndex: 1, stepId: "s2", description: "B", success: true, confidence: 0.8 },
      { stepIndex: 2, stepId: "s3", description: "C", success: false, confidence: 0.3 },
    ]);

    const entries = t.read();
    expect(entries[0].data.steps_succeeded).toBe(2);
    expect(entries[0].data.steps_failed).toBe(1);
    expect(entries[0].data.avg_confidence).toBeCloseTo(0.667, 2);
  });

  it("should return empty array for non-existent transcript", () => {
    const t = new RunTranscript(999, tempDir);
    expect(t.read()).toEqual([]);
  });

  it("should return the file path", () => {
    const t = new RunTranscript(42, tempDir);
    expect(t.getPath()).toContain("42");
    expect(t.getPath()).toContain("transcript.jsonl");
  });
});
