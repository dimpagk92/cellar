# CEL as the Trust and Execution Layer

Date: May 14, 2026

## Thesis

CEL is the trust and execution layer for AI-operated computers. Agents bring intent and planning; CEL turns that intent into reliable device observations, routed actions, verification signals, and receipts that can be audited later.

The durable platform value is not one planner. It is the substrate that lets many planners act on a real computer without guessing whether the screen, browser, app model, or input route can be trusted.

## Trust Loop

Every serious action should fit this loop:

```text
intent -> observation -> dispatch -> observed effect -> evidence -> receipt
```

- **Intent**: the agent states what it is trying to do.
- **Observation**: CEL provides fused context from AX, CDP, screenshots, signals, and adapters.
- **Dispatch**: CEL routes the action through the safest available substrate: adapter, CDP, AX, or native input.
- **Observed effect**: CEL or the agent re-checks the state after the action.
- **Evidence**: adapter readback, CDP/AX state, screenshot, or Cortex diff supports the claim.
- **Receipt**: CEL returns a structured record of what was dispatched and what verification is still required.

## What CEL Owns

- Context fusion across AX, CDP, vision, signals, network, audio, and adapters.
- Runtime capability reporting, including whether a trusted input/perception path is actually available.
- Adapter lifecycle, adapter routing, and app-specific structured truth.
- Canonical action execution and dispatch path selection.
- Action receipts: dispatch path, timing, verification requirement, and evidence hints.
- Stable MCP, CLI, SDK, and N-API surfaces that any agent can drive.

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
