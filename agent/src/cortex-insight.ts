import type {
  ContextElement,
  FreshnessAssessment,
  MentalModel,
  ScreenContext,
  SemanticInsight,
  SourceSummary,
} from "./types.js";

const SOFT_STALE_MS = 1500;
const HARD_STALE_MS = 5000;
const SOFT_STALE_CONFIDENCE = 0.75;
const HARD_STALE_CONFIDENCE = 0.4;

function findElement(ctx: ScreenContext | undefined, id: string | undefined): ContextElement | undefined {
  if (!ctx || !id) return undefined;
  return ctx.elements.find((el) => el.id === id);
}

function normalizeSource(source: string | undefined): keyof Omit<SourceSummary, "adapterBacked"> {
  switch (source) {
    case "native_api":
      return "nativeApi";
    case "vision":
      return "vision";
    case "merged":
      return "merged";
    default:
      return "accessibility";
  }
}

function firstActionableLabel(ctx: ScreenContext | undefined): string | null {
  if (!ctx) return null;
  const element = ctx.elements.find(
    (el) => el.state?.enabled && el.state?.visible && (el.actions?.length ?? 0) > 0 && el.label,
  );
  return element?.label ?? null;
}

function normalizeAgeMs(model: Partial<MentalModel>, now: number): number {
  const explicitAge = typeof model.ageMs === "number" && Number.isFinite(model.ageMs) ? model.ageMs : null;
  const timestampMs = model.currentContext?.timestamp_ms;
  if (explicitAge !== null && explicitAge > 0) return explicitAge;
  if (typeof timestampMs === "number" && Number.isFinite(timestampMs) && timestampMs > 0) {
    return Math.max(0, now - timestampMs);
  }
  return explicitAge ?? 0;
}

export function deriveFreshnessAssessment(
  model: Partial<MentalModel>,
  now = Date.now(),
): FreshnessAssessment {
  const existing = model.freshness;
  const ageMs = normalizeAgeMs(model, now);
  const confidence = typeof model.confidence === "number" ? model.confidence : existing?.confidence ?? 0;
  const lastUpdateMs = existing?.lastUpdateMs
    ?? (typeof model.currentContext?.timestamp_ms === "number" ? model.currentContext.timestamp_ms : now - ageMs);
  const lastEventMs = existing?.lastEventMs ?? null;
  const lastSignificantEventMs = existing?.lastSignificantEventMs ?? null;
  const causes = new Set(existing?.causes ?? []);
  let state: FreshnessAssessment["state"] = existing?.state ?? "fresh";

  if (lastSignificantEventMs !== null && lastSignificantEventMs >= lastUpdateMs) {
    causes.add("event");
    state = "hard-stale";
  }
  if (ageMs >= HARD_STALE_MS) {
    causes.add("time");
    state = "hard-stale";
  } else if (ageMs >= SOFT_STALE_MS && state !== "hard-stale") {
    causes.add("time");
    state = "soft-stale";
  }
  if (confidence <= HARD_STALE_CONFIDENCE) {
    causes.add("confidence");
    state = "hard-stale";
  } else if (confidence <= SOFT_STALE_CONFIDENCE) {
    causes.add("confidence");
    if (state === "fresh") {
      state = "soft-stale";
    }
  }

  return {
    state,
    causes: [...causes],
    ageMs,
    confidence,
    lastUpdateMs,
    lastEventMs,
    lastSignificantEventMs,
  };
}

export function deriveSourceSummary(model: Partial<MentalModel>): SourceSummary {
  const summary: SourceSummary = {
    accessibility: 0,
    nativeApi: 0,
    vision: 0,
    merged: 0,
    adapterBacked: Object.keys(model.elementAdapterIndex ?? {}).length,
  };

  for (const element of model.currentContext?.elements ?? []) {
    summary[normalizeSource(element.source)] += 1;
  }

  return summary;
}

