# Driving CEL from a Raw MCP Client

This page shows how to drive CEL from a framework-less MCP client using the standard MCP SDKs.

Read [docs/adapters-cel-agents.md](../adapters-cel-agents.md) first if you haven't.

## Purpose

This is the reference integration. It proves the claim that CEL is agent-agnostic: any MCP-speaking process can drive CEL with no bespoke glue. Use this page when:

- You are writing a custom agent and want to call CEL tools directly.
- You are debugging whether a problem is in your agent framework or in CEL.
- You want a minimal, copy-pasteable example for a new language binding.
- You are bringing up a framework that is not in the cookbook set (Gemini CLI, GPT custom tool callers, bespoke in-house runtimes, etc.).

In this setup:

- Your code owns the LLM, the loop, and the stop condition.
- CEL provides `cel_see`, `cel_act`, `cel_perceive`, `cel_think` over stdio MCP.
- You call tools using the standard MCP client API for your language.

## Setup

### 1. Build CEL

```bash
cd /path/to/cellar
pnpm install && pnpm -r build
```

### 2. Install an MCP SDK

**Node.js:**

```bash
pnpm add @modelcontextprotocol/sdk
```

**Python:**

```bash
pip install mcp
```

### 3. Grant macOS Accessibility permission

The process that spawns CEL (your script) will need Accessibility permission in `System Settings → Privacy & Security → Accessibility`. Easiest is to grant it to the terminal / Node / Python binary used to run your script.

## Minimal Example (Node.js, ~40 lines)

A minimal client that spawns CEL, lists tools, and executes one task: open Numbers, write "BTC" into A1, read it back.

```typescript
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

const transport = new StdioClientTransport({
  command: "node",
  args: ["/absolute/path/to/cellar/mcp-server/dist/index.js"],
});

const client = new Client({ name: "cel-reference-client", version: "0.1.0" }, { capabilities: {} });
await client.connect(transport);

// 1. Confirm CEL's tools are exposed.
const tools = await client.listTools();
console.log("CEL tools:", tools.tools.map((t) => t.name));
// → cel_see, cel_act, cel_perceive, cel_think

// 2. See the current screen.
const ctx = await client.callTool({
  name: "cel_see",
  arguments: { mode: "context" },
});
console.log("Elements:", JSON.parse(ctx.content[0].text).elements.length);

// 3. Write a value into Numbers A1 via the Numbers adapter.
await client.callTool({
  name: "cel_act",
  arguments: {
    action: "write_cells",
    app: "Numbers",
    writes: [{ cell_ref: "A1", value: "BTC" }],
    verify: true,
  },
});

// 4. Read it back deterministically.
const read = await client.callTool({
  name: "cel_act",
  arguments: { action: "read_cells", app: "Numbers", cell_refs: ["A1"] },
});
console.log("A1 =", read.content[0].text);

await client.close();
```

That is a complete, framework-less driver. Everything else — loops, planners, retry policy, stop conditions — is yours to layer on top.

## Minimal Example (Python sketch)

```python
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client
import asyncio, json

async def main():
    params = StdioServerParameters(
        command="node",
        args=["/absolute/path/to/cellar/mcp-server/dist/index.js"],
    )
    async with stdio_client(params) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()

            tools = await session.list_tools()
            print("CEL tools:", [t.name for t in tools.tools])

            ctx = await session.call_tool("cel_see", {"mode": "context"})
            print("Elements:", len(json.loads(ctx.content[0].text)["elements"]))

            await session.call_tool("cel_act", {
                "action": "write_cells",
                "app": "Numbers",
                "writes": [{"cell_ref": "A1", "value": "BTC"}],
                "verify": True,
            })

asyncio.run(main())
```

TODO: verify with the current `mcp` Python SDK — the exact class names have shifted across releases. The shape is stable.

## The `see → act` Pattern (raw version)

With a raw client, the canonical agent loop is yours to write. The simplest version:

```typescript
for (let step = 0; step < MAX_STEPS; step++) {
  const ctxResp = await client.callTool({ name: "cel_see", arguments: { mode: "context" } });
  const ctx = JSON.parse(ctxResp.content[0].text);

  if (goalMet(ctx)) break;

  const action = yourPlanner(ctx); // LLM call, rules engine, whatever
  await client.callTool({ name: "cel_act", arguments: action });
}
```

Rules (every external-agent cookbook shares these):

1. **Re-observe between action batches.** Element IDs are ephemeral.
2. **Prefer structured actions.** `ax_action` > `click(x,y)`, `set_value` > `type`, `write_cells` > typing into cells.
3. **Batch up to ~4 actions per call to `cel_act`**, then re-observe.

## Tips

- **Tool list is your contract.** Always call `listTools()` at least once during development to confirm your build of CEL matches [docs/mcp-server.md](../mcp-server.md).
- **Error envelope.** CEL returns typed errors in the MCP `content` array. Parse `isError` on the response to distinguish transport-level failures from tool-level failures.
- **Confidence-aware branching.** Elements carry confidence (0.0-1.0). Branch on it the same way every other CEL agent does.
- **Stay on the small surface.** Unless you are building delegated autonomy, only use `cel_see` and `cel_act`. `cel_perceive` is for multi-phase tasks; `cel_think` is for CEL-owned loops.

## Known Gaps

- The Python SDK has evolved faster than the Node SDK. Treat the Python snippet above as a sketch and reconcile with your installed version.
- `cel_perceive` is a singleton — concurrent clients cannot each open their own Cortex session.
- `cel_act` `write_cells` / `read_cells` only ship a Numbers backend today.
- CDP modes require `cellar setup cdp` and Chrome launched with remote debugging.

## See Also

- [docs/agents/README.md](./README.md) — index of all agent cookbooks
- [docs/adapters-cel-agents.md](../adapters-cel-agents.md) — architecture
- [docs/mcp-server.md](../mcp-server.md) — full tool reference
- [docs/agent-integration-roadmap.md](../agent-integration-roadmap.md) — raw MCP client is P0 (reference integration)
