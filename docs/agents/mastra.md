# Driving CEL from Mastra

This page shows how to drive CEL from a Mastra agent.

Read [docs/adapters-cel-agents.md](../adapters-cel-agents.md) first if you haven't.

## Purpose

Mastra is a TypeScript agent framework. Same ecosystem language as Cellar, easy mental model: a Mastra `Agent` has `tools`, an `instructions` prompt, and a `model`. To drive CEL, you expose `cel_see` and `cel_act` as two Mastra tools and let the agent loop.

In this setup:

- Mastra owns the model, tool loop, and stop condition.
- CEL provides perception, action execution, and adapter truth.
- You don't need `cel_think` — the Mastra agent is the planner.

Two integration paths:

1. **Via Mastra's MCP client.** Mastra supports MCP tools natively in recent versions; the agent speaks MCP to CEL as a subprocess and imports tools automatically.
2. **Direct tool-registration.** Wrap CEL calls as plain Mastra tools backed by the Node SDK (`@cellar/agent`) or raw child-process MCP calls.

Path 2 is shown below because it makes the ownership clearest. Path 1 is more convenient once Mastra's MCP client surface is stable for your version.

TODO: verify with the latest Mastra release notes at [https://mastra.ai/docs](https://mastra.ai/docs) — the exact MCP-client API has moved between minor versions.

## Setup

### 1. Build CEL and install Mastra in your project

```bash
# In your Mastra app
pnpm add @mastra/core @cellar/agent
# or: pnpm add @modelcontextprotocol/sdk  # if using raw MCP path
```

### 2. Set your LLM credentials

Mastra needs its own model provider credentials (separate from `CEL_LLM_*`, which only matters if you use `cel_think`):

```bash
export OPENAI_API_KEY=sk-...
# or ANTHROPIC_API_KEY, etc., per Mastra's provider docs
```

### 3. Pick the integration path

- **MCP client path:** register the CEL MCP server in Mastra's MCP config and let Mastra import tools. Consult the current Mastra docs for the exact `createMCPClient` / `MCPClient` constructor signature.
- **Direct tool-registration path:** call into `@cellar/agent` from a Mastra `createTool` factory. Shown below.

## Minimal Example (direct tool-registration)

A ~30-line Mastra agent that wires CEL as two tools and loops until "done":

```typescript
import { Agent } from "@mastra/core";
import { createTool } from "@mastra/core/tools";
import { Cel } from "@cellar/agent";
import { z } from "zod";

const cel = new Cel();

const celSee = createTool({
  id: "cel_see",
  description: "Read current screen state as structured JSON.",
  inputSchema: z.object({ mode: z.string().default("context") }),
  execute: async ({ context }) => cel.getContext(),
});

const celAct = createTool({
  id: "cel_act",
  description: "Execute one desktop action (click, type, ax_action, set_value, ...).",
  inputSchema: z.object({
    action: z.string(),
    element_id: z.string().optional(),
    value: z.string().optional(),
    x: z.number().optional(),
    y: z.number().optional(),
  }),
  execute: async ({ context }) => {
    // Dispatch to the appropriate SDK method based on action type.
    // See @cellar/agent docs and docs/mcp-server.md for full action list.
    return cel.performAction(context);
  },
});

export const desktopAgent = new Agent({
  name: "desktop-agent",
  instructions:
    "You drive a macOS desktop via CEL. Always call cel_see first, then cel_act. Stop when the goal is met or after 25 steps.",
  model: { provider: "openai", name: "gpt-4o" },
  tools: { celSee, celAct },
});
```

TODO: verify with the latest Mastra version — the `Agent` constructor shape, tool schema format (`zod` vs. `inputSchema` string), and loop / stop-condition API have been iterated on recently. Treat the above as pseudocode and reconcile against your installed `@mastra/core` version.

Then in an entry script:

```typescript
import { desktopAgent } from "./agent";

const result = await desktopAgent.generate({
  messages: [{ role: "user", content: "Open Numbers and write BTC in A1." }],
});
console.log(result.text);
```

## The `see -> act -> verify` Pattern (Mastra version)

Mastra's tool loop is identical to every other external agent:

```
loop:
  ctx = cel_see(mode="context")
  if goal_met(ctx): break
  step = mastra_agent_decides(ctx)
  receipt = cel_act(step)
  verify(receipt)
```

Same rules:

1. Re-observe between action batches.
2. Prefer structured actions (`ax_action`, `set_value`, `write_cells`) over coordinate guesses.
3. Batch up to ~4 actions per `cel_act` call.
4. Store `cel_act` receipts and pair them with post-action evidence.

## Tips

- **Stop conditions are yours to own.** Mastra's default loop will keep calling tools until the model stops requesting them. Set a `maxSteps` (or equivalent) so a misbehaving loop cannot run forever.
- **Use Mastra's MCP client once stable.** When your Mastra version supports MCP clients cleanly, switch from the direct-wiring path above to the MCP path — you get all four CEL tools for free and stay version-compatible with `docs/mcp-server.md`.
- **Confidence-aware branching.** Elements come with a confidence score. Make it a first-class input to the agent's decision: `>= 0.9` act, `0.7-0.9` act+verify, `< 0.7` escalate.
- **Persist state with your own store, not `cel_think`.** If the Mastra agent needs memory between runs, use Mastra's own storage abstractions rather than pulling `cel_think` `store_knowledge` into the loop.

## Known Gaps

- Mastra's MCP-client API surface is evolving quickly. The code block above is representative, not pinned — verify against your installed version before copying.
- `cel_perceive` is a singleton; if you run multiple Mastra agents concurrently they cannot each open their own Cortex session.
- Error shapes from `@cellar/agent` are stable, but error propagation through Mastra's tool loop depends on Mastra's internals — add your own wrapping if you need typed error branches.
- Adapter coverage is currently Numbers for spreadsheets; other apps fall back to generic AX.

## See Also

- [docs/agents/README.md](./README.md) — index of all agent cookbooks
- [docs/agents/mcp-client.md](./mcp-client.md) — the generic MCP path is always a fallback
- [docs/adapters-cel-agents.md](../adapters-cel-agents.md) — architecture
- [docs/mcp-server.md](../mcp-server.md) — full tool reference
- [docs/agent-integration-roadmap.md](../agent-integration-roadmap.md) — Mastra is P1
