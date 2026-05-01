# Driving CEL from n8n

This page shows how to drive CEL from n8n.

Read [docs/adapters-cel-agents.md](../adapters-cel-agents.md) first if you haven't.

## Purpose

n8n is a workflow automation platform. It does not speak MCP natively, so CEL needs a bridge. Two paths:

1. **Execute Command node → `cellar` CLI.** Works today. Each step is a shell invocation. Good for deterministic, short workflows.
2. **HTTP Request node → `cellar-worker`.** Planned. Uses the Phase 1 worker protocol ([docs/worker-protocol.md](../worker-protocol.md)). Good for long-running goals and remote execution. **Not yet shipped** — tracked on the roadmap.

This document covers both. Path 1 is usable now.

## Ownership

- **n8n owns:** the workflow graph, branching, error routing, retries, and scheduling.
- **CEL owns:** execution of each goal or action.
- **Adapters own:** app-specific truth (Numbers cells etc.).

Because n8n is a workflow engine, not an LLM agent, it typically either:

- (a) hands CEL a full natural-language goal and lets `cel_think run_goal` drive autonomously, or
- (b) hands CEL discrete actions produced by some upstream LLM node (e.g. an OpenAI node) and composes them with `cel_act`.

Mode (a) is the simplest integration and is what the examples below show.

## Path 1: Execute Command → `cellar` CLI (works today)

### Setup

1. Build CEL on the machine running n8n:
   ```bash
   cd /path/to/cellar && pnpm install && pnpm -r build
   ```
2. Make sure `cellar` is on PATH (or reference it by absolute path in the n8n node).
3. Grant macOS Accessibility permission to whichever process n8n uses to shell out (e.g. `node`, `docker`, or the n8n desktop app).
4. If you use `cel_think run_goal` from the CLI, set `CEL_LLM_PROVIDER`, `CEL_LLM_API_KEY`, and `CEL_LLM_MODEL` in the n8n environment.

### Example n8n Workflow Snippet

Minimal workflow: manual trigger → Execute Command node that runs a CEL goal → Set node that surfaces the result.

```json
{
  "nodes": [
    {
      "parameters": {},
      "id": "trigger",
      "name": "Manual Trigger",
      "type": "n8n-nodes-base.manualTrigger",
      "typeVersion": 1,
      "position": [240, 300]
    },
    {
      "parameters": {
        "command": "cellar run-goal \"Open Numbers and write BTC in A1\" --json"
      },
      "id": "cel-run",
      "name": "CEL Run Goal",
      "type": "n8n-nodes-base.executeCommand",
      "typeVersion": 1,
      "position": [520, 300]
    },
    {
      "parameters": {
        "values": {
          "string": [
            { "name": "result", "value": "={{ $json.stdout }}" }
          ]
        }
      },
      "id": "result",
      "name": "Result",
      "type": "n8n-nodes-base.set",
      "typeVersion": 2,
      "position": [800, 300]
    }
  ],
  "connections": {
    "Manual Trigger": { "main": [[{ "node": "CEL Run Goal", "type": "main", "index": 0 }]] },
    "CEL Run Goal": { "main": [[{ "node": "Result", "type": "main", "index": 0 }]] }
  }
}
```

TODO: verify that `cellar run-goal` accepts `--json`. If not in your build, omit the flag and parse stdout as free text.

### Tips

- Each Execute Command invocation starts a fresh CEL process. State does not persist between nodes.
- For action-by-action composition (rather than one `run-goal` call), invoke `cellar` subcommands repeatedly: `cellar context`, `cellar action click --x 100 --y 200`, etc. See `cli/src/commands/` for the current command set.
- Timeouts — set the Execute Command node's timeout to match CEL's `--timeout-ms`.

## Path 2: HTTP Request → `cellar-worker` (planned)

### Status

**Not yet shipped.** The worker HTTP protocol is drafted in [docs/worker-protocol.md](../worker-protocol.md) as v1-draft. When the worker daemon is stable (Phase 1, Milestone 1.0), n8n will be able to post goals over HTTP and poll for results.

Once shipped, the wire looks like:

```
POST /v1/goals       → { job_id, status: "queued", created_at }
GET  /v1/jobs/{id}   → { job_id, status, result?, error? }
```

The n8n side becomes a standard HTTP Request node pair: one to submit, one to poll. This is the right path for:

- Remote execution (the worker runs elsewhere, n8n runs anywhere).
- Long-running goals where shell-timeout semantics are awkward.
- Multi-tenant setups where each n8n workspace talks to its own worker.

### TODO markers

- [ ] Pin worker-protocol v1 (tracked in `docs/worker-protocol.md`).
- [ ] Publish `cellar-worker` as a runnable binary / Docker image.
- [ ] Write an n8n community node (`n8n-nodes-cellar`) that wraps submit/poll into a single node.

## Minimal Example Task

For Path 1, a practical n8n recipe:

```
Trigger: Cron, every hour.
  → Execute Command: cellar run-goal "Open Safari, go to finance.example.com, screenshot the BTC price card, save to /tmp/btc.png"
  → Read Binary File: /tmp/btc.png
  → Slack node: upload image to #trading
```

The Execute Command step delegates the entire desktop flow to CEL's built-in runner. n8n is responsible for scheduling, reading the artifact, and routing it to Slack.

## Known Gaps

- **No first-party n8n node yet.** You are composing standard n8n primitives. A dedicated community node would remove boilerplate.
- **Path 2 requires cellar-worker**, which is not yet shipped.
- **Filesystem coupling.** Path 1 exchanges data through stdout and temp files; there is no structured return envelope. Use `--json` where supported; otherwise parse stdout.
- **Accessibility permission ownership.** If n8n runs under a different user or in a container, macOS Accessibility permission needs to be granted to whichever process ultimately invokes `cellar`.
- `cel_perceive` is a singleton — concurrent n8n workflows cannot each start their own Cortex session.

## See Also

- [docs/agents/README.md](./README.md) — index of all agent cookbooks
- [docs/worker-protocol.md](../worker-protocol.md) — the HTTP protocol that Path 2 will speak
- [docs/adapters-cel-agents.md](../adapters-cel-agents.md) — architecture
- [docs/mcp-server.md](../mcp-server.md) — tool reference (Path 1 CLI mirrors this surface)
- [docs/agent-integration-roadmap.md](../agent-integration-roadmap.md) — n8n is P2 (blocked on worker protocol for Path 2)
