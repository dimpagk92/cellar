import type { Cel } from "../cel-bindings.js";
import type { ScreenContext, PageContent } from "../types.js";

import type {
  CanonicalStep,
  CanonicalStepResult,
  PerceptionFrame,
  PlanningBudget,
  PlanningView,
  RuntimeCaps,
} from "./canonical.js";

export interface CellarLangGraphDriver {
  perceive(options?: { captureScreenshot?: boolean }): Promise<PerceptionFrame>;
  executeStep(step: CanonicalStep): Promise<CanonicalStepResult>;
  /**
   * Build a budgeted `PlanningView` via the cortex selector. Stateful by
   * default (uses booted cortex); callers may pass their own perception +
   * caps for a stateless build (e.g. to render a view of a captured frame).
   *
   * Added in PR1c so the LangGraph tool surface (`see` in tools.ts) can
   * render the SAME selected context the canonical Rust runner sees,
   * eliminating the last TS-side context-compression fork.
   */
  buildPlanningView(
    goal: string,
    options?: {
      budget?: PlanningBudget;
      perception?: ScreenContext;
      caps?: RuntimeCaps;
    },
  ): Promise<PlanningView>;
  getPageContent?(): Promise<PageContent | null>;
  readModel?(): { activeAdapters?: string[] | null } | null;
}

export class CelLangGraphDriver implements CellarLangGraphDriver {
  constructor(
    private readonly cel: Pick<
      Cel,
      | "canonicalPerceive"
      | "canonicalExecuteStep"
      | "canonicalBuildPlanningView"
      | "getCdpPageContent"
      | "readCortexModel"
    >,
  ) {}

  perceive(options?: { captureScreenshot?: boolean }): Promise<PerceptionFrame> {
    return this.cel.canonicalPerceive(options?.captureScreenshot ?? true);
  }

  executeStep(step: CanonicalStep): Promise<CanonicalStepResult> {
    return this.cel.canonicalExecuteStep(step);
  }

  buildPlanningView(
    goal: string,
    options?: { budget?: PlanningBudget; perception?: ScreenContext; caps?: RuntimeCaps },
  ): Promise<PlanningView> {
    return this.cel.canonicalBuildPlanningView(goal, options);
  }

  getPageContent(): Promise<PageContent | null> {
    return this.cel.getCdpPageContent();
  }

  readModel(): { activeAdapters?: string[] | null } | null {
    return this.cel.readCortexModel() as { activeAdapters?: string[] | null } | null;
  }
}
