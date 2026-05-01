import { Annotation } from "@langchain/langgraph";

import type {
  AttemptRecord,
  CanonicalStep,
  GoalOutcome,
  NextMove,
  RuntimeCaps,
} from "./canonical.js";
import type { ScreenContext } from "../types.js";

export const CellarLangGraphState = Annotation.Root({
  goal: Annotation<string>,
  runId: Annotation<string | undefined>,
  sharedMemory: Annotation<unknown>,
  history: Annotation<AttemptRecord[]>,
  perception: Annotation<ScreenContext | null>,
  screenshotBase64: Annotation<string | null>,
  caps: Annotation<RuntimeCaps | null>,
  nextMove: Annotation<NextMove | null>,
  pendingPurpose: Annotation<string | null>,
  pendingSteps: Annotation<CanonicalStep[]>,
  stepIndex: Annotation<number>,
  outcome: Annotation<GoalOutcome | null>,
  lastError: Annotation<string | null>,
});

export interface CellarGraphStateValue {
  goal: string;
  runId?: string;
  sharedMemory: unknown;
  history: AttemptRecord[];
  perception: ScreenContext | null;
  screenshotBase64: string | null;
  caps: RuntimeCaps | null;
  nextMove: NextMove | null;
  pendingPurpose: string | null;
  pendingSteps: CanonicalStep[];
  stepIndex: number;
  outcome: GoalOutcome | null;
  lastError: string | null;
}

export function createInitialCellarGraphState(
  goal: string,
  runId?: string,
): CellarGraphStateValue {
  return {
    goal,
    runId,
    sharedMemory: {},
    history: [],
    perception: null,
    screenshotBase64: null,
    caps: null,
    nextMove: null,
    pendingPurpose: null,
    pendingSteps: [],
    stepIndex: 0,
    outcome: null,
    lastError: null,
  };
}
