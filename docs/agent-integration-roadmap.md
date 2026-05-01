# Agent Integration Roadmap

Date: April 24, 2026

Status: **proposal** — the ranking below is a recommendation, not a commitment. User may re-rank; treat changes to this file as first-class.

## Framing

CEL's durable value is in device understanding, execution, and adapter truth — not in owning a planner. That means the roadmap question is not "which planner do we build?" but "which agent runtimes should work well against CEL, and in what order?"

The north-star boundary stays the same across every runtime:

- **Agent owns:** planning, retries, branching, checkpointing, approvals, stop conditions.
- **CEL owns:** fused context, screenshots, action execution, adapter dispatch.
- **Adapters own:** app-specific truth.

See [docs/adapters-cel-agents.md](./adapters-cel-agents.md).

Integration docs live under [docs/agents/](./agents/README.md).

## Priority Tiers & Acceptance Bars

| Tier | Meaning | Acceptance Bar |
|---|---|---|
| **P0** | Must-have for v0.2 agent-agnostic claim | Working cookbook + runnable example in `examples/` |
| **P1** | High value next | Cookbook + one eval scenario under `eval/scenarios/` that exercises the runtime |
| **P2** | Nice to have | Cookbook only |
| **P3** | Covered transitively | No dedicated doc — user routes through the raw MCP client cookbook |

## Ranked List

### Already shipped

- **LangGraph** — Reference integration. Cookbook: [`docs/langgraph-rust-sidecar.md`](./langgraph-rust-sidecar.md). Validated the boundary before the cookbook set existed. No change planned.

### P0 — ship next

- **Claude Code** — Cookbook: [`docs/agents/claude-code.md`](./agents/claude-code.md).
  - Rationale: highest adoption of any MCP client in April 2026; a working Claude Code path is the biggest reach win and the lowest-friction demo for new users.
  - Acceptance: cookbook (done), plus `examples/claude-code/` containing the Numbers `BTC/ETH/SOL` task so a user can copy-paste and watch it run end-to-end.

- **Raw MCP client reference** — Cookbook: [`docs/agents/mcp-client.md`](./agents/mcp-client.md).
  - Rationale: proves the agent-agnostic claim. Low effort. Also serves as the fallback path for any runtime we don't cover explicitly.
  - Acceptance: cookbook (done), plus `examples/mcp-client-node/` (and optionally `examples/mcp-client-python/`) implementing the minimal example verbatim.

### P1 — ship after P0 lands

- **Mastra** — Cookbook: [`docs/agents/mastra.md`](./agents/mastra.md).
  - Rationale: TypeScript-first, same ecosystem as Cellar, shortest path from "look at the CEL SDK" to "I have a running agent."
  - Acceptance: cookbook (done), one eval scenario wiring a Mastra agent against a `BrowserGym`/`Numbers` task under `eval/scenarios/`. Pinned against a specific Mastra version to avoid surface drift.

- **Cursor** — Cookbook: [`docs/agents/cursor.md`](./agents/cursor.md).
  - Rationale: Cursor's MCP usage is growing fast among developers; good overlap with our target early-adopter audience.
  - Acceptance: cookbook (done), one eval scenario that runs via Cursor's composer against a canned task. Screenshot assets captured and checked in.

### P2 — ship when bandwidth permits

- **Codex CLI** — Cookbook: [`docs/agents/codex.md`](./agents/codex.md).
  - Rationale: OpenAI's official terminal agent. Adoption is growing but smaller than Claude Code / Cursor today. Version instability on the Codex side makes a pinned example risky until Codex settles its MCP config.
  - Acceptance: cookbook only. No example in `examples/` until Codex MCP config stabilizes.

- **n8n** — Cookbook: [`docs/agents/n8n.md`](./agents/n8n.md).
  - Rationale: unlocks workflow-engine users, but Path 1 (CLI) is brittle and Path 2 (HTTP) depends on `cellar-worker` which is not yet shipped.
  - Acceptance: cookbook (done) with Path 1 working today; Path 2 tracked as blocked on Phase 1 worker HTTP protocol ([docs/worker-protocol.md](./worker-protocol.md)). Dedicated `n8n-nodes-cellar` community node is a Phase 2 follow-up, not blocking.

### P3 — covered transitively

- **Gemini CLI**, **GPT custom tool callers**, and any other MCP-capable runtime we have not named.
  - Rationale: the raw MCP client cookbook is sufficient. Standing up a bespoke cookbook for each is make-work until there is explicit user demand.
  - Acceptance: no dedicated doc. The raw MCP client cookbook is the documented path. Revisit on request.

## Why This Order

Three forces shape the ranking:

1. **Reach.** Claude Code and raw MCP together cover most MCP-native demand in April 2026.
2. **Ecosystem match.** Mastra and Cursor are closest to the Cellar stack (TypeScript, MCP-first, IDE-adjacent).
3. **Drift risk.** Codex and n8n both depend on external surfaces that have been moving: Codex's config and n8n's lack of native MCP. Pinning examples against moving targets is costly — cookbooks first, runnable examples only after stabilization.

## Expected Deliverables by End of Next Sprint

- [ ] P0 — `examples/claude-code/` runs end-to-end with the Numbers task.
- [ ] P0 — `examples/mcp-client-node/` implements the cookbook verbatim.
- [ ] P1 — Mastra eval scenario, pinned Mastra version.
- [ ] P1 — Cursor eval scenario + screenshot assets.
- [ ] P2 — Revisit Codex cookbook once upstream config settles.

## Review Cadence

Re-evaluate this ranking at each minor CEL release (next checkpoint: v0.3). Reasons to re-rank:

- A runtime we have as P2 suddenly accounts for >20% of user demand.
- A P0/P1 runtime ships a breaking MCP change that invalidates its cookbook.
- Phase 1 worker HTTP lands and unblocks n8n Path 2.
- A new major agent runtime emerges (plausible given Q2 2026's pace).

## See Also

- [docs/agents/README.md](./agents/README.md) — the cookbook index
- [docs/adapters-cel-agents.md](./adapters-cel-agents.md) — why runtimes are clients, not the identity
- [docs/mcp-server.md](./mcp-server.md) — the tool surface every runtime binds against
- [docs/worker-protocol.md](./worker-protocol.md) — the HTTP path (unblocks n8n Path 2)
- [docs/langgraph-rust-sidecar.md](./langgraph-rust-sidecar.md) — the original reference integration
