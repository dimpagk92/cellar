import type { Cel } from "../cel-bindings.js";
import type {
  AttemptRecord,
  DoneVerdict,
  NextMove,
  PerceptionFrame,
  PlanningBudget,
} from "./canonical.js";
import type { CellarLangGraphPlanner } from "./planner.js";

/**
 * LangGraph-facing planner that delegates to the canonical Rust planner
 * via the CEL N-API surface.
 *
 * **PR1b: now a thin shim over `cel.canonicalBuildPlanningView` +
 * `cel.canonicalDecideNext` / `canonicalVerifyDone`.** Previously held its
 * own TS-side prompt + context-compression logic, which forked off the
 * Rust planner and caused planner fragmentation. Now the cortex builds
 * one PlanningView per turn and the Rust planner consumes it; this class
 * just bridges that into the LangGraph graph contract.
 *
 * The LLM client is the Rust-side one (configured via `CEL_LLM_PROVIDER`
 * or `~/.cellar/config.toml`). The TS-side `LlmSurface` dependency is
 * gone — there's nothing left for the TS layer to do during planning.
 */

export interface CelLlmPlannerOptions {
  /** Hard cap on planner turns. Used to short-circuit before the LLM call. */
  maxSteps?: number;
  /** Optional planning budget overriding `cel-cortex` defaults. */
  budget?: PlanningBudget;
}

const DEFAULT_OPTIONS: Required<Omit<CelLlmPlannerOptions, "budget">> & {
  budget: PlanningBudget | undefined;
} = {
  maxSteps: 80,
  budget: undefined,
};

/**
 * Subset of [`Cel`] that this planner needs. Lets tests mock the
 * canonical helpers without standing up the full native module.
 */
export type PlannerSurface = Pick<
  Cel,
  "canonicalBuildPlanningView" | "canonicalDecideNext" | "canonicalVerifyDone"
>;

export class CelLlmPlanner implements CellarLangGraphPlanner {
  private readonly options: typeof DEFAULT_OPTIONS;

  constructor(
    private readonly cel: PlannerSurface,
    options: CelLlmPlannerOptions = {},
  ) {
    this.options = { ...DEFAULT_OPTIONS, ...options };
  }

  async decideNext(input: {
    goal: string;
    history: AttemptRecord[];
    sharedMemory: unknown;
    frame: PerceptionFrame;
  }): Promise<NextMove> {
    if (input.history.length >= this.options.maxSteps) {
      return {
        kind: "fail",
        reason: `max_steps budget exhausted after ${input.history.length} executed steps`,
      };
    }
    const view = await this.cel.canonicalBuildPlanningView(input.goal, {
      budget: this.options.budget,
      perception: input.frame.perception,
      caps: input.frame.caps,
    });
    return this.cel.canonicalDecideNext(
      view,
      input.history,
      input.sharedMemory,
      input.frame.screenshot_base64 ?? null,
    );
  }

  async verifyDone(input: {
    goal: string;
    summary: string;
    sharedMemory: unknown;
    frame: PerceptionFrame;
  }): Promise<DoneVerdict> {
    const view = await this.cel.canonicalBuildPlanningView(input.goal, {
      budget: this.options.budget,
      perception: input.frame.perception,
      caps: input.frame.caps,
    });
    return this.cel.canonicalVerifyDone(
      view,
      input.summary,
      input.sharedMemory,
      input.frame.screenshot_base64 ?? null,
    );
  }
}
