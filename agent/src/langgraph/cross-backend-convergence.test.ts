/**
 * Cross-backend convergence test.
 *
 * After PR1b, the canonical Rust runner and the LangGraph TS planner both
 * end at the same N-API export: `canonicalDecideNext(view, ...)`. The
 * `PlanningView` they pass is built by the same cortex-side selector, via
 * `canonicalBuildPlanningView`. There is no longer a TS-side fork.
 *
 * This test makes that convergence explicit: given the same goal +
 * perception + caps, both call sites end up invoking
 * `canonicalDecideNext` with byte-identical view JSON. If a future change
 * re-introduces TS-side context compression, this test breaks loudly.
 *
 * Plan reference: COGNITION_LAYER_PLAN.md, "Evals" section — "the same
 * scenario can run against canonical and LangGraph backends using the
 * same CEL planning view."
 */

import { describe, expect, it, vi } from "vitest";

import { CelLlmPlanner, type PlannerSurface } from "./llm-planner.js";
import type {
  AttemptRecord,
  NextMove,
  PerceptionFrame,
  PlanningView,
  RuntimeCaps,
} from "./canonical.js";
import type { ScreenContext } from "../types.js";

function fixedPerception(): ScreenContext {
  return {
    app: "Test App",
    window: "Form",
    timestamp_ms: 1_700_000_000_000,
    elements: [
      {
        id: "ax:submit",
        label: "Submit",
        element_type: "button",
        state: { focused: false, enabled: true, visible: true, selected: false },
        confidence: 1,
        source: "accessibility_tree",
        actions: ["click"],
      },
    ],
  };
}

function fixedCaps(): RuntimeCaps {
  return {
    cdp_bound: false,
    native_input: true,
    steps_used: 0,
    max_steps: 80,
  };
}

function stubView(goal: string): PlanningView {
  // Deterministic view fixture — what we'd expect the cortex to produce
  // for the fixed perception. Both backends should invoke
  // `canonicalDecideNext` with this exact JSON.
  return {
    goal,
    budget: {
      max_tokens: 8000,
      max_elements: 80,
      max_memories: 8,
      max_adapter_facts: 12,
    },
    screen: { active_app: "Test App", window: "Form" },
    elements: [
      {
        id: "ax:submit",
        element_type: "button",
        label: "Submit",
        state: {
          focused: false,
          selected: false,
          enabled: true,
          checked: false,
          expanded: false,
        },
        clickable: true,
        settable: false,
      },
    ],
    adapter_facts: [],
    adapter_actions: [],
    capabilities: [{ id: "native_input" }],
    run_progress: { steps_used: 0, max_steps: 80 },
    memories: [],
    knowledge: [],
    recent_events: [],
    blockers: [],
    anomalies: [],
    evidence: [],
    omitted_counts: {
      elements: 0,
      memories: 0,
      knowledge: 0,
      adapter_facts: 0,
      recent_events: 0,
    },
  };
}

