import type {
  Workflow,
  WorkflowStep,
  WorkflowStatus,
  ScreenContext,
} from "./types.js";
import { WorkflowQueue, type QueueEntry } from "./queue.js";
import type { Cel } from "./cel-bindings.js";
import {
  assembleContext,
  formatContextSummary,
  type AssembledContext,
  type StepResult,
  type ContextAssemblyConfig,
} from "./context-assembly.js";

/** Confidence thresholds matching CEL's confidence-driven behavior. */
const CONFIDENCE_THRESHOLDS = {
  actImmediately: 0.9,
  actAndLog: 0.7,
  actCautiously: 0.5,
};

export interface EngineCallbacks {
  /** Called to get current screen context from CEL. */
  getContext: () => Promise<ScreenContext>;
  /** Called to execute an action via CEL input layer. */
  executeAction: (step: WorkflowStep, context: AssembledContext) => Promise<boolean>;
  /** Called when confidence is too low — agent pauses for user. */
  onPause: (step: WorkflowStep, context: AssembledContext) => Promise<void>;
  /** Called on step completion. */
  onStepComplete: (step: WorkflowStep, stepIndex: number, context: AssembledContext) => void;
  /** Called on workflow completion with full step history. */
  onComplete: (workflow: Workflow, status: WorkflowStatus, steps: StepResult[]) => void;
  /** Called for logging. */
  onLog: (level: "info" | "warn" | "error", message: string) => void;
}

export interface EngineOptions {
  /** CEL instance for memory lookups. If not provided, context assembly is skipped. */
  cel?: Cel;
  /** Configuration for context assembly budget caps. */
  contextConfig?: ContextAssemblyConfig;
}

/**
 * Workflow execution engine.
 * Runs workflows step-by-step, respecting confidence thresholds.
 * Now memory-aware: assembles full context (working memory, observations,
 * knowledge, screen) before each step.
 */
export class WorkflowEngine {
  private queue = new WorkflowQueue();
  private running = false;
  private cel?: Cel;
  private contextConfig: ContextAssemblyConfig;

  constructor(private callbacks: EngineCallbacks, options: EngineOptions = {}) {
    this.cel = options.cel;
    this.contextConfig = options.contextConfig ?? {};
  }

  /** Submit a workflow for execution. */
  submit(workflow: Workflow, priority: "low" | "normal" | "high" | "critical" = "normal"): string {
    return this.queue.enqueue(workflow, priority);
  }

  /** Start processing the queue. */
  async start(): Promise<void> {
    if (this.running) return;
    this.running = true;

    while (this.running) {
      const entry = this.queue.dequeue();
      if (!entry) {
        // No work — wait briefly and check again
        await sleep(1000);
        continue;
      }

      await this.executeWorkflow(entry);
    }
  }

  /** Stop the engine after the current workflow completes. */
  stop(): void {
    this.running = false;
  }

  private async executeWorkflow(entry: QueueEntry): Promise<void> {
    const { workflow } = entry;
    this.callbacks.onLog("info", `Starting workflow: ${workflow.name}`);

    let status: WorkflowStatus = "completed";
    const completedSteps: StepResult[] = [];

    for (let i = 0; i < workflow.steps.length; i++) {
      if (!this.running) {
        status = "failed";
        break;
      }

      const step = workflow.steps[i];
      const screen = await this.callbacks.getContext();

      // Assemble full context if CEL is available, otherwise use screen-only
      const assembled = this.cel
        ? assembleContext(this.cel, workflow, i, screen, completedSteps, this.contextConfig)
        : makeMinimalContext(workflow, i, screen, completedSteps);

      this.callbacks.onLog("info", formatContextSummary(assembled));

      // Check confidence of relevant elements
      const relevantElements = screen.elements.filter(
        (el) => el.id === step.action.type || el.label === step.description
      );
      const maxConfidence = relevantElements.length > 0
        ? Math.max(...relevantElements.map((el) => el.confidence))
        : 0;

      const minRequired = step.min_confidence ?? CONFIDENCE_THRESHOLDS.actCautiously;

      if (maxConfidence < minRequired) {
        this.callbacks.onLog(
          "warn",
          `Low confidence (${maxConfidence}) at step ${i}: ${step.description}`
        );
        await this.callbacks.onPause(step, assembled);
      }

      try {
        const success = await this.callbacks.executeAction(step, assembled);

        const stepResult: StepResult = {
          stepIndex: i,
          stepId: step.id,
          description: step.description,
          success,
          confidence: maxConfidence,
        };
        completedSteps.push(stepResult);

        if (!success) {
          this.callbacks.onLog("error", `Step ${i} failed: ${step.description}`);
          status = "failed";
          break;
        }
        this.callbacks.onStepComplete(step, i, assembled);
      } catch (err) {
        completedSteps.push({
          stepIndex: i,
          stepId: step.id,
          description: step.description,
          success: false,
          confidence: maxConfidence,
        });
        this.callbacks.onLog("error", `Step ${i} error: ${err}`);
        status = "failed";
        break;
      }
    }

    this.queue.complete(status as "completed" | "failed");
    this.callbacks.onComplete(workflow, status, completedSteps);
  }
}

/** Build a minimal AssembledContext when CEL is not available. */
function makeMinimalContext(
  workflow: Workflow,
  stepIndex: number,
  screen: ScreenContext,
  completedSteps: StepResult[],
): AssembledContext {
  return {
    workflow: {
      name: workflow.name,
      description: workflow.description,
      app: workflow.app,
      currentStep: stepIndex,
      totalSteps: workflow.steps.length,
    },
    workingMemory: "",
    observations: [],
    knowledge: [],
    screen,
    recentSteps: completedSteps.slice(-10),
    currentStep: workflow.steps[stepIndex],
  };
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
