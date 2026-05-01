import {
  Command,
  END,
  interrupt,
  MemorySaver,
  START,
  StateGraph,
} from "@langchain/langgraph";

import type {
  AttemptRecord,
  CanonicalStep,
  CanonicalStepResult,
  GoalOutcome,
  ReviewDecision,
} from "./canonical.js";
import type { CellarLangGraphDriver } from "./driver.js";
import type { CellarLangGraphPlanner } from "./planner.js";
import {
  CellarLangGraphState,
  type CellarGraphStateValue,
} from "./state.js";
import {
  defaultCellarGraphPolicy,
  type CellarGraphPolicy,
} from "./policy.js";

export interface CellarGraphOptions {
  driver: CellarLangGraphDriver;
  planner: CellarLangGraphPlanner;
  policy?: CellarGraphPolicy;
  checkpointer?: unknown;
}

function mergeSharedMemory(memory: unknown, key: string, data: unknown): unknown {
  if (data == null) {
    return memory ?? {};
  }
  const base = memory && typeof memory === "object" && !Array.isArray(memory)
    ? { ...(memory as Record<string, unknown>) }
    : {};
  base[key] = data;
  return base;
}

function recordAttempt(step: CanonicalStep, result: CanonicalStepResult): AttemptRecord {
  if (result.status === "ok") {
    return {
      step_purpose: step.purpose,
      action: step.action,
      succeeded: true,
      error: null,
      data: result.data ?? null,
    };
  }

  return {
    step_purpose: step.purpose,
    action: step.action,
    succeeded: false,
    error: result.message,
    data: null,
  };
}

function isPendingStepAvailable(state: CellarGraphStateValue): boolean {
  return state.stepIndex < state.pendingSteps.length;
}

