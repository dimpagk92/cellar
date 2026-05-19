/**
 * Skeleton agent backend — reference for non-MCP agent backends consuming
 * @cellar/agent/runtime.
 *
 * Real backends (the in-process LangGraph driver, the built-in runGoal loop,
 * future Mastra integration, the future in-house planner) follow this shape:
 * they depend on @cellar/agent/runtime — and nothing else from @cellar/agent —
 * and consume Cel + Cortex + the runtime kernel to perceive, act, and verify.
 *
 * Boundary rule (enforced by scripts/check-agent-boundary.mjs):
 *   This package may import ONLY from "@cellar/agent/runtime".
 *   Importing from the bare "@cellar/agent" root would cross the boundary
 *   and fail the lint check.
 */

import {
  Cel,
  type AdapterCapabilities,
  type CelComposite,
  type KernelActionOutcome,
  type PlannedAction,
  type ScreenContext,
  diffContexts,
  executePlannedAction,
  isCortexActive,
  isDiffSignificant,
  log,
} from "@cellar/agent/runtime";

/** Read the current fused screen context via Cel. */
function perceive(cel: CelComposite): ScreenContext {
  return cel.getContext();
}

/**
 * Execute a planned action through the runtime kernel. Real backends wire
 * `AdapterCapabilities` to a concrete adapter (the browser adapter, the AX
 * adapter, …); this skeleton uses no-op stubs so the kernel API surface is
 * visible without taking a real action.
 *
 * Exported but intentionally NOT called from main() — running the skeleton
 * should be safe to invoke without surprising side effects on the screen.
 */
export async function act(
  cel: CelComposite,
  action: PlannedAction,
  context: ScreenContext,
): Promise<KernelActionOutcome> {
  const capabilities: AdapterCapabilities = {
    readContext: async () => cel.getContext(),
    executeStructured: async () => false,
    resolveSemantic: async () => null,
    captureScreenshot: async () => Buffer.from([]),
  };
  return executePlannedAction({
    action,
    context,
    capabilities,
    readFreshness: () => null,
    ingestOutcome: () => {},
  });
}

/**
 * Compare two perceived contexts to check whether the screen meaningfully
 * changed. Real backends would use this to verify the effect of an action.
 */
function verify(before: ScreenContext, after: ScreenContext): boolean {
  return isDiffSignificant(diffContexts(before, after));
}

async function main(): Promise<void> {
  const cel = new Cel();

  log.info("skeleton-agent: starting", { cortexActive: isCortexActive() });

  const before = perceive(cel);
  log.info("skeleton-agent: perceived", {
    app: before.app,
    elementCount: before.elements.length,
  });

  // No-op pause to demonstrate the diff path a real backend would use after
  // an act() call to verify outcomes. The kernel surface is exported via
  // act() above; not invoked here to keep skeleton runs free of side effects.
  await new Promise((resolve) => setTimeout(resolve, 100));

  const after = perceive(cel);
  log.info("skeleton-agent: change detected", { changed: verify(before, after) });
}

main().catch((err) => {
  console.error("skeleton-agent failed:", err);
  process.exit(1);
});
