/**
 * Logging Callbacks Decorator — wraps GoalRunnerCallbacks with per-run file logging.
 *
 * Adapter-agnostic: works with browser, Excel, SAP, native, or any callback set.
 * Persists step traces, failure screenshots, and a manifest.json on completion.
 *
 * Usage:
 *   const logged = withRunLogging(baseCallbacks, { runDir, goal });
 *   const result = await runGoal(cel, config, logged);
 */

import * as fs from "fs";
import * as path from "path";
import type { GoalRunnerCallbacks } from "./config.js";

export interface RunLoggingOptions {
  /** Directory to write logs (e.g. ~/.cellar/runs/{runId}/) */
  runDir: string;
  /** The goal being executed (for the manifest). */
  goal: string;
}

interface StepLogEntry {
  index: number;
  phase: string;
  action?: unknown;
  reasoning?: string;
  confidence?: number;
  success?: boolean;
  error?: string;
  ts: number;
}

/**
 * Wrap GoalRunnerCallbacks with per-run file logging.
 * Records step traces, captures failure screenshots, writes manifest.json on complete.
 */
export function withRunLogging(
  callbacks: GoalRunnerCallbacks,
  opts: RunLoggingOptions,
): GoalRunnerCallbacks {
  const { runDir, goal } = opts;
  const stepLog: StepLogEntry[] = [];
  const startTime = Date.now();

  // Ensure run directory exists
  try { fs.mkdirSync(runDir, { recursive: true }); } catch { /* best effort */ }

  return {
    // Pass through all base callbacks
    ...callbacks,

    onStepPlanned: (step: any, index: number) => {
      stepLog.push({
        index,
        phase: "planned",
        action: step.action,
        reasoning: step.reasoning,
        confidence: step.confidence,
        ts: Date.now(),
      });
      callbacks.onStepPlanned?.(step, index);
    },

    onStepExecuted: async (step: any, index: number, success: boolean, error?: string) => {
      stepLog.push({ index, phase: "executed", success, error, ts: Date.now() });

      // Capture screenshot on failure for post-mortem
      if (!success && callbacks.screenshot) {
        try {
          const img = await callbacks.screenshot();
          fs.writeFileSync(path.join(runDir, `step-${index}-fail.png`), img);
        } catch { /* best effort */ }
      }

      callbacks.onStepExecuted?.(step, index, success, error);
    },

    onComplete: (result: any) => {
      try {
        const manifest = {
          runId: path.basename(runDir),
          goal,
          status: result.status,
          summary: result.summary,
          totalSteps: result.totalSteps,
          metrics: result.metrics,
          steps: stepLog,
          startedAt: new Date(startTime).toISOString(),
          completedAt: new Date().toISOString(),
        };
        fs.writeFileSync(
          path.join(runDir, "manifest.json"),
          JSON.stringify(manifest, null, 2),
        );
      } catch { /* best effort */ }

      callbacks.onComplete?.(result);
    },
  };
}
