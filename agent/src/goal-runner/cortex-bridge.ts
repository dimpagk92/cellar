/**
 * CortexBridge — translates the Cortex's MentalModel into actionable signals
 * for the planning loop.
 *
 * Supports both:
 * - Rust Cortex (via Cel NAPI bindings) — preferred, always-on
 * - TypeScript Cortex (legacy fallback)
 *
 * The bridge auto-detects which Cortex is available and reads from it.
 */

import type { Cortex } from "../cortex.js";
import type { InputController } from "../interfaces/input-controller.js";
import type { Anomaly, ElementStability, FreshnessAssessment, DiffSummary, ActionOutcome } from "../types.js";
import { findDismissableDialog, type DismissableDialog } from "../dialog-dismisser.js";
import { normalizeCortexAnomalies, normalizeCortexModel } from "../cortex-normalize.js";

// ─── Types ──────────────────────────────────────────────────────────────────

export type CortexSignalType =
  | "dialog"
  | "loading_stall"
  | "error_persisting"
  | "spinner"
  | "idle"
  | "app_switch";

export interface CortexSignal {
  type: CortexSignalType;
  description: string;
  elementIds?: string[];
  /** True = must handle before continuing (dialogs, persistent errors). */
  actionRequired: boolean;
}

/** Minimal interface for reading from either Rust or TS Cortex. */
interface CortexReader {
  isRunning(): boolean;
  readModel(): any;
  consumeAnomalies(): Anomaly[];
  readFreshness(): FreshnessAssessment | null;
  readDiffSummary(): DiffSummary | null;
  ingestActionOutcome(outcome: ActionOutcome): void;
}

/** Adapter: wraps a TS Cortex to satisfy CortexReader. */
function tsCortexReader(cortex: Cortex): CortexReader {
  return {
    isRunning: () => cortex.isRunning(),
    readModel: () => cortex.model,
    consumeAnomalies: () => cortex.consumeAnomalies(),
    readFreshness: () => cortex.readFreshness(),
    readDiffSummary: () => cortex.readDiffSummary(),
    ingestActionOutcome: (outcome) => cortex.ingestActionOutcome(outcome),
  };
}

/** Adapter: wraps a Cel instance (Rust Cortex via NAPI) to satisfy CortexReader. */
function rustCortexReader(cel: any): CortexReader {
  return {
    isRunning: () => cel.isCortexRunning?.() ?? false,
    readModel: () => normalizeCortexModel(cel.readCortexModel?.()),
    consumeAnomalies: () => normalizeCortexAnomalies(cel.consumeCortexAnomalies?.()),
    readFreshness: () => {
      try {
        const model = normalizeCortexModel(cel.readCortexModel?.());
        return model?.freshness ?? null;
      } catch {
        return null;
      }
    },
    readDiffSummary: () => {
      try {
        const model = normalizeCortexModel(cel.readCortexModel?.());
        return model?.lastDiffSummary ?? null;
      } catch {
        return null;
      }
    },
    ingestActionOutcome: (outcome) => {
      if (outcome.success) cel.reportCortexActionSuccess?.();
      else cel.reportCortexActionFailure?.();
    },
  };
}

// ─── Constants ──────────────────────────────────────────────────────────────

const LOADING_STALL_MS = 5000;
const ERROR_PERSIST_MS = 3000;
const DEFAULT_SETTLE_TIMEOUT_MS = 5000;
const SETTLE_POLL_MS = 200;

// ─── CortexBridge ───────────────────────────────────────────────────────────

export class CortexBridge {
  private reader: CortexReader;

  /**
   * Create a CortexBridge.
   * @param cortexOrCel — either a TS Cortex instance or a Cel instance with Rust Cortex NAPI bindings
   * @param cel — InputController for auto-dismiss actions
   */
  constructor(
    cortexOrCel: Cortex | { isCortexRunning(): boolean },
    private cel: InputController,
  ) {
    // Detect which type of cortex we have
    if ("isCortexRunning" in cortexOrCel && typeof (cortexOrCel as any).readCortexModel === "function") {
      // Rust Cortex via Cel NAPI
      this.reader = rustCortexReader(cortexOrCel);
    } else {
      // TypeScript Cortex
      this.reader = tsCortexReader(cortexOrCel as Cortex);
    }
  }

