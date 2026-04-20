/**
 * Test utilities for CEL agent — type-safe mock factories.
 *
 * Usage:
 *   import { createMockPlanner, sampleContext } from "../test-utils/index.js";
 *
 *   const planner = createMockPlanner({ steps: [clickStep, doneStep] });
 *   const result = await planStep(planner, "Click submit", sampleContext(), [], null, 10, false, callbacks);
 *   expect(planner.calls.planStep).toHaveLength(1);
 */

export {
  createMockContextProvider,
  emptyContext,
  sampleContext,
  type MockContextProviderOptions,
} from "./mock-context-provider.js";

export {
  createMockInputController,
  type InputCall,
} from "./mock-input-controller.js";

export {
  createMockPlanner,
  type MockPlannerOptions,
} from "./mock-planner.js";

export {
  createMockKnowledgeStore,
} from "./mock-knowledge-store.js";