export function createCellarGraph(options: CellarGraphOptions) {
  const policy = options.policy ?? defaultCellarGraphPolicy;
  const checkpointer = (options.checkpointer ?? new MemorySaver()) as any;

  const builder = new StateGraph(CellarLangGraphState)
    .addNode("perceive", async (state: CellarGraphStateValue) => {
      const frame = await options.driver.perceive({
        captureScreenshot: policy.captureScreenshot(state),
      });

      return new Command({
        update: {
          perception: frame.perception,
          screenshotBase64: frame.screenshot_base64 ?? null,
          caps: frame.caps,
          lastError: null,
        },
        goto: "routeAfterPerceive",
      });
    }, { ends: ["routeAfterPerceive"] })
    .addNode("routeAfterPerceive", (state: CellarGraphStateValue) => {
      return new Command({
        goto: isPendingStepAvailable(state) ? "executeStep" : "plan",
      });
    }, { ends: ["executeStep", "plan"] })
    .addNode("plan", async (state: CellarGraphStateValue) => {
      if (!state.perception || !state.caps) {
        return new Command({ goto: "perceive" });
      }

      const nextMove = await options.planner.decideNext({
        goal: state.goal,
        history: state.history,
        sharedMemory: state.sharedMemory,
        frame: {
          perception: state.perception,
          screenshot_base64: state.screenshotBase64,
          caps: state.caps,
        },
      });

      if (nextMove.kind === "done") {
        return new Command({
          update: {
            nextMove,
            pendingPurpose: null,
            pendingSteps: [],
            stepIndex: 0,
            lastError: null,
          },
          goto: "verifyDone",
        });
      }

      if (nextMove.kind === "fail") {
        const outcome: GoalOutcome = {
          status: "failed",
          failing_sub_goal: state.pendingPurpose ?? "<planner>",
          failing_step: "<planner fail>",
          attempts: [nextMove.reason],
        };
        return new Command({
          update: {
            nextMove,
            outcome,
            lastError: nextMove.reason,
          },
          goto: "terminal",
        });
      }

      return new Command({
        update: {
          nextMove,
          pendingPurpose: nextMove.purpose,
          pendingSteps: nextMove.steps,
          stepIndex: 0,
          lastError: null,
        },
        goto: "executeStep",
      });
    }, { ends: ["perceive", "executeStep", "verifyDone", "terminal"] })
    .addNode("executeStep", async (state: CellarGraphStateValue) => {
      const originalStep = state.pendingSteps[state.stepIndex];
      if (!originalStep) {
        return new Command({
          update: {
            pendingPurpose: null,
            pendingSteps: [],
            stepIndex: 0,
            nextMove: null,
          },
          goto: "plan",
        });
      }

      let step = originalStep;
      let pendingSteps = state.pendingSteps;

      if (policy.interruptBeforeStep(step, state)) {
        const decision = interrupt({
          kind: "review_step",
          goal: state.goal,
          pending_purpose: state.pendingPurpose,
          step_index: state.stepIndex,
          step,
        }) as ReviewDecision | undefined;

        if (decision && decision.approved === false) {
          const outcome: GoalOutcome = {
            status: "failed",
            failing_sub_goal: state.pendingPurpose ?? "<human review>",
            failing_step: step.purpose,
            attempts: [decision.feedback ?? "Step rejected by human review"],
          };
          return new Command({
            update: {
              outcome,
              lastError: outcome.attempts[0],
            },
            goto: "terminal",
          });
        }

        if (decision?.edited_step) {
          step = decision.edited_step;
          pendingSteps = [...state.pendingSteps];
          pendingSteps[state.stepIndex] = step;
        }
      }

      const result = await options.driver.executeStep(step);
      const attempt = recordAttempt(step, result);
      const history = [...state.history, attempt];

      if (result.status === "err") {
        return new Command({
          update: {
            history,
            pendingPurpose: null,
            pendingSteps: [],
            stepIndex: 0,
            nextMove: null,
            lastError: result.message,
          },
          goto: "perceive",
        });
      }

      let sharedMemory = state.sharedMemory;
      if (result.data !== undefined) {
        sharedMemory = mergeSharedMemory(sharedMemory, step.purpose, result.data);
      }

      const nextStepIndex = state.stepIndex + 1;
      const hasMoreSteps = nextStepIndex < pendingSteps.length;

      return new Command({
        update: {
          history,
          sharedMemory,
          pendingSteps: hasMoreSteps ? pendingSteps : [],
          stepIndex: hasMoreSteps ? nextStepIndex : 0,
          pendingPurpose: hasMoreSteps ? state.pendingPurpose : null,
          nextMove: hasMoreSteps ? state.nextMove : null,
          lastError: null,
        },
        goto: hasMoreSteps && !policy.perceiveAfterStep(step, result, state)
          ? "executeStep"
          : "perceive",
      });
    }, { ends: ["executeStep", "perceive", "plan", "terminal"] })
    .addNode("verifyDone", async (state: CellarGraphStateValue) => {
      if (!state.perception || !state.nextMove || state.nextMove.kind !== "done" || !state.caps) {
        return new Command({ goto: "perceive" });
      }

      const verdict = await options.planner.verifyDone({
        goal: state.goal,
        summary: state.nextMove.summary,
        sharedMemory: state.sharedMemory,
        frame: {
          perception: state.perception,
          screenshot_base64: state.screenshotBase64,
          caps: state.caps,
        },
      });

      if (verdict.verified) {
        const outcome: GoalOutcome = {
          status: "succeeded",
          summary: state.nextMove.summary,
          extracted_data: state.nextMove.extracted_data ?? state.sharedMemory,
        };

        return new Command({
          update: {
            outcome,
            lastError: null,
          },
          goto: "terminal",
        });
      }

      const verifyAttempt: AttemptRecord = {
        step_purpose: "verify_done",
        action: {
          type: "done",
          summary: state.nextMove.summary,
          evidence_ids: [],
        },
        succeeded: false,
        error: `runtime rejected Done: ${verdict.reason}`,
        data: null,
      };

      return new Command({
        update: {
          history: [...state.history, verifyAttempt],
          nextMove: null,
          pendingPurpose: null,
          pendingSteps: [],
          stepIndex: 0,
          lastError: verdict.reason,
        },
        goto: "perceive",
      });
    }, { ends: ["perceive", "terminal"] })
    .addNode("terminal", (state: CellarGraphStateValue) => {
      return state;
    })
    .addEdge(START, "perceive")
    .addEdge("terminal", END);

  return builder.compile({ checkpointer });
}