  /**
   * Poll for actionable signals from the cortex.
   * Called at the top of each step in the planning loop.
   */
  poll(): CortexSignal[] {
    if (!this.reader.isRunning()) return [];

    const signals: CortexSignal[] = [];
    const model = this.reader.readModel();
    if (!model) return [];

    // Normalize field names (Rust uses snake_case, TS uses camelCase)
    const temporal = model.temporal ?? {};
    const loading = temporal.loading;
    const errorPersisting = temporal.errorPersisting ?? temporal.error_persisting;
    const idleSince = temporal.idleSince ?? temporal.idle_since;

    // 1. Drain anomaly queue → convert to signals
    const anomalies = this.reader.consumeAnomalies();
    for (const anomaly of anomalies) {
      const aType = anomaly.type ?? (anomaly as any).anomaly_type;
      const actionRequired = aType === "dialog" || aType === "auth_prompt";
      signals.push({
        type: aType === "app_switch" ? "app_switch" : "dialog",
        description: anomaly.description,
        elementIds: anomaly.elementIds ?? (anomaly as any).element_ids,
        actionRequired,
      });
    }

    // 2. Loading stall
    const loadingDuration = loading?.durationMs ?? loading?.duration_ms ?? 0;
    if (loading?.detected && loadingDuration > LOADING_STALL_MS) {
      signals.push({
        type: "loading_stall",
        description: `Loading for ${Math.round(loadingDuration / 1000)}s`,
        actionRequired: false,
      });
    }

    // 3. Persistent error
    const errorDuration = errorPersisting?.durationMs ?? errorPersisting?.duration_ms ?? 0;
    if (errorPersisting?.detected && errorDuration > ERROR_PERSIST_MS) {
      signals.push({
        type: "error_persisting",
        description: `Error persisting: "${errorPersisting.message ?? "unknown"}"`,
        actionRequired: true,
      });
    }

    // 4. Idle detection
    if (idleSince !== null && idleSince !== undefined) {
      const idleMs = Date.now() - idleSince;
      if (idleMs > 1000) {
        signals.push({
          type: "idle",
          description: `Page idle for ${Math.round(idleMs / 1000)}s`,
          actionRequired: false,
        });
      }
    }

    return signals;
  }

  /**
   * Handle interrupts before planning. Auto-dismisses dialogs and waits for loading.
   */
  async handleInterrupts(): Promise<string[]> {
    if (!this.reader.isRunning()) return [];

    const handled: string[] = [];
    const model = this.reader.readModel();
    if (!model) return [];

    // 1. Auto-dismiss dialogs
    const ctx = model.currentContext;
    if (ctx) {
      const dialog = findDismissableDialog(ctx);
      if (dialog) {
        try {
          const el = ctx.elements?.find((e: any) => e.id === dialog.elementId);
          if (el?.bounds) {
            const cx = el.bounds.x + Math.floor(el.bounds.width / 2);
            const cy = el.bounds.y + Math.floor(el.bounds.height / 2);
            this.cel.click(cx, cy);
            handled.push(`Dismissed ${dialog.dialogType}: "${dialog.label}"`);
            await this.sleep(500);
          }
        } catch {
          handled.push(`Failed to dismiss dialog`);
        }
      }
    }

    // 2. Wait for loading
    const temporal = model.temporal ?? {};
    const loading = temporal.loading;
    if (loading?.detected) {
      await this.waitForSettle(3000);
      handled.push("Waited for page loading to complete");
    }

    return handled;
  }

  /** Whether the page is settled. */
  isSettled(): boolean {
    if (!this.reader.isRunning()) return true;
    const model = this.reader.readModel();
    if (!model) return true;

    const temporal = model.temporal ?? {};
    const idleSince = temporal.idleSince ?? temporal.idle_since;
    const stagnantCycles = temporal.stagnantCycles ?? temporal.stagnant_cycles ?? 0;

    return (
      idleSince !== null && idleSince !== undefined &&
      !(temporal.loading?.detected) &&
      stagnantCycles >= 2
    );
  }

  /** Wait for the page to settle. */
  async waitForSettle(timeoutMs: number = DEFAULT_SETTLE_TIMEOUT_MS): Promise<void> {
    if (!this.reader.isRunning()) return;
    const start = Date.now();
    while (Date.now() - start < timeoutMs) {
      if (this.isSettled()) return;
      await this.sleep(SETTLE_POLL_MS);
    }
  }

  /** Get element stability classification. */
  getStability(): ElementStability | null {
    if (!this.reader.isRunning()) return null;
    const model = this.reader.readModel();
    return model?.stability ?? null;
  }

  /** Format cortex signals as text for prompt injection. */
  getPromptSignals(signals: CortexSignal[]): string {
    const informational = signals.filter(s => !s.actionRequired);
    if (informational.length === 0) return "";
    return informational.map(s => `CORTEX: ${s.description}`).join("\n");
  }

  /** Whether cortex vision is needed. */
  isVisionNeeded(): boolean {
    if (!this.reader.isRunning()) return false;
    const model = this.reader.readModel();
    return model?.visionNeeded ?? false;
  }

  readFreshness(): FreshnessAssessment | null {
    if (!this.reader.isRunning()) return null;
    return this.reader.readFreshness();
  }

  readDiffSummary(): DiffSummary | null {
    if (!this.reader.isRunning()) return null;
    return this.reader.readDiffSummary();
  }

  ingestActionOutcome(outcome: ActionOutcome): void {
    if (!this.reader.isRunning()) return;
    this.reader.ingestActionOutcome(outcome);
  }

  private sleep(ms: number): Promise<void> {
    return new Promise(resolve => setTimeout(resolve, ms));
  }
}
