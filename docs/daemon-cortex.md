# Daemon-hosted Cortex

The daemon (`cel-cortex-daemon`) can host the **single** live Cortex —
perception + execution — and every surface (the Tauri app, the CLI, and the
MCP server) drives that one Cortex over IPC. This is the execution core of
Cellar v1.

## Why one Cortex

A Cortex grabs the macOS Accessibility tree, a CDP client, and input/focus.
Two live Cortexes in two processes (e.g. the napi/MCP one *and* an app/daemon
one) would fight over those single, machine-global resources. So there is
exactly **one** Cortex in a running system, and it lives in the always-on
daemon — the process the app and CLI already talk to, and the one that owns the
governance gateway (every actuation chokepoint).

## Topology

```
  app  ─IPC─┐
  CLI  ─IPC─┤→  daemon  ──►  Cortex (the one)  ──►  AX / CDP / input
  MCP  ─IPC─┘     │            tick loop
                  │            └─► ExecutionReceipt ─► ~/.cellar/runs + memory
                  └─► gateway (governance) ─► receipts (governed actions)
                        │
                  agent_runtime ─► BriefBuilder + ReceiptSource + PerceptionSource
```

- The daemon boots the Cortex via the shared `cel-boot::boot_default_cortex`
  helper — the same boot path `cel-napi` (standalone MCP) and `cel-eval`
  (benchmarks) use.
- IPC methods `cortex.see` / `cortex.act` / `cortex.perceive.*`
  (`cellar-ipc` `Handler` trait) drive it.
- The MCP server proxies `cel_act` / `cel_see` / `cel_perceive` to those IPC
  methods when the daemon hosts a Cortex; otherwise it falls back to its own
  in-process napi Cortex (standalone / benchmark use).

## Environment flags

| Variable | Default | Effect |
|---|---|---|
| `CELLAR_DAEMON_CORTEX` | off | When truthy (`1`/`true`), the daemon hosts a live Cortex at boot. Off → the daemon runs exactly as before (no AX/input grab, no perception tick). |
| `CELLAR_DAEMON_NATIVE_INPUT` | off | When truthy, the daemon-hosted Cortex may dispatch native input (CGEvent mouse/keyboard/AX/app-activation). Off → perceive-only. Independent of `CELLAR_DAEMON_CORTEX`. |
| `CELLAR_MCP_DAEMON` | on | Set to `0` to force the MCP server to use its own napi Cortex and never proxy to the daemon. |
| `CELLAR_DAEMON_SOCK` | `~/.cellar/daemon.sock` | UDS path the daemon binds and the MCP client dials. |

## The one-Cortex invariant (how it's enforced)

The MCP server probes `daemon.status.cortex_running` on first use:

- **`cortex_running == true`** → proxy cortex operations to the daemon and
  **never boot the napi Cortex** (the two-Cortex guard, `mcp-server/src/tools`).
  The probe result is sticky for the server's lifetime.
- **otherwise** → boot/use the in-process napi Cortex as before.

A few in-process-only MCP operations (`cel_perceive plan_view`,
`cel_think run_goal`, anomaly consumption) return a typed error in daemon mode
rather than booting a second Cortex; they move onto IPC / `agent.run` in a
later phase.

## Receipts & continuity

Every governed dispatch through the gateway, and every canonical action through
the Cortex, emits a core `ExecutionReceipt` (`cel-contracts`) scoped to a
`run_id`, appended to `~/.cellar/runs/<run_id>.jsonl`. The daemon agent's
per-turn brief surfaces the run's recent receipts (`ReceiptSource`) plus a live
screen snapshot (`PerceptionSource`), closing the
`intent → dispatch → observed effect → evidence → brief` loop. See
`cellar-receipt-timeline.md`.

## Benchmarks

`cel-eval` and the `benchmarks/` harness keep using the in-process napi Cortex
(via `cel-boot`) — they don't require a running daemon. Production (app + CLI +
MCP together) uses the daemon-hosted Cortex.
