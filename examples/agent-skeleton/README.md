# @cellar/agent-skeleton

Reference skeleton showing how a non-MCP agent backend consumes
`@cellar/agent/runtime`.

The real backends — the in-process LangGraph driver, the built-in `runGoal`
loop, a future Mastra integration, the future in-house planner — follow this
same shape. They all depend on `@cellar/agent/runtime` and nothing else from
`@cellar/agent`, and they consume Cel + Cortex + the runtime kernel to
perceive, act, and verify.

## Boundary rule

This package may only import from `@cellar/agent/runtime`. Imports from the
bare `@cellar/agent` root would cross the agent-backend boundary and would be
caught by [`scripts/check-agent-boundary.mjs`](../../scripts/check-agent-boundary.mjs).

## Layout

- [`src/index.ts`](src/index.ts) — minimal perceive / verify demo
- This package is workspace-resolved (`workspace:*`) so the runtime subpath
  resolves locally without any registry round-trip.

## Run it

```bash
pnpm --filter @cellar/agent-skeleton build
pnpm --filter @cellar/agent-skeleton start
```

## Read it next to

- [`docs/adapters-cel-agents.md`](../../docs/adapters-cel-agents.md) § "Layer 3:
  Agents" — the documented boundary rule and the table of current agent
  backends as peers.
- [`agent/src/runtime-surface.ts`](../../agent/src/runtime-surface.ts) — the
  full runtime primitive surface this skeleton imports from.
