# Stage 3 — physical package split

Stages 1 and 2 enforced the agent-backend boundary via the subpath export
`@cellar/agent/runtime` and a lint guard. Stage 3 is the physical split: turn
that subpath into a standalone package so the runtime can be depended on
without dragging the built-in planner along.

**Status: not executed.** This document is the migration plan. Execute when one
of these triggers fires:

- A new agent backend (Mastra, in-house planner, etc.) wants to depend on
  `@cellar/runtime` without pulling LangGraph and runGoal into its dep graph.
- The runtime needs to be published as a standalone npm package while the
  built-in planner stays internal.
- Build times or dep-graph weight become a real friction.

Until then, the subpath gives ~90% of the clarity benefit at ~5% of the cost.

## Target shape

| Package                    | Source today                                              | Contains                                                                                |
| -------------------------- | --------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| `@cellar/runtime`          | `agent/src/runtime-surface.ts` + everything it re-exports | Cel + interfaces, Cortex, runtime kernel, CDP helpers, context utils, types, log/config |
| `@cellar/agent-langgraph`  | `agent/src/langgraph/`                                    | The LangGraph driver, planner, react agent, graph state                                 |
| `@cellar/agent-builtin`    | The rest of `agent/src/*`                                 | `WorkflowEngine`, `runGoal`, `orchestrator`, `self-healer`, `strategy-router`, `replay`, `validator`, `failure-recovery`, `vision-router`, caches, `workflow-io`, `post-run`, `constraint-extractor`, `cua-provider` |
| `@cellar/agent` (optional) | meta package                                              | Thin back-compat shim re-exporting from the three above (deprecate over time)           |

## File ownership (concrete moves)

Move from `agent/src/` to `runtime/src/`:

```
cel-bindings.ts            interfaces/
cortex.ts                  perception-socket.ts       cortex-normalize.ts        cortex-insight.ts
runtime/                   action-executor.ts
cdp-browser.ts             cdp-extractor.ts
context-assembly.ts        context-differ.ts          context-serializer.ts      context-compressor.ts
message-compaction.ts      transcript.ts
sensitive-data.ts          skeleton-detector.ts       url-shortener.ts           paginated-extractor.ts
config.ts                  logger.ts                  device-baseline.ts
types.ts                   types.test.ts
*.test.ts for the above
runtime-surface.ts → becomes src/index.ts of the new package
```

Move from `agent/src/` to `agent-langgraph/src/`:

```
langgraph/                 (entire directory + tests)
```

Move from `agent/src/` to `agent-builtin/src/`:

```
engine.ts                  queue.ts
goal-runner.ts             goal-runner/               (validator, failure-recovery, vision-router, logging-callbacks)
orchestrator.ts
self-healer.ts             strategy-router.ts         strategy-tracker.ts
replay/                    post-run.ts
constraint-extractor.ts    cache/                     cua-provider.ts
workflow-io.ts
run-goal.ts                run-goal-langgraph.ts      numbers-smoke.ts            (these live in cli/ today)
```

Tests follow their source files. Cross-package imports become explicit:
`agent-langgraph` and `agent-builtin` depend on `@cellar/runtime` via
`workspace:*`. `agent-langgraph` likely also pulls LangGraph deps directly
rather than transitively.

## Consumer migration

Already on the boundary (single-line change per file):

| Consumer                           | Today                                | After Stage 3                                                       |
| ---------------------------------- | ------------------------------------ | ------------------------------------------------------------------- |
| `mcp-server/src/**`                | `@cellar/agent/runtime`              | `@cellar/runtime`                                                   |
| `cli/src/commands/browser.ts`      | `@cellar/agent/runtime`              | `@cellar/runtime`                                                   |
| `examples/agent-skeleton/src/**`   | `@cellar/agent/runtime`              | `@cellar/runtime`                                                   |

Backend-side consumers (real refactor needed, but only one package per file):