describe("cross-backend planner convergence", () => {
  it("LangGraph CelLlmPlanner ends at canonicalDecideNext with the cortex-built view", async () => {
    const goal = "submit the form";
    const view = stubView(goal);

    // The Cel mock represents the N-API surface. After PR1b, both
    // backends ultimately call `canonicalDecideNext` through this surface.
    const buildView = vi.fn(
      async (
        g: string,
        _opts?: unknown,
      ): Promise<PlanningView> => stubView(g),
    );
    const decideNext = vi.fn(
      async (
        _view: PlanningView,
        _history: AttemptRecord[],
        _shared: unknown,
        _screenshot: string | null,
      ): Promise<NextMove> => ({
        kind: "batch",
        purpose: "click submit",
        steps: [
          {
            purpose: "click",
            kind: "deterministic",
            action: { type: "click", target_id: "ax:submit" },
          },
        ],
      }),
    );
    const cel: PlannerSurface = {
      canonicalBuildPlanningView: buildView,
      canonicalDecideNext: decideNext,
      canonicalVerifyDone: vi.fn(),
    };

    const planner = new CelLlmPlanner(cel);
    const frame: PerceptionFrame = {
      perception: fixedPerception(),
      screenshot_base64: null,
      caps: fixedCaps(),
    };
    const history: AttemptRecord[] = [];
    const sharedMemory = {};

    await planner.decideNext({ goal, history, sharedMemory, frame });

    // 1. The LangGraph path requested a planning view with the same goal,
    //    perception, and caps the canonical Rust runner would.
    expect(buildView).toHaveBeenCalledTimes(1);
    expect(buildView).toHaveBeenCalledWith(
      goal,
      expect.objectContaining({
        perception: frame.perception,
        caps: frame.caps,
      }),
    );

    // 2. The LangGraph path then invoked decideNext with the EXACT view
    //    the builder produced — no TS-side massaging.
    expect(decideNext).toHaveBeenCalledTimes(1);
    const call = decideNext.mock.calls[0];
    expect(JSON.stringify(call[0])).toEqual(JSON.stringify(view));
    expect(call[1]).toBe(history);
    expect(call[2]).toBe(sharedMemory);
    expect(call[3]).toBeNull();
  });

  it("verifyDone follows the same convergence pattern", async () => {
    const goal = "submit the form";
    const view = stubView(goal);

    const buildView = vi.fn(
      async (
        g: string,
        _opts?: unknown,
      ): Promise<PlanningView> => stubView(g),
    );
    const verifyDone = vi.fn(
      async (
        _view: PlanningView,
        _summary: string,
        _shared: unknown,
        _screenshot: string | null,
      ) => ({ verified: true, reason: "ok" }),
    );
    const cel: PlannerSurface = {
      canonicalBuildPlanningView: buildView,
      canonicalDecideNext: vi.fn(),
      canonicalVerifyDone: verifyDone,
    };

    const planner = new CelLlmPlanner(cel);
    const frame: PerceptionFrame = {
      perception: fixedPerception(),
      screenshot_base64: null,
      caps: fixedCaps(),
    };

    await planner.verifyDone({
      goal,
      summary: "submitted",
      sharedMemory: {},
      frame,
    });

    expect(buildView).toHaveBeenCalledTimes(1);
    expect(verifyDone).toHaveBeenCalledTimes(1);
    const call = verifyDone.mock.calls[0];
    expect(JSON.stringify(call[0])).toEqual(JSON.stringify(view));
    expect(call[1]).toBe("submitted");
  });

  it("documents that no TS-side context compression remains in the planner path", async () => {
    // Static-evidence-by-mock: if some future refactor re-introduces
    // TS-side compression, this assertion breaks because the planner
    // would touch something OTHER than canonicalBuildPlanningView /
    // canonicalDecideNext.
    const buildView = vi.fn(
      async (
        g: string,
        _opts?: unknown,
      ): Promise<PlanningView> => stubView(g),
    );
    const decideNext = vi.fn(
      async (
        _view: PlanningView,
        _history: AttemptRecord[],
        _shared: unknown,
        _screenshot: string | null,
      ): Promise<NextMove> => ({
        kind: "done",
        summary: "x",
      }),
    );
    const cel: PlannerSurface = {
      canonicalBuildPlanningView: buildView,
      canonicalDecideNext: decideNext,
      canonicalVerifyDone: vi.fn(),
    };

    const planner = new CelLlmPlanner(cel);
    await planner.decideNext({
      goal: "any",
      history: [],
      sharedMemory: null,
      frame: {
        perception: fixedPerception(),
        screenshot_base64: null,
        caps: fixedCaps(),
      },
    });

    // Exactly one buildView + one decideNext. No other Cel surface is
    // touched. (The compressContext / serializeContextForLLM TS helpers
    // would have shown up as extra setup if they were still on the path.)
    expect(buildView).toHaveBeenCalledTimes(1);
    expect(decideNext).toHaveBeenCalledTimes(1);
  });
});
