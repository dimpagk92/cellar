/**
 * Runtime Kernel — one canonical route→execute→verify pipeline (LEGACY FALLBACK).
 *
 * @deprecated The kernel's policies have been absorbed into the Rust goal-runner
 * (cel-goal-runner). This TS implementation is kept as a fallback for the TS
 * goal-runner path. New work should use the Rust runner, which owns:
 * - Route selection (strategy_router.rs)
 * - Verification (verification.rs)
 * - Escalation / terminal failure / refresh handling
 * - Event emission
 *
 * Execution dispatch now goes through the Cortex adapter system, not through
 * AdapterCapabilities directly. See docs/architecture.md.
 *
 * Original description:
 * This module owns the action-execution lifecycle:
 *   1. Consult the strategy router for route selection
 *   2. Execute via the adapter capability matching the chosen route
 *   3. Verify the outcome via context diff
 *   4. Record the attempt and ingest the outcome into cortex
 *   5. Decide: succeed, escalate, refresh, or terminal-fail
 *
 * License: MIT
 */

import { selectStrategyRoute } from "../strategy-router.js";
import type { StrategyAttempt } from "../strategy-router.js";
import { diffContexts, isDiffSignificant } from "../context-differ.js";
import type { ScreenContext, PlannedAction } from "../types.js";
import type {
  KernelExecutionInput,
  KernelActionOutcome,
  KernelEvent,
  VerificationResult,
} from "./types.js";

// ── Helpers ─────────────────────────────────────────────────────────────────

function normalizeText(value?: string | null): string {
  return (value ?? "").toLowerCase().replace(/\s+/g, " ").trim();
}

/**
 * Check whether a set_value action's value actually landed in the target element.
 * Pure function — operates only on context snapshots.
 */
function didSetValueLand(
  action: PlannedAction,
  before: ScreenContext,
  after: ScreenContext,
): boolean {
  if (action.type !== "set_value") return false;

  const targetId = (action as { target_id?: string }).target_id;
  if (!targetId) return false;

  const beforeEl = before.elements.find((el) => el.id === targetId);
  const afterEl = after.elements.find((el) => el.id === targetId);
  if (!afterEl) return false;

  const desired = normalizeText((action as { value?: string }).value);
  const beforeValue = normalizeText(beforeEl?.value ?? beforeEl?.label ?? "");
  const afterValue = normalizeText(afterEl.value ?? afterEl.label ?? "");
  const selectedText = normalizeText(
    String((afterEl.properties as Record<string, unknown>)?.selected_text ?? ""),
  );

  if (
    (afterValue.includes(desired) || selectedText.includes(desired)) &&
    afterValue !== beforeValue
  ) {
    return true;
  }

  return afterValue.includes(desired) || selectedText.includes(desired);
}

// ── Verification ────────────────────────────────────────────────────────────

/**
 * Verify an action outcome by diffing before/after contexts.
 *
 * This is the single canonical verification path. It checks:
 * - Context diff significance (elements added/removed/changed)
 * - set_value confirmation (value landed in target element)
 * - Cross-app/window shift detection
 */
export function verifyActionOutcome(
  action: PlannedAction,
  beforeContext: ScreenContext,
  afterContext: ScreenContext,
): VerificationResult {
  const diff = diffContexts(beforeContext, afterContext);
  const changedByDiff = isDiffSignificant(diff);
  const valueConfirmed = didSetValueLand(action, beforeContext, afterContext);
  const changed = changedByDiff || valueConfirmed;

  const crossAppShift =
    changed &&
    (afterContext.app !== beforeContext.app ||
      afterContext.window !== beforeContext.window);

  let sideEffectSummary: string | undefined;
  if (!changed) {
    sideEffectSummary = `No significant post-action diff for ${action.type}`;
  } else if (crossAppShift) {
    sideEffectSummary =
      `Action ${action.type} shifted context from ` +
      `${beforeContext.app}/${beforeContext.window} to ` +
      `${afterContext.app}/${afterContext.window}`;
  }

  return {
    changed,
    valueConfirmed,
    crossAppShift,
    sideEffectSummary,
  };
}

// ── Trusted Execution Check ─────────────────────────────────────────────────

/** Action types where adapter-reported success is trusted even without diff change. */
const TRUSTED_ACTION_TYPES = new Set(["click", "act", "set_value", "type"]);

