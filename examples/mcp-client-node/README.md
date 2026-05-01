# Raw MCP Client × CEL (Node)

Reference integration: a minimal Node script that connects to the CEL MCP server with `@modelcontextprotocol/sdk`, lists the tools, takes a screenshot, and reads the window list.

This is the P0 acceptance artifact for the "raw MCP client" entry in the [agent integration roadmap](../../docs/agent-integration-roadmap.md). The full cookbook is at [docs/agents/mcp-client.md](../../docs/agents/mcp-client.md).

## Why this exists

If your agent framework isn't in the cookbook, this example is the proof that CEL is genuinely agent-agnostic — anything that speaks MCP can drive it.

## Setup

```bash
pnpm install && pnpm -r build      # build CEL itself
cd examples/mcp-client-node
pnpm install                        # install MCP SDK
```

Set the path to your built CEL MCP server:

```bash
export CEL_MCP_SERVER=/absolute/path/to/cellar/mcp-server/dist/index.js
```

## Run

```bash
pnpm start
```

Expected output:

```
[cel-demo] connecting...
[cel-demo] connected. tools: cel_see, cel_act, cel_perceive, cel_think
[cel-demo] cel_see windows -> 3 visible windows
[cel-demo]   • Finder — Desktop
[cel-demo]   • Terminal — node
[cel-demo]   • Safari — Apple
[cel-demo] cel_see screenshot -> 248321 bytes
[cel-demo] disconnected.
```

## What this proves

- CEL works with an unbundled MCP client over stdio.
- No CEL-specific SDK, no framework lock-in. The same pattern works from Python (`mcp` package), Go, or any other MCP-capable runtime.

## Files

- [`run.ts`](./run.ts) — the runnable script (~70 lines, no abstractions)
- [`package.json`](./package.json) — MCP SDK dependency only
- [`tsconfig.json`](./tsconfig.json) — minimal TS config

## See also

- [docs/agents/mcp-client.md](../../docs/agents/mcp-client.md) — full cookbook (Node + Python)
- [docs/mcp-server.md](../../docs/mcp-server.md) — tool surface reference
