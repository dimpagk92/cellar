import type {
  AttemptRecord,
  DoneVerdict,
  NextMove,
  PerceptionFrame,
} from "./canonical.js";

export interface CellarLangGraphPlanner {
  decideNext(input: {
    goal: string;
    history: AttemptRecord[];
    sharedMemory: unknown;
    frame: PerceptionFrame;
  }): Promise<NextMove>;
  verifyDone(input: {
    goal: string;
    summary: string;
    sharedMemory: unknown;
    frame: PerceptionFrame;
  }): Promise<DoneVerdict>;
}

export const permissiveDoneVerifier: Pick<CellarLangGraphPlanner, "verifyDone"> = {
  async verifyDone() {
    return {
      verified: true,
      reason: "",
    };
  },
};
