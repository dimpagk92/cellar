# Agent Integration Cookbooks

Date: April 24, 2026

This is the index for per-agent-runtime integration guides. CEL is the trust and execution layer; agents are clients. Each page below shows how to drive CEL from one specific agent runtime without making that runtime the platform identity.

If this is your first time, read [docs/adapters-cel-agents.md](../adapters-cel-agents.md) before picking a runtime.

## The Boundary Rule (read once, then stop worrying about it)

- **The agent owns:** planning, tool selection, retries, branching, checkpointing, approvals, stop conditions.
- **CEL owns:** fused context, screenshots, action execution, adapter dispatch, results, receipts, verification surfaces.
- **Adapters own:** app-specific structured truth (Numbers cells, Figma nodes, Slides shapes, etc.).

For any external agent, the preferred tool boundary is small: `cel_see` + `cel_act`, occasionally `cel_perceive`. Reaching for `cel_think` from an external agent is usually an anti-pattern — `cel_think` exists for built-in autonomous flows and for agents that explicitly want CEL to take over the loop.

The preferred external-agent loop is:

```text
healthcheck -> observe -> act -> verify -> cite receipt/evidence
```

See [docs/mcp-server.md](../mcp-server.md) for the full tool surface.

## Supported Agent Runtimes

| Runtime | Integration Style | Status | Cookbook |
|---|---|---|---|
| LangGraph | Node SDK / custom tool wrapping | Reference integration (shipped) | [../langgraph-rust-sidecar.md](../langgraph-rust-sidecar.md) |
| Claude Code | MCP native (stdio) | Cookbook (this doc set) | [claude-code.md](./claude-code.md) |
| Cursor | MCP native (stdio) | Cookbook (this doc set) | [cursor.md](./cursor.md) |
| Codex CLI | MCP native (stdio) | Cookbook (this doc set) | [codex.md](./codex.md) |
| Mastra | MCP client or direct tool wrapping | Cookbook (this doc set) | [mastra.md](./mastra.md) |
| n8n | CLI via execute-command; HTTP via worker (planned) | Cookbook (this doc set); worker HTTP path tracked in [docs/worker-protocol.md](../worker-protocol.md) | [n8n.md](./n8n.md) |
| Raw MCP client | `@modelcontextprotocol/sdk` (Node) or `mcp` (Python) | Reference integration (this doc set) | [mcp-client.md](./mcp-client.md) |
| Gemini CLI / GPT tool callers | Via raw MCP client pattern | Planned — no bespoke cookbook yet | Use [mcp-client.md](./mcp-client.md) |

Integration priority and acceptance bar per tier: see [docs/agent-integration-roadmap.md](../agent-integration-roadmap.md).

## Starting the MCP Server

Every MCP-native cookbook below assumes this block somewhere in the client's MCP config:

```json
{
  "mcpServers": {
    "cel": {
      "command": "node",
      "args": ["/path/to/cellar/mcp-server/dist/index.js"]
    }
  }
}
```

Or equivalently via CLI: `cellar mcp` (stdio).

Environment variables that affect the MCP server:

| Variable | Purpose |
|---|---|
| `CEL_LLM_PROVIDER` | `openai`, `anthropic`, `gemini`, `ollama`, `compatible` |
| `CEL_LLM_API_KEY` | API key for the LLM provider (only needed if the agent uses `cel_think`) |
| `CEL_LLM_MODEL` | Model name |
| `CEL_LLM_ENDPOINT` | Custom endpoint |

For external agents that bring their own LLM (Claude Code, Cursor, Codex, Mastra, etc.), `CEL_LLM_*` is not required unless you also call `cel_think`.

## Tool Surface Recap

CEL exposes four MCP tools. External-agent cookbooks in this folder focus on the first two.

| Tool | Modes | External-agent recommendation |
|---|---|---|
| `cel_see` | 14 modes (context, screenshot, windows, focused, element_at, is_settable, make_reference, cursor_position, cdp_status, cdp_page, wait_for_element, wait_for_idle, watch, monitors) | Use freely |
| `cel_act` | single-action and batched (click, right_click, double_click, mouse_move, type, key_press, key_combo, scroll, drag, ax_action, set_value, cdp_eval, navigate, write_cells, read_cells, adapter_action) | Use freely; cite receipts and verify effects |
| `cel_perceive` | 7 modes (start, read, feed, checkpoint, configure, status, stop) | Use for multi-step tasks where continuous awareness matters |
| `cel_think` | 17 modes (run_goal, plan, plan_with_vision, search_knowledge, store_knowledge, memory_get, memory_set, observe, get_observations, run_start, run_finish, run_log_step, run_history, run_steps, llm_complete, llm_complete_with_image, eviction) | Generally avoid from external agents. Use only when you explicitly want CEL to own the loop |

## See Also

- [docs/adapters-cel-agents.md](../adapters-cel-agents.md) — the three-layer architecture
- [docs/trust-execution-layer.md](../trust-execution-layer.md) — the trust loop and receipt contract
- [docs/mcp-server.md](../mcp-server.md) — full tool reference
- [docs/langgraph-rust-sidecar.md](../langgraph-rust-sidecar.md) — the original reference integration
- [docs/agent-integration-roadmap.md](../agent-integration-roadmap.md) — which runtimes to prioritize next
- [docs/worker-protocol.md](../worker-protocol.md) — the HTTP path (Phase 1, for future n8n / remote integrations)
