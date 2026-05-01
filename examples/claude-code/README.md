# Claude Code × CEL

Drive CEL from [Claude Code](https://claude.com/claude-code) via MCP. Claude owns planning, retries, and the agent loop; CEL provides perception, execution, and adapter-backed truth.

This example is the P0 acceptance artifact for the [agent integration roadmap](../../docs/agent-integration-roadmap.md). The full cookbook is at [docs/agents/claude-code.md](../../docs/agents/claude-code.md).

## Setup

1. Build CEL:

   ```bash
   pnpm install && pnpm -r build
   ```

2. Add the server to Claude Code. Pick one:

   **Project scope** — create `.mcp.json` in your project root:

   ```json
   {
     "mcpServers": {
       "cel": {
         "command": "node",
         "args": ["/absolute/path/to/cellar/mcp-server/dist/index.js"]
       }
     }
   }
   ```

   **Global scope** — add to `~/.claude/settings.json` under `mcpServers`.

3. Verify Claude Code sees the tools:

   ```
   /mcp
   ```

   You should see `cel_see`, `cel_act`, `cel_perceive`, and (optionally) `cel_think`.

## Run the demo

Open Claude Code in this directory and paste the contents of [`goal.md`](./goal.md) into the prompt. Claude will use `cel_see` to inspect Numbers and `cel_act` `write_cells` to populate cells deterministically.

Expected outcome: cells `A1`, `B1`, `C1` of the active Numbers sheet contain `BTC`, `ETH`, `SOL`.

## What this proves

- CEL works with an off-the-shelf MCP client without bespoke glue.
- The `see → act` loop is driven entirely by Claude — CEL has no opinion about planning.
- The `write_cells` action returns deterministic confirmation (no AX guessing).

## Troubleshooting

- **No tools listed**: confirm the path in `.mcp.json` is absolute and the build succeeded (`mcp-server/dist/index.js` must exist).
- **Permission errors on first run**: macOS will prompt for Accessibility and Screen Recording. Grant them to the process running Claude Code.
- **Numbers not foreground**: open Numbers manually before running the goal. CEL won't launch apps autonomously in this demo.

## See also

- [docs/agents/claude-code.md](../../docs/agents/claude-code.md) — full cookbook
- [docs/mcp-server.md](../../docs/mcp-server.md) — tool surface reference
- [docs/adapters-cel-agents.md](../../docs/adapters-cel-agents.md) — three-layer north star