| Consumer                                                      | Today           | After Stage 3                                       |
| ------------------------------------------------------------- | --------------- | --------------------------------------------------- |
| `cli/src/commands/run.ts`                                     | `@cellar/agent` | `@cellar/runtime` + `@cellar/agent-builtin`         |
| `cli/src/commands/run-goal.ts`                                | `@cellar/agent` | `@cellar/runtime` + `@cellar/agent-builtin`         |
| `cli/src/commands/run-goal-langgraph.ts`                      | `@cellar/agent` | `@cellar/runtime` + `@cellar/agent-langgraph`       |
| `cli/src/commands/workflow.ts`                                | `@cellar/agent` | `@cellar/agent-builtin`                             |
| `cli/src/commands/{action,capture,context,history,memory,setup,status,train,numbers-smoke}.ts` | `@cellar/agent` | mostly `@cellar/runtime`, a few `@cellar/agent-builtin` |
| `recorder/src/**`                                             | `@cellar/agent` | `@cellar/runtime`                                   |
| `live-view/src/**`                                            | `@cellar/agent` | `@cellar/runtime`                                   |
| `adapters/browser/src/**`                                     | `@cellar/agent` | `@cellar/runtime`                                   |

## Execution order

1. Create the three new package skeletons (package.json, tsconfig, README) under e.g. `packages/runtime`, `packages/agent-langgraph`, `packages/agent-builtin`.
2. Add to `pnpm-workspace.yaml`.
3. Move runtime files first. Get `@cellar/runtime` building in isolation. Update its tests.
4. Move LangGraph files. Get `@cellar/agent-langgraph` building. Update tests.
5. Move remaining built-in files. Get `@cellar/agent-builtin` building. Update tests.
6. Make the old `agent/` directory either:
   - Empty (delete after consumers migrate), or
   - A thin meta-package whose `src/index.ts` re-exports from the three. This keeps `@cellar/agent` working as a back-compat alias.
7. Migrate consumers in dependency order: leaf packages first (`adapters/browser`, `recorder`, `live-view`), then `mcp-server`, then `cli`, then `examples/agent-skeleton`.
8. Update `scripts/check-agent-boundary.mjs`:
   - `FORBIDDEN_IMPORT` becomes `@cellar/agent-builtin` and `@cellar/agent-langgraph` (any backend-side package). `@cellar/agent` only if the meta-package is kept.
   - `AGENT_BACKEND_PACKAGES` stays the same.
9. Update `docs/adapters-cel-agents.md` Layer-3 section to reference the new package names.
10. Run `pnpm -r build && pnpm -r test && pnpm lint` end-to-end. Verify nothing went sideways.

## Things to watch out for

- **Circular imports:** Today's `agent/` package has internal couplings (e.g. `goal-runner.ts` imports cortex/runtime helpers via relative paths). After splitting, those become `@cellar/runtime` imports. If anything in `@cellar/runtime` currently reaches *into* the planner files, the split surfaces it as a cycle. Resolve by either moving that file or inverting the dependency.
- **Test colocation:** Vitest configs and `tsconfig` `include` rules need to follow each file move.
- **Type re-exports:** All public types currently live in `agent/src/types.ts` and are re-exported from `runtime-surface.ts`. After split, `types.ts` belongs to `@cellar/runtime`. Anything currently doing `import { ... } from "@cellar/agent/types"` should switch to `@cellar/runtime`.
- **`@cellar/agent` subpaths:** `./runtime`, `./cortex`, `./interfaces`, `./types`, `./test-utils` all collapse to root-level exports of the new packages. The meta-package (if kept) should preserve at least `./runtime` for one release to ease the transition.
- **`cli` is a mixed-mode consumer:** Some CLI commands need both `@cellar/runtime` (Cel + helpers) and `@cellar/agent-builtin` (WorkflowEngine + saveWorkflow). Split the imports per file.

## Rough cost estimate

- Mechanical file moves: 1–2 hours
- Resolving any surfaced internal couplings / circular deps: highly variable, 1–4 hours
- Consumer migration: 1–2 hours (boundary already enforced means it's mostly find-and-replace)
- Test + CI verification: 30 min – 1 hour

Plan budget: half a day of focused work for a clean operator; more if hidden cycles surface.
