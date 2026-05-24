# Cellar v1 Architecture

This is a developer-facing tour of the daemon's internals. For the
strategic plan see `/Users/dimitriospagkratis/.claude/plans/cellar-app-v1.md`.
For the IPC wire contract see
`/Users/dimitriospagkratis/.claude/plans/cellar-ipc-protocol.md`.

## High-level shape

```
┌────────────────────────────────────────────────────────────────────┐
│                     cel-cortex-daemon (one process)                │
│                                                                    │
│  ┌─ Ambient event sources ─┐    ┌─ Matcher consumer task ─┐         │
│  │ process_poller          │───▶│ - filter gateway events │         │
│  │ fsevents adapter        │    │ - run Matcher::evaluate │         │
│  │ signals_poller          │    │ - cooldown check        │         │
│  └─────────────────────────┘    │ - write Fire chunks     │         │
│                                 │ - publish on FireBus    │         │
│                                 │ - webhook fan-out       │         │
│                                 └─────────────────────────┘         │
│                                                                    │
│  ┌─ cel_act gateway ──────┐                                         │
│  │ intercept(action)       │     SqliteRulesStore                   │
│  │ - synthesise event      │     • rules / watchlists / webhooks    │
│  │ - run Matcher           │◀────• Arc<...> shared across the       │
│  │ - cooldown filter       │       gateway + matcher consumer       │
│  │ - confirm via broker    │     • impls RuleSource + WatchlistLookup │
│  │ - actuate               │                                         │
│  │ - write Action chunk    │     SqliteMemoryProvider                │
│  │ - webhook fan-out       │     • sessions, chunks, retrieval      │
│  └─────────────────────────┘     • MemoryWriteHook governance seam  │
│                                                                    │
│  ┌─ AgentRuntime ─────────┐     IpcConfirmationBroker               │
│  │ run_turn(s, content)    │     • Mutex<HashMap<id, oneshot>>      │
│  │ - write user chunk      │◀───▶• request_confirmation awaits      │
│  │ - retrieve context      │     • resolve via IPC unblocks         │
│  │ - LLM complete()        │                                         │
│  │ - write assistant chunk │                                         │
│  │ - publish on ChatBus    │                                         │
│  └─────────────────────────┘                                         │
│                                                                    │
│  ┌─ IPC server (UDS, JSON-RPC 2.0) ────────────────────────────┐    │
│  │  DaemonIpcHandler — every method dispatches to the relevant │    │
│  │  subsystem above. Subscribe methods spawn forwarder tasks   │    │
│  │  that bridge the broadcast buses to per-connection sinks.   │    │
│  └─────────────────────────────────────────────────────────────┘    │
└────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼ JSON-RPC over UDS
                  ┌─────────────────────────────────┐
                  │ Tauri app  /  cellar CLI  /     │
                  │ external MCP (Cursor, Codex)    │
                  └─────────────────────────────────┘
```

## Subsystems by crate

| Crate | Role |
|---|---|
| `cellar-types` | Wire types: `Rule`, `Event`, `Action`, `Expression`, `Watchlist`, `WebhookConfig`. Pure data + the `Matcher`. |
| `cel-act-gateway` | The gateway. Generic over `Actuator`, `ConfirmationBroker`, `RuleSource`, `WatchlistLookup`. Owns `CooldownTracker` + `WebhookHook` trait. |
| `cel-memory` | `MemoryProvider` trait + `BasicMemoryProvider` (in-memory). |
| `cel-memory-sqlite` | Production memory provider: SQLite + sqlite-vec + fastembed. |
| `cellar-rules-store` | SQLite-backed rules + watchlists + webhooks. Implements `RuleSource` + `WatchlistLookup`. Hot-reload via `Arc<RwLock>` snapshots. |
| `cellar-llm-router` | LLM provider abstraction with Anthropic / OpenAI-compatible / Ollama adapters. Per-subsystem env-var config. |
| `cellar-rule-compiler` | NL → typed `Rule`. Uses the router. |
| `cellar-webhook` | `WebhookService` (queue + retry), `Sender` trait, `GatewayHook`. |
| `cellar-ipc` | JSON-RPC 2.0 over UDS. `Handler` trait, `Server`, `Client`, params + results. |
| `cel-cortex-daemon` | The daemon. Owns all the runtime state and wires every subsystem above. |
| `cellar-cli` | `cellar` shell binary. |

## Hot-reload model