function isTrustedExecution(action: PlannedAction, executed: boolean, changed: boolean): boolean {
  return executed && !changed && TRUSTED_ACTION_TYPES.has(action.type);
}

// ── Core Kernel ─────────────────────────────────────────────────────────────

/**
 * Execute a single planned action through the full route→execute→verify pipeline.
 *
 * This is the kernel's primary entry point. It replaces the `while (true)` loop
 * previously in callback-builder.ts executeAction, making the same policy
 * decisions but in an adapter-agnostic way.
 *
 * Returns a KernelActionOutcome with full execution metadata.
 */
export async function executePlannedAction(
  input: KernelExecutionInput,
): Promise<KernelActionOutcome> {
  const {
    action,
    capabilities,
    readFreshness,
    ingestOutcome,
    logRoute,
    onEvent,
    assessAmbiguity,
  } = input;

  const emit = (event: Omit<KernelEvent, "timestamp">) => {
    const full: KernelEvent = { ...event, timestamp: Date.now() };
    onEvent?.(full);
  };

  const startTime = Date.now();
  const routeAttempts: StrategyAttempt[] = input.attempts ?? [];
  let currentContext = input.context;
  let refreshTriggered = false;

  while (true) {
    // 1. Assess ambiguity (adapter-specific, optional)
    const ambiguity =
      assessAmbiguity?.(action, currentContext) ?? input.ambiguity ?? null;

    // 2. Select strategy route
    const selection = selectStrategyRoute({
      action,
      context: currentContext,
      freshness: readFreshness(),
      attempts: routeAttempts,
      ambiguity,
    });

    logRoute?.(`action=${action.type} route=${selection.route}`, {
      confidence: selection.confidence,
      reason: selection.reason,
      freshness: selection.freshness?.state ?? null,
      causes: selection.freshness?.causes ?? [],
      ambiguity: ambiguity?.preferredTargetId ?? null,
    });

    emit({
      type: "route_selected",
      action: action.type,
      route: selection.route,
      confidence: selection.confidence,
      reason: selection.reason,
      freshnessState: selection.freshness?.state ?? null,
      causes: selection.freshness?.causes,
      details: ambiguity?.preferredTargetId
        ? { preferredTargetId: ambiguity.preferredTargetId }
        : undefined,
    });

    // 3. Terminal failure — escalation ceiling reached
    if (selection.route === "terminal_failure") {
      emit({
        type: "terminal_failure",
        action: action.type,
        route: "terminal_failure",
        success: false,
        terminal: true,
        sideEffectSummary: "Escalation ceiling reached after vision fallback",
      });

      ingestOutcome({
        action: action.type,
        route: selection.route,
        success: false,
        verified: false,
        contradiction: true,
        sideEffectSummary: "Escalation ceiling reached after vision fallback",
      });

      return {
        action: action.type,
        route: "terminal_failure",
        success: false,
        verified: false,
        contradiction: true,
        sideEffectSummary: "Escalation ceiling reached after vision fallback",
        timestamp: Date.now(),
        durationMs: Date.now() - startTime,
        terminal: true,
        refreshTriggered,
        confidence: selection.confidence,
        routeAttempts: [...routeAttempts],
      };
    }

    // 4. Refresh — context is hard-stale
    if (selection.route === "refresh") {
      currentContext = await capabilities.readContext();
      refreshTriggered = true;
      emit({
        type: "refresh_triggered",
        action: action.type,
        route: "refresh",
        freshnessState: selection.freshness?.state ?? null,
        causes: selection.freshness?.causes,
      });
      continue;
    }

    // 5. Execute via the selected route
    try {
      let executed = false;
      let routeAction: PlannedAction = action;

      if (selection.route === "structured") {
        executed = await capabilities.executeStructured(action, currentContext);
      } else if (selection.route === "semantic") {
        // Use ambiguity-preferred target if available for click actions
        const resolved =
          ambiguity?.preferredTargetId && action.type === "click"
            ? ({ type: "click", target_id: ambiguity.preferredTargetId } as PlannedAction)
            : await capabilities.resolveSemantic(action, currentContext);

        if (!resolved) throw new Error("semantic resolution failed");
        routeAction = resolved;
        executed = await capabilities.executeStructured(resolved, currentContext);
      } else {
        // Vision route: screenshot + context diff
        await capabilities.captureScreenshot();
        const verifyContext = await capabilities.readContext();
        const diff = diffContexts(currentContext, verifyContext);
        const changed = isDiffSignificant(diff);

        routeAttempts.push({ route: "vision", success: changed, verified: changed });
        emit({
          type: "verification_result",
          action: action.type,
          route: "vision",
          success: changed,
          verified: changed,
          confidence: selection.confidence,
        });
        ingestOutcome({
          action: action.type,
          route: selection.route,
          success: changed,
          verified: changed,
          contradiction: !changed,
          sideEffectSummary: changed
            ? undefined
            : "Vision fallback could not verify action success",
        });

        if (changed) {
          return {
            action: action.type,
            route: "vision",
            success: true,
            verified: true,
            contradiction: false,
            timestamp: Date.now(),
            durationMs: Date.now() - startTime,
            terminal: false,
            refreshTriggered,
            confidence: selection.confidence,
            routeAttempts: [...routeAttempts],
          };
        }

        currentContext = verifyContext;
        continue;
      }

      // 6. Verify: read fresh context and diff against pre-action state
      const verifyContext = await capabilities.readContext();
      const verification = verifyActionOutcome(routeAction, currentContext, verifyContext);

      routeAttempts.push({
        route: selection.route,
        success: executed,
        verified: verification.changed,
      });

      emit({
        type: "verification_result",
        action: action.type,
        route: selection.route,
        success: executed,
        verified: verification.changed,
        confidence: selection.confidence,
        sideEffectSummary: verification.sideEffectSummary,
      });

      ingestOutcome({
        action: action.type,
        route: selection.route,
        success: executed,
        verified: verification.changed,
        contradiction: !verification.changed,
        sideEffectSummary: verification.sideEffectSummary,
      });

      if (verification.sideEffectSummary) {
        emit({
          type: "side_effect",
          action: routeAction.type,
          route: selection.route,
          sideEffectSummary: verification.sideEffectSummary,
          details: {
            fromApp: currentContext.app,
            toApp: verifyContext.app,
            crossAppShift: verification.crossAppShift,
          },
        });
        logRoute?.(`side-effect action=${routeAction.type}`, {
          route: selection.route,
          sideEffectSummary: verification.sideEffectSummary,
          fromApp: currentContext.app,
          toApp: verifyContext.app,
        });
      }

      // 7. Success: executed and verified
      if (executed && verification.changed) {
        // Post-navigate cleanup (e.g., dismiss cookie banners)
        if (
          action.type === "custom" &&
          (action as { action?: string }).action === "navigate" &&
          capabilities.postNavigateCleanup
        ) {
          try {
            await capabilities.postNavigateCleanup();
          } catch { /* best effort */ }
        }

        return {
          action: action.type,
          route: selection.route,
          success: true,
          verified: true,
          contradiction: false,
          sideEffectSummary: verification.sideEffectSummary,
          timestamp: Date.now(),
          durationMs: Date.now() - startTime,
          terminal: false,
          refreshTriggered,
          confidence: selection.confidence,
          routeAttempts: [...routeAttempts],
        };
      }

      // 8. Trust targeted execution: adapter confirmed success but diff didn't detect change
      if (isTrustedExecution(action, executed, verification.changed)) {
        emit({
          type: "trusted_execution",
          action: action.type,
          route: selection.route,
          success: true,
          verified: true,
          confidence: selection.confidence,
        });
        ingestOutcome({
          action: action.type,
          route: selection.route,
          success: true,
          verified: true,
          contradiction: false,
        });

        return {
          action: action.type,
          route: selection.route,
          success: true,
          verified: true,
          contradiction: false,
          timestamp: Date.now(),
          durationMs: Date.now() - startTime,
          terminal: false,
          refreshTriggered,
          confidence: selection.confidence,
          routeAttempts: [...routeAttempts],
        };
      }

      // Not verified — loop will escalate on next iteration
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      routeAttempts.push({
        route: selection.route,
        success: false,
        verified: false,
      });
      emit({
        type: "execution_result",
        action: action.type,
        route: selection.route,
        success: false,
        sideEffectSummary: errorMsg,
      });
      ingestOutcome({
        action: action.type,
        route: selection.route,
        success: false,
        verified: false,
        contradiction: false,
        sideEffectSummary: errorMsg,
      });
    }
  }
}
