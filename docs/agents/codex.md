# Driving CEL from OpenAI Codex CLI

This page shows how to drive CEL from the OpenAI Codex CLI.

Read [docs/adapters-cel-agents.md](../adapters-cel-agents.md) first if you haven't.

## Purpose

Codex CLI is OpenAI's terminal-resident agent. It calls tools, loops, and stops on its own, which is exactly the external-agent pattern CEL is designed for. In this setup:

- Codex owns the loop and tool-calling.
- CEL provides `cel_see`, `cel_act`, `cel_perceive`, `cel_think` via MCP stdio.
- You normally only need `cel_see` + `cel_act`.

## Setup

### 1. Build CEL

```bash
cd /path/to/cellar
pnpm install && pnpm -r build
```

### 2. Register CEL with Codex

Codex exposes MCP servers via a config file in the user's home directory.

TODO: verify with latest Codex CLI docs at [https://platform.openai.com/docs/codex](https://platform.openai.com/docs/codex) (or the Codex GitHub repo README) — the exact path and key name have changed between Codex releases. As of the most recent public guidance the config lives under `~/.codex/` and MCP servers follow the standard `mcpServers` schema shared across MCP clients.

The MCP server block itself is stable:

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

Or via the CLI entry point:

```json
{
  "mcpServers": {
    "cel": {
      "command": "cellar",
      "args": ["mcp"]
    }
  }
}
```

### 3. Verify Codex sees CEL

Start Codex and list tools. The exact command varies by version; check `codex --help` or `codex mcp list` in your build.

TODO: verify with upstream docs — Codex version in April 2026 should support listing attached MCP servers. If your version does not, run the MCP Inspector against CEL directly to confirm the server is healthy:

```bash
npx @modelcontextprotocol/inspector node /path/to/cellar/mcp-server/dist/index.js
```

### 4. Grant macOS Accessibility permission

As with every CEL client, the first `cel_act` call that performs input requires Accessibility permission for the host process. Grant it in `System Settings → Privacy & Security → Accessibility` and restart Codex.

## Minimal Example

Run Codex and ask (flag it as a task that uses CEL):

```
Use the cel tools (cel_see, cel_act) to open Safari, navigate to
example.com, and return the page title. Use cel_see context to check
state between actions.
```

What Codex will do internally:

1. Call `cel_see` `mode: "context"` — observe the desktop.
2. Call `cel_act` to launch Safari (Spotlight or Dock).
3. Call `cel_see` again to find the URL bar.
4. Call `cel_act` `ax_action` on the URL field, then `set_value` to `"example.com"`, then `key_press: "Enter"`.
5. Call `cel_see` `cdp_page` if CDP is configured, else `context`, to extract the page title.
6. Return the title.

## The `see → act` Pattern (Codex version)

Same canonical loop every external agent uses with CEL:

```
loop:
  ctx = cel_see(mode="context")
  if goal_met(ctx): break
  step = codex_model_decides_next_tool(ctx)
  cel_act(step)
```

Rules:

1. Re-observe between batches — element IDs are ephemeral.
2. Prefer `ax_action` and `set_value` over coordinates and typing.
3. Batch up to ~4 actions per `cel_act` call, then re-observe.

## Tips

- **Structured output.** Codex benefits from explicit JSON-shaped prompts. When asking it to report the result of a CEL run, request a machine-readable return (e.g. `Return {"title": "...", "ok": true}`) so downstream shell pipelines can parse it.
- **Non-interactive mode.** Codex CLI often runs headless inside CI or shells. Make sure whatever process spawns Codex has Accessibility permission — not just your terminal.
- **Approval policy.** Codex's tool-call approval behavior differs from Claude Code's and Cursor's. For one-shot scripts, disable approval only for read-only tools (`cel_see`) and keep approvals on for `cel_act` during development.
- **No `cel_think` needed.** Codex is already a planner. Reach for `cel_think` only if you want to delegate long-horizon autonomy to CEL's built-in loop.

## Known Gaps

- Codex CLI config paths and flag names have been unstable across versions. Treat the configuration block above as canonical and the CLI invocation specifics as subject to version drift — consult the official Codex docs for the exact commands in your release.
- Codex does not currently expose MCP server logs in-line; for debugging use the MCP Inspector against CEL.
- `cel_perceive` is singleton; only one Cortex session can exist across all MCP clients.
- CDP modes require `cellar setup cdp` and a Chrome instance launched with remote debugging.

## See Also

- [docs/agents/README.md](./README.md) — index of all agent cookbooks
- [docs/agents/mcp-client.md](./mcp-client.md) — raw MCP reference if Codex config drifts
- [docs/adapters-cel-agents.md](../adapters-cel-agents.md) — architecture
- [docs/mcp-server.md](../mcp-server.md) — full tool reference
- [docs/agent-integration-roadmap.md](../agent-integration-roadmap.md) — Codex is P2