export function deriveSemanticInsight(model: Partial<MentalModel>): SemanticInsight {
  const ctx = model.currentContext;
  const focused = model.focusedElement ? findElement(ctx, model.focusedElement.id) : undefined;
  const firstAnomaly = model.anomalyQueue?.[0];
  const loading = model.temporal?.loading;
  const errorPersisting = model.temporal?.errorPersisting;
  const focusTrail = model.temporal?.focusTrail ?? [];
  const lastDiff = model.lastDiffSummary;
  const actionableLabel = firstActionableLabel(ctx);

  let taskPhase: SemanticInsight["taskPhase"] = "review";
  if (loading?.detected) {
    taskPhase = "loading";
  } else if (firstAnomaly || errorPersisting?.detected) {
    taskPhase = "blocked";
  } else if (focused && ["input", "textarea", "textfield", "searchfield", "combobox", "select"].includes(focused.element_type)) {
    taskPhase = "input";
  } else if (model.temporal?.idleSince !== null && model.temporal?.idleSince !== undefined) {
    taskPhase = "idle";
  } else if ((lastDiff?.addedCount ?? 0) + (lastDiff?.removedCount ?? 0) + (lastDiff?.changedCount ?? 0) > 0) {
    taskPhase = "navigation";
  }

  const currentActivityParts = [
    ctx?.app ? `Using ${ctx.app}` : "Reading the current device state",
    ctx?.window && ctx.window !== ctx.app ? `in ${ctx.window}` : null,
    model.focusedElement?.label ? `focused on ${model.focusedElement.label}` : null,
  ].filter(Boolean);

  let recentTransition: string | null = null;
  if (firstAnomaly?.type === "app_switch") {
    recentTransition = firstAnomaly.description;
  } else if (focusTrail.length >= 2) {
    const previous = focusTrail.at(-2);
    const current = focusTrail.at(-1);
    if (previous && current && previous !== current) {
      recentTransition = `Focus moved from ${previous} to ${current}.`;
    }
  } else if (lastDiff) {
    const changedTotal = lastDiff.addedCount + lastDiff.removedCount + lastDiff.changedCount;
    if (changedTotal > 0) {
      recentTransition = `Context changed (+${lastDiff.addedCount} / -${lastDiff.removedCount} / ~${lastDiff.changedCount}).`;
    }
  }

  let likelyBlocker: string | null = null;
  if (firstAnomaly?.description) {
    likelyBlocker = firstAnomaly.description;
  } else if (errorPersisting?.detected) {
    likelyBlocker = errorPersisting.message
      ? `Persistent error: ${errorPersisting.message}`
      : "A persistent error is still visible.";
  } else if (loading?.detected && loading.durationMs >= 1500) {
    likelyBlocker = `The UI is still loading (${Math.round(loading.durationMs / 1000)}s).`;
  } else if (model.visionNeeded) {
    likelyBlocker = "Structured streams are still sparse; a richer read may be needed.";
  }

  let suggestedNextStep: string | null = null;
  if (firstAnomaly?.type === "dialog" || firstAnomaly?.type === "auth_prompt") {
    suggestedNextStep = "Handle the blocking dialog or prompt before continuing.";
  } else if (errorPersisting?.detected) {
    suggestedNextStep = "Acknowledge the error, then retry or backtrack.";
  } else if (loading?.detected) {
    suggestedNextStep = "Wait for the UI to settle before taking the next action.";
  } else if (focused?.label && ["input", "textarea", "textfield", "searchfield", "combobox", "select"].includes(focused.element_type)) {
    suggestedNextStep = `Continue entering or selecting data in "${focused.label}".`;
  } else if (actionableLabel) {
    suggestedNextStep = `Inspect or use "${actionableLabel}" if it matches the goal.`;
  } else if (model.visionNeeded) {
    suggestedNextStep = "Refresh context or fall back to vision for a denser read.";
  } else {
    suggestedNextStep = "Inspect the current screen and choose the next actionable control.";
  }

  return {
    currentActivity: currentActivityParts.join(" "),
    recentTransition,
    likelyBlocker,
    suggestedNextStep,
    taskPhase,
  };
}

export function enrichMentalModel<T extends Partial<MentalModel>>(model: T, now = Date.now()): T {
  const mutable = model as T & Partial<MentalModel>;
  mutable.ageMs = normalizeAgeMs(mutable, now);
  mutable.freshness = deriveFreshnessAssessment(mutable, now);

  if (!mutable.lastDiffSummary && Array.isArray(mutable.recentDiffs) && mutable.recentDiffs.length > 0) {
    const lastDiff = mutable.recentDiffs.at(-1);
    if (lastDiff) {
      mutable.lastDiffSummary = {
        addedCount: lastDiff.addedCount,
        removedCount: lastDiff.removedCount,
        changedCount: lastDiff.changedCount,
        unchangedCount: lastDiff.unchangedCount,
      };
    }
  }

  mutable.sourceSummary = deriveSourceSummary(mutable);
  mutable.semantic = deriveSemanticInsight(mutable);
  return model;
}
