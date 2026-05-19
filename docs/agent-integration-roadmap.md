# Agent Integration and Trust Proof Roadmap

Date: May 14, 2026

Status: **active direction** — trust/execution proof comes before broad runtime expansion.

## Framing

CEL's durable value is in device understanding, trusted execution, verification, receipts, and adapter truth - not in owning a planner. That means the roadmap question is not "which planner do we build?" and not even "how many runtimes can we claim?" The sharper question is:

> Can any competent agent use CEL to operate a real computer, prove what happened, and leave an auditable receipt trail?

The north-star boundary stays the same across every runtime:

- **Agent owns:** planning, retries, branching, checkpointing, approvals, stop conditions.
- **CEL owns:** fused context, screenshots, action execution, adapter dispatch, verification surfaces, receipts.
- **Adapters own:** app-specific truth.

See [docs/adapters-cel-agents.md](./adapters-cel-agents.md) and [docs/trust-execution-layer.md](./trust-execution-layer.md).

Integration docs live under [docs/agents/](./agents/README.md).

## Priority Tiers & Acceptance Bars

| Tier | Meaning | Acceptance Bar |
|---|---|---|
| **P0** | Must-have for the trust/execution claim | End-to-end transcript with healthcheck, receipts, independent verification, and env-valid eval result |
| **P1** | Expand trust proof across surfaces | Adapter-backed examples and agent-agnostic evals that prove receipts + verification |
| **P2** | Runtime reach after P0/P1 is credible | Cookbook + thin example, no bespoke semantics |
| **P3** | Covered transitively | No dedicated doc - user routes through the raw MCP client cookbook |

## Ranked List

### Already shipped

- **LangGraph** — Reference integration. Cookbook: [`docs/langgraph-rust-sidecar.md`](./langgraph-rust-sidecar.md). Validated the boundary before the cookbook set existed. No change planned.

### P0 — ship next: trusted execution proof

- **Claude Code** — Cookbook: [`docs/agents/claude-code.md`](./agents/claude-code.md).
  - Rationale: highest adoption of any MCP client in April 2026; a working Claude Code path is the biggest reach win and the lowest-friction demo for new users.
  - Acceptance: cookbook (done), `/cellar/healthcheck`, plus `examples/claude-code/` containing the Numbers `BTC/ETH/SOL` task with `write_cells` receipts, `read_cells` readback, and a final answer that cites both. The transcript must distinguish "dispatched" from "verified".

- **Raw MCP client reference** — Cookbook: [`docs/agents/mcp-client.md`](./agents/mcp-client.md).
  - Rationale: proves the agent-agnostic claim. Low effort. Also serves as the fallback path for any runtime we don't cover explicitly.
  - Acceptance: cookbook (done), plus `examples/mcp-client-node/` implementing the minimal example verbatim and printing receipts. The example should pass even when no planner is involved.

### P1 — ship after P0 lands: adapter-backed trust

- **Numbers trust suite** — external-agent evals.
  - Rationale: Numbers is AX-hostile, so it cleanly proves why adapter truth matters.
  - Acceptance: env-valid eval lane with active `numbers` adapter, `write_cells` receipt, `read_cells` readback, and no generic typing fallback.

- **Browser/CDP trust suite** — external-agent evals.
  - Rationale: browser tasks should prove CDP access instead of silently degrading to blind coordinate actions.
  - Acceptance: healthcheck shows CDP target, navigation/action receipts include `dispatch_path: "cdp"` or browser adapter routing, and post-action DOM state verifies success.

- **Eval validity gate**.
  - Rationale: a low score from missing adapters, stub AX, or unavailable CDP is not a product baseline.
  - Acceptance: reports call out environment-invalid signals separately from product failures.

### P2 — runtime reach after the proof is solid

- **Mastra** — Cookbook: [`docs/agents/mastra.md`](./agents/mastra.md).
  - Rationale: TypeScript-first, same ecosystem as Cellar. Good once the CEL receipt contract is stable enough to wrap.
  - Acceptance: cookbook (done), one thin example or eval scenario that consumes the same MCP receipts, pinned against a specific Mastra version.

- **Cursor** — Cookbook: [`docs/agents/cursor.md`](./agents/cursor.md).
  - Rationale: good early-adopter overlap, but not worth bespoke work until Claude Code and raw MCP prove the general boundary.
  - Acceptance: cookbook (done), one canned task transcript that includes healthcheck and receipts.

- **Codex CLI** — Cookbook: [`docs/agents/codex.md`](./agents/codex.md).
  - Rationale: OpenAI's official terminal agent. Keep it as an MCP client, not a new Cellar identity.
  - Acceptance: cookbook only until Codex MCP config stabilizes enough for a pinned example.

- **n8n** — Cookbook: [`docs/agents/n8n.md`](./agents/n8n.md).
  - Rationale: unlocks workflow-engine users, but Path 1 (CLI) is brittle and Path 2 (HTTP) depends on `cellar-worker` which is not yet shipped.
  - Acceptance: cookbook (done) with Path 1 working today; Path 2 tracked as blocked on Phase 1 worker HTTP protocol ([docs/worker-protocol.md](./worker-protocol.md)). Dedicated `n8n-nodes-cellar` community node is a Phase 2 follow-up, not blocking.

### P3 — covered transitively

- **Gemini CLI**, **GPT custom tool callers**, and any other MCP-capable runtime we have not named.
  - Rationale: the raw MCP client cookbook is sufficient. Standing up a bespoke cookbook for each is make-work until there is explicit user demand.
  - Acceptance: no dedicated doc. The raw MCP client cookbook is the documented path. Revisit on request.

## Why This Order

Three forces shape the ranking:

1. **Trust beats reach.** A trusted Claude Code transcript is more valuable than five shallow runtime badges.
2. **Receipts create the integration contract.** Once receipts are stable, every runtime can consume the same proof shape.
3. **Adapters prove the moat.** Numbers and browser CDP show why CEL is more than generic UI perception.
4. **Drift risk is real.** Codex, Cursor, Mastra, and n8n can move underneath us. Keep their docs thin until the CEL boundary is stronger than their churn.

## Expected Deliverables by End of Next Sprint

- [ ] P0 — `examples/claude-code/` runs end-to-end with healthcheck, Numbers write receipt, readback, and final verification.
- [ ] P0 — `examples/mcp-client-node/` implements the cookbook verbatim and prints receipts.
- [ ] P0 — `cel_act` receipts are documented and returned over MCP for success and failure paths.
- [ ] P1 — Eval reports flag environment-invalid signals separately from product failures.
- [ ] P1 — External-agent Numbers and browser/CDP scenarios assert adapter/CDP truth, not planner internals.

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
