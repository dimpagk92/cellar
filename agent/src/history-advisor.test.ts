import { describe, it, expect, vi } from "vitest";
import { HistoryAdvisor } from "./goal-runner/history-advisor.js";
import { CognitiveTrail } from "./goal-runner/cognitive-trail.js";
import { Notebook } from "./goal-runner/notebook.js";

// Mock Cel with the 4 methods HistoryAdvisor uses
function mockCel(overrides: {
  knowledge?: Array<{ id: number; content: string; source: string; score: number }>;
  observations?: Array<{ id: number; content: string; priority: string; observed_at: string }>;
  runs?: Array<{ id: number; workflow_name: string; status: string; steps_completed: number; steps_total: number; interventions: number; started_at: string }>;
  workingMemory?: string;
} = {}) {
  return {
    searchKnowledge: vi.fn().mockReturnValue(overrides.knowledge ?? []),
    getObservations: vi.fn().mockReturnValue(overrides.observations ?? []),
    getRunHistory: vi.fn().mockReturnValue(overrides.runs ?? []),
    getWorkingMemory: vi.fn().mockReturnValue(overrides.workingMemory ?? ""),
    addObservation: vi.fn().mockReturnValue(1),
    addScopedKnowledge: vi.fn().mockReturnValue(1),
    updateWorkingMemory: vi.fn(),
  } as any;
}

describe("HistoryAdvisor", () => {
  describe("query", () => {
    it("should return null when no relevant data", async () => {
      const cel = mockCel();
      const result = await HistoryAdvisor.query(cel, "book a hotel", "test-workflow");
      expect(result).toBeNull();
    });

    it("should include knowledge in advice", async () => {
      const cel = mockCel({
        knowledge: [
          { id: 1, content: "Hotels.com requires cookie dismissal", source: "past-run", score: 0.8 },
        ],
      });
      const result = await HistoryAdvisor.query(cel, "book a hotel", "hotel-booking");
      expect(result).toContain("RELEVANT KNOWLEDGE");
      expect(result).toContain("cookie dismissal");
    });

    it("should include observations in advice", async () => {
      const cel = mockCel({
        observations: [
          { id: 1, content: "Login required before booking", priority: "high", observed_at: "2026-03-28" },
        ],
      });
      const result = await HistoryAdvisor.query(cel, "book a hotel", "hotel-booking");
      expect(result).toContain("PAST OBSERVATIONS");
      expect(result).toContain("Login required");
      expect(result).toContain("[high]");
    });

    it("should include run history statistics", async () => {
      const cel = mockCel({
        runs: [
          { id: 1, workflow_name: "hotel-booking", status: "completed", steps_completed: 8, steps_total: 20, interventions: 0, started_at: "2026-03-27" },
          { id: 2, workflow_name: "hotel-booking", status: "failed", steps_completed: 15, steps_total: 20, interventions: 0, started_at: "2026-03-26" },
        ],
      });
      const result = await HistoryAdvisor.query(cel, "book a hotel", "hotel-booking");
      expect(result).toContain("PAST RUNS");
      expect(result).toContain("1/2 succeeded");
    });

    it("should include working memory", async () => {
      const cel = mockCel({
        workingMemory: "Hotel A: $149/night, Hotel B: $179/night",
      });
      const result = await HistoryAdvisor.query(cel, "book a hotel", "hotel-booking");
      expect(result).toContain("WORKING MEMORY");
      expect(result).toContain("Hotel A");
    });

    it("should skip sections with no data", async () => {
      const cel = mockCel({
        knowledge: [{ id: 1, content: "Useful fact", source: "test", score: 0.9 }],
        // No observations, runs, or memory
      });
      const result = await HistoryAdvisor.query(cel, "test goal", "test-wf");
      expect(result).toContain("RELEVANT KNOWLEDGE");
      expect(result).not.toContain("PAST OBSERVATIONS");
      expect(result).not.toContain("PAST RUNS");
      expect(result).not.toContain("WORKING MEMORY");
    });
  });

  describe("queryForReplan", () => {
    it("should search with failure context", async () => {
      const cel = mockCel({
        knowledge: [
          { id: 1, content: "Payment form requires scrolling on mobile", source: "past-run", score: 0.7 },
        ],
      });
      const result = await HistoryAdvisor.queryForReplan(
        cel, "book a hotel", "payment form timed out", "hotel-booking",
      );
      expect(result).toContain("PAST EXPERIENCE WITH SIMILAR FAILURES");
      expect(result).toContain("scrolling");
      // Verify search was called with combined keywords
      expect(cel.searchKnowledge).toHaveBeenCalledWith(
        expect.stringContaining("payment"),
        "hotel-booking",
        5,
      );
    });

    it("should return null when no relevant failure data", async () => {
      const cel = mockCel();
      const result = await HistoryAdvisor.queryForReplan(
        cel, "test", "unknown error", "wf",
      );
      expect(result).toBeNull();
    });
  });

  describe("storeOutcome", () => {
    it("should store notebook data as scoped knowledge", async () => {
      const cel = mockCel();
      const trail = new CognitiveTrail();
      trail.add(0, "MILESTONE", "search_done");
      const notebook = new Notebook();
      notebook.write("price", "$149", "step-3", "data");
      notebook.write("url", "hotels.com/room/1", "step-5", "url");

      await HistoryAdvisor.storeOutcome(cel, "book hotel", trail, notebook, "hotel-wf", true);

      // Should store each data/url notebook entry as knowledge
      expect(cel.addScopedKnowledge).toHaveBeenCalledTimes(2);
      expect(cel.addScopedKnowledge).toHaveBeenCalledWith(
        "price: $149",
        expect.stringContaining("book hotel"),
        "hotel-wf",
        "data",
      );

      // Should store observation with milestone info
      expect(cel.addObservation).toHaveBeenCalledWith(
        "hotel-wf",
        expect.stringContaining("succeeded"),
        "medium",
        [],
      );
      expect(cel.addObservation).toHaveBeenCalledWith(
        "hotel-wf",
        expect.stringContaining("search_done"),
        "medium",
        [],
      );

      // Should update working memory
      expect(cel.updateWorkingMemory).toHaveBeenCalled();
    });

    it("should store failure with high priority", async () => {
      const cel = mockCel();
      const trail = new CognitiveTrail();
      const notebook = new Notebook();

      await HistoryAdvisor.storeOutcome(cel, "book hotel", trail, notebook, "hotel-wf", false);

      expect(cel.addObservation).toHaveBeenCalledWith(
        "hotel-wf",
        expect.stringContaining("failed"),
        "high",
        [],
      );
    });

    it("should skip storage when no workflow name", async () => {
      const cel = mockCel();
      await HistoryAdvisor.storeOutcome(
        cel, "test", new CognitiveTrail(), new Notebook(), undefined, true,
      );
      expect(cel.addObservation).not.toHaveBeenCalled();
    });
  });
});
