# Driving CEL from Cursor

This page shows how to drive CEL from Cursor.

Read [docs/adapters-cel-agents.md](../adapters-cel-agents.md) first if you haven't.

## Purpose

Cursor supports MCP servers from its composer / chat runtime, which makes CEL usable directly from the editor without any bespoke glue. In this setup:

- Cursor owns planning, tool selection, retries, and approval prompts.
- CEL exposes `cel_see`, `cel_act`, `cel_perceive`, `cel_think` over stdio MCP.
- You normally only need `cel_see` + `cel_act` from Cursor. Leave `cel_think` alone unless you explicitly want delegated autonomy.

## Setup

### 1. Build CEL

```bash
cd /path/to/cellar
pnpm install && pnpm -r build
```

### 2. Register CEL as an MCP server

Cursor reads MCP servers from its global settings and/or project settings. The most reliable location is the global MCP config:

- macOS: `~/.cursor/mcp.json`

TODO: verify with latest Cursor version — Cursor has iterated on MCP config paths. If `~/.cursor/mcp.json` does not exist for you, check `Cursor → Settings → MCP` (the UI now exposes a server editor in recent builds). The stdio config block itself is stable across versions.

Add CEL:

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

Or, if `cellar` is on your PATH:

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

### 3. Confirm tools appear

Restart Cursor. Open `Settings → MCP` (or the equivalent panel). You should see `cel` listed with its four tools: `cel_see`, `cel_act`, `cel_perceive`, `cel_think`.

TODO: screenshot — Cursor Settings panel with CEL tools listed.

In the composer, enabling the CEL tools in the tool picker makes them callable from the current session.

### 4. Grant macOS Accessibility permission

The first call to `cel_act` that performs input will fail until the host process (Cursor, or the Node process Cursor spawns) has Accessibility permission. Grant it in `System Settings → Privacy & Security → Accessibility` and restart Cursor.

TODO: screenshot — macOS Accessibility list with the Cursor-spawned Node process enabled.

## Minimal Example

Open Cursor's composer and paste:

```
Use CEL to open the Calculator app, compute 128 * 7, and tell me the
result. Before acting, call cel_see with mode=context to confirm what's
on screen. Prefer ax_action over coordinate clicks.
```

What Cursor will do internally:

1. `cel_see` `mode: "context"` — list elements on the current desktop.
2. `cel_act` `key_combo: ["Cmd", "Space"]` to open Spotlight.
3. `cel_act` `type: "Calculator"` → `key_press: "Enter"`.
4. `cel_see` again — get new Calculator element IDs.
5. `cel_act` (batched) — press `1`, `2`, `8`, `*`, `7`, `=` via `ax_action` on each button.
6. `cel_see` — read the displayed result.
7. Return the answer in chat.

TODO: screenshot — Cursor composer showing the sequence of CEL tool calls.

## The `see -> act -> verify` Pattern (Cursor version)

Cursor's tool loop is a tight read-plan-act loop driven by the model:

```
loop:
  ctx = cel_see(mode="context")
  if goal_met(ctx): break
  step = cursor_model_decides_next_tool(ctx)
  receipt = cel_act(step)
  verify(receipt)
```

Same rules as any other external-agent cookbook:

1. **Re-observe between action batches.** Element IDs are ephemeral.
2. **Prefer structured actions.** `ax_action` > `click(x,y)`, `set_value` > `type`, `write_cells` > typing into Numbers cells.
3. **Batch up to ~4 actions**, then re-observe.
4. **Record receipts.** A receipt proves dispatch. Verify the user-facing result with readback, CDP/AX state, screenshot, or Cortex diff.

## Tips

- **Scope tools per-project.** If a project does not need desktop automation, leave the CEL server out of the project's MCP whitelist so it is not a distraction for the model.
- **Use `wait_for_element`** instead of sleeping when Cursor's composer is asked to wait for app state.
- **Watch Cursor's approval prompts.** Cursor may ask the user to approve each tool call by default. For long sequences, consider enabling auto-approve for `cel_see` (read-only) but keep approval on for `cel_act` during development.
- **Cursor chats are ephemeral.** If you need persistent memory across sessions, use `cel_think` `store_knowledge` / `search_knowledge` — but that pulls CEL into the loop beyond the minimal surface.

## Known Gaps

- MCP UX in Cursor is evolving. Paths, UI labels, and approval semantics may differ from the above; when in doubt verify with the Cursor docs in your current version.
- `cel_perceive` is singleton — only one Cortex session at a time across all MCP clients.
- CDP-backed modes (`cdp_eval`, `cdp_page`) need `cellar setup cdp` and a Chrome launched with remote debugging.
- Cursor does not currently surface MCP server logs in a first-class way; for debugging, run the server manually (`cellar mcp`) in a separate terminal and pipe stderr.

## See Also

- [docs/agents/README.md](./README.md) — index of all agent cookbooks
- [docs/agents/claude-code.md](./claude-code.md) — sibling MCP-native cookbook
- [docs/adapters-cel-agents.md](../adapters-cel-agents.md) — architecture
- [docs/mcp-server.md](../mcp-server.md) — full tool reference
- [docs/agent-integration-roadmap.md](../agent-integration-roadmap.md) — Cursor is P1
