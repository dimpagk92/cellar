import type { CanonicalStep, CanonicalStepResult } from "./canonical.js";
import type { CellarGraphStateValue } from "./state.js";

export interface CellarGraphPolicy {
  captureScreenshot(state: CellarGraphStateValue): boolean;
  interruptBeforeStep(step: CanonicalStep, state: CellarGraphStateValue): boolean;
  perceiveAfterStep(
    step: CanonicalStep,
    result: CanonicalStepResult,
    state: CellarGraphStateValue,
  ): boolean;
}

const REVIEW_TYPES = new Set<string>(["write_cells", "custom"]);

export const defaultCellarGraphPolicy: CellarGraphPolicy = {
  captureScreenshot: () => true,
  interruptBeforeStep: (step) => REVIEW_TYPES.has(step.action.type),
  perceiveAfterStep: () => true,
};
