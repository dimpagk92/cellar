# Agent Architecture

> **Note (April 2026):** The primary execution path is now the **full Rust goal-runner** (`cel-goal-runner`). The TS agent layer described below is kept for backward compatibility and the benchmark pipeline. See `docs/architecture.md` for the current Rust-first architecture: Cortex (I/O + adapters) → Goal Runner (Rust) → Planner (Rust).

The agent layer is the TypeScript orchestration engine that sits on top of the Rust CEL core. It handles planning, execution, perception, and persistence.

## Interface-Based Composition

Instead of depending on the monolithic `Cel` class, each module depends on the narrowest interface it needs:

```
ContextProvider     ← Screen reading (getContext, listMonitors, etc.)
InputController     ← Mouse/keyboard/a11y actions (click, type, etc.)
Planner             ← LLM step planning (planStep, llmComplete, etc.)
KnowledgeStore      ← Persistence (knowledge, runs, observations, memory)
BrowserBridge       ← CDP operations (cdpEvaluate, cdpNavigate, etc.)
EventSource         ← Watchdog events (startWatchdog, pollEvents)
```

The `Cel` class implements all 6 interfaces. Pass the full `Cel` where needed, but accept interfaces in function signatures.

### Import Pattern

```typescript
// Consumer module — depends on interface, not Cel
import type { Planner } from "../interfaces/planner.js";

export async function planStep(cel: Planner, goal: string, ...): Promise<PlannedStep> {
  return await cel.planStep(goal, context, history);
}

// Caller — passes Cel (which implements Planner)
import { Cel } from "./cel-bindings.js";
const cel = new Cel();
await planStep(cel, "Click submit", ...);
```

### Dependency Map

| Module | Interfaces Used |
|--------|----------------|
| `goal-runner/planner.ts` | `Planner` |
| `goal-runner/history-advisor.ts` | `KnowledgeStore` |
| `goal-runner/cortex-bridge.ts` | `InputController` |
| `action-executor.ts` | `InputController` + `ContextProvider` (resolveReference) |
| `cortex.ts` | `ContextProvider` + `EventSource` |
| `self-healer.ts` | `Planner` |
| `constraint-extractor.ts` | `Planner` |
| `context-assembly.ts` | `KnowledgeStore` |
| `device-baseline.ts` | `ContextProvider` (listMonitors) |
| `post-run.ts` | `KnowledgeStore` |
| `cdp-extractor.ts` | `BrowserBridge` + `Planner` (llmComplete) |
| `perception-socket.ts` | `ContextProvider` + `EventSource` + `Planner` |
| `orchestrator.ts` | Full `Cel` (passes to runGoal) |
| `goal-runner.ts` | Full `Cel` (orchestrates all phases) |

## Testing with Mock Factories

```typescript
import { createMockPlanner, sampleContext } from "../test-utils/index.js";

const planner = createMockPlanner({
  steps: [
    { reasoning: "Click submit", action: { type: "click", target: "a11y:1" }, confidence: 0.9 },
  ],
});

// No native module needed — pure TypeScript mock
const step = await planStep(planner, "Submit the form", sampleContext(), [], null, 10, false, callbacks);
expect(planner.calls.planStep).toHaveLength(1);
```

Available mock factories in `test-utils/`:
- `createMockContextProvider()` — returns canned ScreenContext
- `createMockInputController()` — records calls, returns success
- `createMockPlanner()` — returns scripted PlannedStep sequences
- `createMockKnowledgeStore()` — in-memory persistence

## Cortex (Perception Engine)

The Cortex maintains a continuously-updated mental model via a background loop. Multiple instances can run concurrently (no longer a singleton).

```typescript
const cortex1 = new Cortex(cel, { id: "desktop" });
const cortex2 = new Cortex(cel, { id: "browser", getContext: browserAdapter.getContext });

await cortex1.boot();
await cortex2.boot();

// Query active instances
getActiveCortexIds();  // ["desktop", "browser"]
getCortexById("browser");  // cortex2
```

## Key Files

| File | Purpose |
|------|---------|
| `interfaces/` | 6 composable interfaces + CelComposite type |
| `cel-bindings.ts` | Cel class (implements all interfaces) + napi bridge |
| `goal-runner.ts` | Universal observe-plan-act loop |
| `cortex.ts` | Always-on perception engine |
| `action-executor.ts` | Maps actions to native input calls |
| `test-utils/` | Type-safe mock factories |

## Package Exports

```typescript
import { Cel } from "@cellar/agent";                    // Full Cel class
import type { Planner } from "@cellar/agent/interfaces"; // Individual interface
import { createMockPlanner } from "@cellar/agent/test-utils"; // Test mocks
import { Cortex } from "@cellar/agent/cortex";           // Cortex class
```
