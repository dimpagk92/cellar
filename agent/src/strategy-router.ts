import type { FreshnessAssessment, PlannedAction, ScreenContext } from "./types.js";

export type StrategyRoute = "structured" | "semantic" | "vision" | "refresh" | "terminal_failure";

export interface StrategyAttempt {
  route: Exclude<StrategyRoute, "refresh" | "terminal_failure">;
  success: boolean;
  verified?: boolean;
}

export interface StrategySelection {
  route: StrategyRoute;
  confidence: number;
  reason: string;
  terminal: boolean;
  freshness: FreshnessAssessment | null;
}

export interface AmbiguityAssessment {
  ambiguous: boolean;
  confidence: number;
  reason: string;
  preferredTargetId?: string;
}

export interface StrategyRouterInput {
  action: PlannedAction;
  context: ScreenContext;
  freshness: FreshnessAssessment | null;
  attempts?: StrategyAttempt[];
  ambiguity?: AmbiguityAssessment | null;
}

const SEMANTIC_CAPABLE_ACTIONS = new Set<PlannedAction["type"]>([
  "click",
  "type",
  "set_value",
  "act",
]);

function isSemanticCapable(action: PlannedAction): boolean {
  return SEMANTIC_CAPABLE_ACTIONS.has(action.type);
}

function isVisionCapable(action: PlannedAction): boolean {
  return isSemanticCapable(action) || action.type === "done" || action.type === "extract";
}

/** Max retries for non-escalatable actions (key, scroll, key_combo, drag) before terminal failure. */
const MAX_NON_ESCALATABLE_ATTEMPTS = 3;

export function selectStrategyRoute(input: StrategyRouterInput): StrategySelection {
  const attempts = input.attempts ?? [];
  const attemptedRoutes = attempts.map((attempt) => attempt.route);

  // Terminal ceiling: vision already tried
  if (attemptedRoutes.includes("vision")) {
    return {
      route: "terminal_failure",
      confidence: 0,
      reason: "Vision route already attempted and failed verification",
      terminal: true,
      freshness: input.freshness,
    };
  }

  // Terminal ceiling for non-escalatable actions: after N structured attempts,
  // stop instead of looping forever. Actions like key, scroll, key_combo
  // can't escalate to semantic/vision, so they need a retry limit.
  if (!isSemanticCapable(input.action) && attempts.length >= MAX_NON_ESCALATABLE_ATTEMPTS) {
    return {
      route: "terminal_failure",
      confidence: 0,
      reason: `Non-escalatable action "${input.action.type}" failed ${attempts.length} times without verification`,
      terminal: true,
      freshness: input.freshness,
    };
  }

  if (input.freshness?.state === "hard-stale") {
    return {
      route: "refresh",
      confidence: input.freshness.confidence,
      reason: `Model is hard-stale (${input.freshness.causes.join(", ") || "unknown"})`,
      terminal: false,
      freshness: input.freshness,
    };
  }

  if (input.ambiguity?.ambiguous && isSemanticCapable(input.action) && attempts.length === 0) {
    return {
      route: "semantic",
      confidence: input.ambiguity.confidence,
      reason: input.ambiguity.reason,
      terminal: false,
      freshness: input.freshness,
    };
  }

  if (attemptedRoutes.includes("semantic") && isVisionCapable(input.action)) {
    return {
      route: "vision",
      confidence: 0.35,
      reason: "Structured/semantic execution could not verify the action",
      terminal: false,
      freshness: input.freshness,
    };
  }

  if (attemptedRoutes.includes("structured") && isSemanticCapable(input.action)) {
    return {
      route: "semantic",
      confidence: 0.55,
      reason: "Structured execution was insufficient; escalate to semantic resolution",
      terminal: false,
      freshness: input.freshness,
    };
  }

  if (input.freshness?.state === "soft-stale" && isSemanticCapable(input.action)) {
    return {
      route: "semantic",
      confidence: Math.max(0.45, input.freshness.confidence),
      reason: `Model is soft-stale (${input.freshness.causes.join(", ") || "unknown"}); prefer semantic resolution`,
      terminal: false,
      freshness: input.freshness,
    };
  }

  return {
    route: "structured",
    confidence: input.freshness?.confidence ?? 0.9,
    reason: "Grounded structured execution is preferred",
    terminal: false,
    freshness: input.freshness,
  };
}