All shared subsystem state lives behind `Arc<...>` clones the daemon
hands out at startup:

- `Arc<SqliteRulesStore>` → gateway (as RuleSource + WatchlistLookup),
  matcher consumer task (two more clones), IPC handler. Writes through
  any clone are visible to all readers on the next snapshot.
- `Arc<CooldownTracker>` → gateway + matcher consumer task. A fire
  through either path counts against the same window.
- `Arc<dyn WebhookHook>` (when wired) → gateway + matcher consumer
  task. Same retry queue regardless of which path matched.
- `Arc<dyn MemoryProvider>` → gateway (for Action + Fire chunks),
  matcher consumer (for Fire chunks), agent runtime (for chat sessions).
- `Arc<IpcConfirmationBroker>` → gateway (as `B: ConfirmationBroker`),
  IPC handler (`confirmation.resolve` path).

The blanket impls `impl<T: Trait> Trait for Arc<T>` in `cel-act-gateway`
and `cellar-types` (for the four shared traits) are what make this
pattern compile.

## Event flow — ambient

1. An ambient source (process_poller / fsevents / signals_poller)
   publishes a `cellar_types::Event` on the `EventBus` (tokio
   broadcast).
2. The matcher consumer task receives it.
3. It snapshots the current rule set from `Arc<SqliteRulesStore>`.
4. `Matcher::evaluate(event, rules, watchlists)` returns the matching
   rules.
5. For each match the cooldown tracker decides whether to fire; if
   yes:
   - Write a `ChunkKind::Fire` to memory.
   - Publish a `FireFrame` on the `FireBus`.
   - If `action.type == webhook`, call the `WebhookHook` (which enqueues
     on the `WebhookService` queue).
6. The ring-filler task drains the `FireBus` into the bounded
   `fire_ring` for `fires.recent` backfill.
7. Any `fires.subscribe` forwarder pushes the frame to the
   per-connection IPC sink.

## Event flow — agent action (gateway)

1. Anything calling `gateway.intercept(action)` — embedded agent,
   external MCP, CLI — provides a `ProposedAction`.
2. Gateway synthesises an `agent_action_attempted` event.
3. Runs the matcher; filters by cooldown.
4. Writes Fire chunks; fans out webhooks.
5. Computes the gateway decision:
   - No match / `log_only` → execute.
   - `webhook` action → execute (webhook is fire-and-forget).
   - `require_confirmation` → drive the broker. Production daemon's
     broker pushes a `PendingConfirmation` on the `ConfirmationBus`,
     awaits the oneshot, times out per `expires_at`.
   - `veto` / `soft_block` → return `Vetoed` without executing.
6. On execute, calls the configured `Actuator`.
7. Writes an `Action` chunk capturing the outcome.

## Subscription forwarders

Each subscribe IPC call (`events.subscribe`, `fires.subscribe`,
`confirmation.subscribe`, `agent.chat.subscribe`) spawns a small tokio
task that:

1. Subscribes to the relevant broadcast bus.
2. Applies the server-side filter (kinds / sources / session_id / etc).
3. Wraps each match in a `StreamFrame { subscription_id, payload }`.
4. Pushes the frame into the per-connection `FrameSink` (mpsc).
5. On `Lagged`, emits a `Gap` frame so the client can re-fetch via
   `*.recent`.
6. On sink-closed or registry-aborted, exits cleanly.

The `SubscriptionRegistry` holds a `JoinHandle` per id so
`unsubscribe` and connection close can abort the forwarder.

## What's missing in v1 (Phase 3.x and later)

- **Agent tool dispatch.** The embedded `AgentRuntime` can chat; it
  cannot yet call `cel_act` itself. Wiring the LLM's `tool_use`
  blocks through the gateway is a follow-up.
- **Token-level streaming.** `agent.chat.subscribe` currently sees one
  `MessageComplete` per turn instead of `Token` deltas.
- **Webhook hot-reload.** Webhooks added via `webhooks.add` after
  startup are persisted but not visible to the running
  `WebhookService`. Restart picks them up.
- **`agent.sessions.rename`.** Needs a new trait method on
  `MemoryProvider`.
- **`agent.interrupt`.** Needs a cancellation token threaded through
  `run_turn`.
- **`agent_actions.*` subscribe / recent.** Needs a synthetic
  agent-action bus the gateway publishes to.
- **`ActionType::Allow`** for `RememberKind::ExceptionRule`. Locked
  enum change, deserves its own RFC update.
