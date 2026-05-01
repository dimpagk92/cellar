# Driving CEL from Claude Code

This page shows how to drive CEL from Claude Code.

Read [docs/adapters-cel-agents.md](../adapters-cel-agents.md) first if you haven't.

## Purpose

Claude Code is a strong default client for CEL because:

- it is MCP-native (stdio transport, same boundary CEL exposes)
- it already owns tool calling, retries, stop conditions, and user approval
- you do not need to bring your own LLM or planner wiring

In this setup Claude Code owns the loop, and CEL is just another MCP server providing `cel_see`, `cel_act`, and `cel_perceive`. `cel_think` is available but should usually stay unused here — Claude Code already plans.

## Setup

### 1. Build CEL

```bash
cd /path/to/cellar
pnpm install && pnpm -r build
```

### 2. Register CEL as an MCP server

Claude Code supports two config locations:

- User-wide: `~/.claude/settings.json`
- Project-scoped: `./.mcp.json` in the repo root

Project-scoped is recommended for CEL because permissions and registry state are machine-local.

Add CEL to the MCP section:

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

Equivalent CLI form (if the `cellar` binary is on your PATH):

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

### 3. Start Claude Code

Claude Code picks up MCP servers at start. In a new session, run:

```
/mcp
```

You should see `cel` listed with four tools: `cel_see`, `cel_act`, `cel_perceive`, `cel_think`.

TODO: screenshot — Claude Code `/mcp` output showing CEL registered.

### 4. Grant Accessibility permission

The first `cel_act` call will fail until macOS grants Accessibility permission to whichever process is launching Node (Terminal, iTerm, the Claude Code host, etc.). Grant it in `System Settings → Privacy & Security → Accessibility`, then restart Claude Code.

TODO: screenshot — macOS Accessibility panel with the host process toggled on.

## Minimal Example

Paste this into Claude Code's composer:

```
Use CEL to open Numbers, create a new blank sheet, and write these three
ticker symbols across A1, B1, C1: BTC, ETH, SOL. After you write them,
read the cells back with read_cells and confirm the values match.
```

What Claude Code will do internally:

1. Call `cel_see` with `mode: "context"` to see current screen state.
2. Call `cel_act` to open Numbers (e.g. via `key_combo` for Cmd+Space, then `type` the app name, then `key_press` Enter).
3. Call `cel_act` with `action: "write_cells"` targeting the Numbers adapter:
   ```json
   {
     "action": "write_cells",
     "app": "Numbers",
     "writes": [
       { "cell_ref": "A1", "value": "BTC" },
       { "cell_ref": "B1", "value": "ETH" },
       { "cell_ref": "C1", "value": "SOL" }
     ],
     "verify": true
   }
   ```
4. Call `cel_act` with `action: "read_cells"` for `["A1", "B1", "C1"]`.
5. Compare and report.

TODO: screenshot — Claude Code transcript showing the see → act → read_cells round-trip.

## The `see → act` Pattern

Claude Code's tool-calling loop maps cleanly onto CEL's canonical loop:

```
loop:
  ctx = cel_see(mode="context")
  if goal_met(ctx): break
  step = plan(ctx)              # Claude Code's own planning — no cel_think
  cel_act(step)                  # single action, or batch up to ~4
```

Two rules that keep this loop boring:

1. **Re-observe between action batches.** Element IDs are ephemeral; a stale snapshot is the main source of flaky agents.
2. **Prefer structured actions over coordinates.** `ax_action` beats `click(x,y)`; `set_value` beats typing; `write_cells` beats typing into Numbers cells one char at a time.

## Tips

- **Confidence tiers.** Elements in the fused context come with a confidence score. Claude Code can branch on it: `>= 0.9` act immediately; `0.7-0.9` act and re-verify; `< 0.7` ask the user.
- **Use `make_reference` for long-lived targets.** When a button needs to be re-clicked after intermediate state changes, create a resilient reference once with `cel_see` `make_reference` and re-use it.
- **Wait deterministically.** Prefer `cel_see` `wait_for_element` or `wait_for_idle` over `sleep`-then-retry. These are built for this.
- **Use `cel_perceive` for multi-phase tasks.** If the task spans more than ~10 actions across phases, bootstrap a Cortex session (`cel_perceive` `start`) so the model stays warm between steps. Otherwise just use `cel_see`.
- **Skip `cel_think` here.** Claude Code already plans better than the built-in planner in most cases. Reach for `cel_think` only when you want delegated autonomy.

## Known Gaps

- `cel_perceive` is a singleton. If Claude Code tries to open a second Cortex session, the second `start` will fail. Stop the previous one first.
- `cel_act` `write_cells` / `read_cells` currently only ship a Numbers backend. Other spreadsheet apps fall back to generic AX.
- Claude Code's MCP UI shows tools but does not currently surface per-tool schemas in-line; use [docs/mcp-server.md](../mcp-server.md) as the reference.
- Some CDP modes (`cdp_eval`, `cdp_page`, `cdp_status`) require `cellar setup cdp` and a Chrome instance launched with remote debugging.

## See Also

- [docs/agents/README.md](./README.md) — index of all agent cookbooks
- [docs/adapters-cel-agents.md](../adapters-cel-agents.md) — the three-layer architecture
- [docs/mcp-server.md](../mcp-server.md) — full tool reference
- [docs/agent-integration-roadmap.md](../agent-integration-roadmap.md) — Claude Code is P0
