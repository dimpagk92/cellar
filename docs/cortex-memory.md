# Cortex Memory

Durable, workflow-scoped memory that the cortex selector can hydrate into a [`PlanningView`](cognition.md). Closes [issue #33](https://github.com/dimpagk92/cellar/issues/33). The `cortex_memories` table lives in `~/.cellar/cel-store.db` (the same SQLite database used for the rest of cel-store) and is covered by the same migration history.

This document is the user-facing reference for the memory feature. For the broader design see [`COGNITION_LAYER_PLAN.md`](../COGNITION_LAYER_PLAN.md).

## Principles

1. **Store broadly, select narrowly.** The store is allowed to grow; the LLM only ever sees a budgeted slice surfaced via the cortex selector.
2. **Opt-in.** No memory is written unless the caller explicitly asks for it. Privacy-first defaults.
3. **Workflow-scoped.** Every memory belongs to a `workflow_id`. There is no global cross-workflow recall in v1.
4. **Local-only.** Memories are persisted to the local SQLite store. They never leave the host. Inspection and deletion are first-class operations.

## What gets stored

| Field | Type | Notes |
|---|---|---|
| `id` | `INTEGER` | Auto-increment primary key |
| `workflow_id` | `TEXT NOT NULL` | Caller-chosen identifier; required |
| `kind` | `TEXT NOT NULL` | One of `outcome`, `prior`, `failure`, `preference` |
| `content` | `TEXT NOT NULL` | Structured JSON; shape depends on `kind` |
| `summary` | `TEXT` | Optional one-liner — surfaced in the selector catalog |
| `tags` | `TEXT` | Optional JSON array (populated by future tag-generator enricher) |
| `embedding` | `BLOB` | Optional vector for PR3 pre-filter; `NULL` is fine |
| `source_ref` | `TEXT` | Optional back-reference (transcript span / checkpoint id / fact id) |
| `created_at` | `INTEGER NOT NULL` | Unix epoch seconds |
| `last_accessed_at` | `INTEGER NOT NULL` | Unix epoch seconds — updated on hydration |

### Memory kinds

| `kind` | Use it for | Example `content` |
|---|---|---|
| `outcome` | What happened — replayable | `{ "kind": "outcome", "action": "click", "target": "Save", "result": "ok" }` |
| `prior` | A generalisation derived from outcomes | `{ "kind": "prior", "statement": "Concur uses two-step submit" }` |
| `failure` | Something to avoid + workaround | `{ "kind": "failure", "what_failed": "click submit", "why": "modal opened first", "workaround": "dismiss then click" }` |
| `preference` | User preference informing future planning | `{ "kind": "preference", "statement": "User prefers confirmations before destructive actions" }` |

## Lifecycle

### Writing

There are three ways memories get written:

1. **Explicit, via MCP `cel_think store_memory`.** The host (Claude Code, Cursor, etc.) calls the mode directly — useful when the host wants to seed a known prior or capture a one-off observation.

   ```jsonc
   {
     "mode": "store_memory",
     "workflow_id": "concur-expense",
     "kind": "prior",
     "content": { "kind": "prior", "statement": "..." },
     "summary": "Concur uses a two-step submit",
     "tags": ["concur", "submit"]
   }
   ```

2. **Auto-write on `cel_perceive checkpoint`.** When a session was started with `enable_memory: true` plus a `workflow_id`, every `checkpoint` writes an `outcome` memory capturing the just-finished phase (summary, action count, timestamp).

   ```jsonc
   { "mode": "start", "goal": "...", "enable_memory": true, "workflow_id": "concur-expense" }
   { "mode": "checkpoint", "summary": "Login complete; on the dashboard" }
   ```

3. **Auto-write on canonical-runner final outcome.** When `RunLimits.workflow_id_for_memory` and `RunLimits.memory_db_path` are both set, the canonical Rust runner writes a single final `outcome` (on success) or `failure` (on failure) memory with the run's terminal report. Both fields are required — either alone is a no-op.

### Reading

The cortex selector hydrates memories at planning-view build time (PR3). Hosts can also inspect explicitly:

- **`cel_think search_memory`** — case-insensitive substring search over `summary` + `content` for v1 (PR3 may upgrade to FTS5 if recall demands it).

  ```jsonc
  { "mode": "search_memory", "workflow_id": "concur-expense", "query": "submit", "limit": 20 }
  ```

The MCP `cel_perceive read` response surfaces the selector's chosen subset under `contextSummary.cognition.relevant_memories` once PR3 lands.

### Deletion / decay

Memories never reach a hard cutoff for selection eligibility — decay only influences ranking and pruning.

- **Automatic decay.** Every memory has an exponential-half-life decay score: `score = e^(-ln(2) * age_days / 90)` against `last_accessed_at`. At 90 days of no access, a memory's score halves; at 365 days, it's at ~6%; at 600 days, ~1%.
- **Pruning.** `cel_think prune_memory` deletes everything with `decay_score < threshold`. The default `0.01` cuts memories last accessed roughly 20 months ago. Tighter thresholds prune sooner:

  | Threshold | Cuts memories last accessed older than |
  |---|---|
  | `0.5` | ~3 months |
  | `0.125` | ~9 months |
  | `0.01` (default) | ~20 months |

  ```jsonc
  { "mode": "prune_memory", "threshold": 0.01 }
  ```

- **Manual deletion.** No public single-row delete API in v1. Direct SQLite access against `~/.cellar/cel-store.db` works (`DELETE FROM cortex_memories WHERE id = ?`). PR4 (cel-cognition) may add per-id deletion through a richer surface.

## Privacy guarantees

- Memories are **never written** unless the caller explicitly opts in. Three independent opt-in surfaces (`store_memory` mode, `cel_perceive start { enable_memory }`, `RunLimits.workflow_id_for_memory`) — no path silently enables it.
- Memories are **local-only**. The store is a SQLite file on the host machine. Nothing is uploaded.
- Memories are **workflow-scoped**. Recall stays within the named `workflow_id`; there's no global pool.
- The store is **inspectable** at `~/.cellar/cel-store.db` via standard SQLite tooling (`sqlite3`, DB Browser, etc.).
- The store is **deletable** by removing that file. The cortex re-creates an empty schema on next boot.

## Out of scope (v1)

- Cross-workflow recall.
- Per-record TTL (only global decay).
- Selective recall ranking by tag/kind beyond the selector's heuristics — PR3 adds the selector; PR4 may add LLM-driven ranking.
- Encryption at rest. The SQLite file uses standard filesystem permissions; if you need stronger isolation, run cellar in a container or use FileVault.
- Audit trail of memory writes (who/when from outside the host process). The local-only design removes the most common audit needs; PR4 telemetry can add per-write trace events if demand surfaces.
