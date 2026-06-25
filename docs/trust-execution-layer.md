# CEL as the Context and Trust Layer

Date: May 14, 2026

## Thesis

CEL is the context and trust layer for AI-operated software. Agents bring intent and planning; CEL defines the context, memory, brief, transport, and receipt contracts that let a runtime turn intent into reliable observations, governed actions, verification signals, and audit evidence.

The durable OSS value is not one planner and not a proprietary live engine. It is the data plane that lets many planners speak the same language about what was seen, what persisted, what the model received, what was dispatched, and what evidence remains. The commercial Cellar/Dilipod runtime operates that data plane continuously through cortex, policy, monitoring, and compliance workflows.

## Trust Loop

Every serious action should fit this loop:

```text
intent -> observation -> dispatch -> observed effect -> evidence -> receipt
```

- **Intent**: the agent states what it is trying to do.
- **Observation**: CEL-shaped context provides fused data from AX, CDP, screenshots, signals, adapters, logs, and other sources.
- **Dispatch**: a runtime routes the action through the safest available substrate: adapter, CDP, AX, native input, or another implementation path.
- **Observed effect**: the runtime or agent re-checks the state after the action.
- **Evidence**: adapter readback, CDP/AX state, screenshot, or Cortex diff supports the claim.
- **Receipt**: CEL returns a structured record of what was dispatched and what verification is still required.

## What The OSS Contracts Own

- Context snapshot and merge contracts (`ContextElement`, `ScreenContext`, source metadata, confidence, references).
- Durable memory contracts (`cel-memory`) and local backend (`cel-memory-sqlite`).
- Governed model briefing (`cel-brief`) and brief receipts.
- Transport and action/receipt schemas for MCP, CLI, SDK, and N-API consumers.
- The distinction between dispatch proof, model-input proof, and completion proof.

## What The Commercial Runtime Owns

- Continuous cortex operation: freshness, diffs, anomalies, source prioritization, and live mental models.
- Runtime capability reporting, including whether a trusted input/perception path is actually available.
- Adapter lifecycle, adapter routing, and app-specific structured truth in production sessions.
- Canonical action execution, dispatch path selection, and policy enforcement.
- Audit timelines, retention, alerting, compliance exports, and governance workflows.

## What Agents Own

- Planning, retries, branching, checkpointing, and stop conditions.
- User approval policy.
- How to choose between available CEL observations and action options.
- Final claims, backed by CEL receipts and verification evidence.

## Receipt Contract

`cel_act` returns the legacy `result` / `results` fields and also returns a `receipt` / `receipts` field. A receipt is proof of dispatch, not proof that the whole user goal is done.

Current MCP receipt shape:

```json
{
  "id": "cel_act_write_cells_l...",
  "action": "write_cells",
  "requested_at": "2026-05-14T09:12:01.000Z",
  "completed_at": "2026-05-14T09:12:01.420Z",
  "status": "ok",
  "dispatch_path": "adapter",
  "mutates_state": true,
  "requires_verification": true,
  "verification": "adapter_readback",
  "evidence": [
    { "kind": "dispatch_path", "value": "adapter" },
    { "kind": "cell_refs", "value": ["A1", "B1"] },
    { "kind": "verify_requested", "value": true }
  ],
  "summary": "{... adapter result ...}"
}
```

Future receipts should converge across MCP, CLI, SDK, and N-API. They should be persisted when a run transcript exists, but the public response shape should stay stable.

## Acceptance Bar

A CEL task is trusted only when:

- the required perception path was available before acting
- the action used the most structured route available for the app
- a post-action observation or adapter readback supports the claimed result
- the transcript includes receipts for mutating actions
- evals distinguish environment-invalid runs from product failures

## Design Consequences

- Prefer adapters for app truth. Numbers cells, browser DOM, messages, calendars, and future app models should not be forced through generic pixels when structured APIs exist.
- Keep AX strong. It remains the universal substrate for windows, dialogs, focus, and cross-app handoffs.
- Keep CDP explicit. Browser work should know whether a CDP target is available; agents should not silently fall back to blind clicking when DOM access was required.
- Treat built-in planners as clients. LangGraph, Mastra, Claude Code, Codex, Cursor, GPT, Gemini, and future runtimes should all drive the same CEL trust loop.
- Make evals agent-agnostic first. Runtime-specific tests are useful, but the main score should ask whether CEL made the computer understandable and executable for any competent agent.

## See Also

- [adapters-cel-agents.md](adapters-cel-agents.md)
- [mcp-server.md](mcp-server.md)
- [agent-integration-roadmap.md](agent-integration-roadmap.md)
- [eval-runbook.md](eval-runbook.md)
