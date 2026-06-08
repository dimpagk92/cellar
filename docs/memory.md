# Cellar memory

Cellar's memory subsystem (`cel-memory` + `cel-memory-sqlite`) is the
local-first store the embedded agent, the rule compiler, the `cel_act`
gateway, and the rule matcher all share. It keeps a single
`memory.sqlite` per user with everything needed to recall what
happened, why, and what the user prefers — without leaving the device
unless the user explicitly opts in.

This is the user-facing reference. For the design rationale, the
per-table SQL schema, and the long-term roadmap see
[`cellar-memory-manager.md`](../../../../.claude/plans/cellar-memory-manager.md).
For the legacy `cortex_memories` table (a separate, smaller subsystem
that predates this work and is being deprecated) see
[`cortex-memory.md`](cortex-memory.md).

## What it does

Memory makes the embedded agent **good at recall** — it remembers chat
history, the user's preferences and corrections, actions the agent
took, rule firings, and salient observations. Every chunk is indexed by
both an embedding (vector search) and FTS (keyword search) so the
retrieval blends semantic and lexical signal.

Retrieval is hybrid: vector + FTS + recency, fused via Reciprocal Rank
Fusion (RRF, prior k=60). Six tuned profiles match the caller path —
the embedded agent's per-turn retrieval uses semantic-heavy weights and
a 7-day half-life; the audit timeline uses keyword-heavy weights and a
90-day window. See [§8.3 of the plan][profiles].

[profiles]: ../../../../.claude/plans/cellar-memory-manager.md

The targets, from the plan's §14.1:

| Metric    | Target |
|-----------|--------|
| Recall@5  | ≥ 0.85 |
| Recall@1  | ≥ 0.55 |
| MRR       | ≥ 0.65 |

These are the v1 quality bar — track them with `cellar eval memory`.

## What gets stored

A chunk is one row in `memory_chunks`. The `kind` discriminator drives
filtering, retention horizons, and importance defaults:

| Kind         | Stores                                                  |
|--------------|---------------------------------------------------------|
| `chat`       | A message between the user and an agent.                |
| `action`     | A `cel_act` call — attempted, completed, or denied.     |
| `fire`       | A rule firing (matched event + matched rule + outcome). |
| `observation`| A Cortex event the importance scorer flagged.           |
| `correction` | A user correction or override. Highest-signal kind.     |
| `job_summary`| End-of-session synthesis (goal, plan, actions, outcome).|
| `context`    | A file / app / URL focus episode.                       |
| `rollup`     | A summary that covers many chunks. Created by rollups.  |

Per-chunk fields include `caller_id` (which client wrote it: `embedded`,
`mcp:cursor`, `gateway`, etc.), `session_id` (conversation / job
grouping), `project_root`, `importance` (in [0, 1]), and `pinned`
(never auto-evicted).

## What's local vs cloud

The privacy posture mirrors Cellar's trust-and-execution-layer
positioning: **everything local by default, every off-device path is
opt-in, every off-device call is itself a governable action.**

### Always local

- Memory storage (`~/.cellar/memory.sqlite`).
- Embeddings — `bge-small-en-v1.5` via `fastembed-rs` (ONNX,
  130 MB model in `~/.cellar/models/`).
- FTS index.
- Retrieval (vector + FTS + recency fusion).
- Eviction sweeps.

### Opt-in cloud paths

| Subsystem      | Configured via                       | What leaves the device                                  |
|----------------|--------------------------------------|---------------------------------------------------------|
| Summarization  | `CELLAR_SUMMARIZER_PROVIDER=anthropic` (or `openai`) | Prompt text assembled from chunks                       |
| Embedding      | `CELLAR_EMBEDDING_PROVIDER=openai` (or `voyage`)     | Chunk text being embedded                               |

Both paths emit a `memory_offdevice_call_attempted` synthetic event
before firing. The rule matcher can intercept exactly like any other
action — `cel_act` semantics apply.

Encryption at rest in v1 is the macOS file permissions on
`~/.cellar/` (mode 0600) plus FileVault. Optional SQLCipher with a
Keychain-backed passphrase is v2 work.

## Inspecting your memory

Today (v1), the daemon owns the DB and external clients query through
it. The MCP `cel_remember` / `cel_recall` / `cel_forget` surfaces and
the Memory tab in the Tauri app are Phase 4 of the memory plan and not
yet shipped.

What works today:

```sh
cellar status                            # confirms daemon is healthy
cellar doctor memory                     # full memory-subsystem battery
cellar eval memory --corpus eval/memory/queries.jsonl
                                          # recall benchmark
```

`cellar doctor memory` checks that the DB exists and is readable, that
the embedding model is reachable, that the corpus is within the
configured cap (default 500k chunks), and that recent write p95 is
inside the 30 ms budget.

Direct SQL access is supported but **read-only** while the daemon is
running (SQLite WAL mode permits concurrent reads). For example:

```sh
sqlite3 -readonly ~/.cellar/memory.sqlite \
  "SELECT id, kind, caller_id, substr(content, 1, 80) FROM memory_chunks ORDER BY created_at DESC LIMIT 20;"
```

## Governing memory with rules

Memory writes are themselves governable events. Before any chunk
lands, the daemon emits a synthetic `memory_write_attempted` event
through the same rule matcher that governs `cel_act`. Rules that
match this event with a `redact_memory` (or `veto`) action suppress
the write — the chunk never reaches storage.

The natural-language compiler understands phrasings like:

> "never persist any memory chunk mentioning bank.example.com"
>
> "don't remember anything Cursor writes about my home directory"

These compile to an `audit`-kind rule with a `redact_memory` action
and a match expression on `data.content_preview` (first 256 chars of
the chunk content) or `data.caller`.

You can also author the rule by hand — both shapes are equivalent:

```json
{
  "id": "draft",
  "name": "Redact bank.example.com memory",
  "nl_original": "never persist any memory chunk mentioning bank.example.com",
  "kind": "audit",
  "enabled": true,
  "match": {
    "all": [
      {"leaf": {"field": "kind", "op": "eq", "value": "memory_write_attempted"}},
      {"leaf": {"field": "data.content_preview", "op": "contains", "value": "bank.example.com"}}
    ]
  },
  "action": {"type": "redact_memory"},
  "cooldown_seconds": 0
}
```

Save it via the standard `rules.add` path:

```sh
cellar rules add path/to/rule.json
```

The matcher returns rules in declaration order; the first matching
`veto` or `redact_memory` wins. The redacted chunk surfaces a
`<redacted: rule-name>` marker to the writer so the caller knows the
write was governed.

Three other useful governance patterns:

| Goal                                              | Match on event kind                  | Action                |
|---------------------------------------------------|--------------------------------------|-----------------------|
| Never persist chunks about a sensitive substring  | `memory_write_attempted`             | `redact_memory`       |
| Audit every memory read (sampled)                 | `memory_read`                        | `log_only`            |
| Confirm before any off-device embedding/summary   | `memory_offdevice_call_attempted`    | `require_confirmation`|

See [§11.5 of the plan][offdevice] for the full list of synthetic
memory events.

[offdevice]: ../../../../.claude/plans/cellar-memory-manager.md

## Exporting your memory

The provider exposes a single `export(filter)` call that produces a
self-contained bundle: matched chunks, the sessions they belong to,
the eviction log, and the access log for the same time window.

In v1 this is exposed through the SQL surface and the
`MemoryProvider::export` Rust API. A future CLI command (`cellar
memory export`) and a button in the Memory tab will wrap it.

```sql
-- Quick read-only "give me every chunk in the last 7 days" query.
SELECT id, created_at, kind, caller_id, content
FROM memory_chunks
WHERE created_at > datetime('now', '-7 days')
ORDER BY created_at DESC;
```

The bundle is portable — restoring is a SQL `INSERT` plus rebuilding
the vector index, which a future `cellar memory import` command will
automate.

## Purging your memory

Two purge flows exist:

- **Targeted purge** — `MemoryProvider::delete_matching(predicate)`
  removes every chunk matching the predicate (kind, caller, session,
  project root, time bound, content substring). An empty predicate is
  a guard-railed no-op rather than a "delete everything" footgun.
- **Total purge** — `MemoryProvider::purge_all()` recreates the memory
  tables. Logs to a separate audit file so the purge itself is
  durable.

CLI:

```sh
# Stop the daemon first so the SQLite writer is quiescent.
launchctl unload -w ~/Library/LaunchAgents/com.cellar.daemon.plist

# Remove the file.
rm ~/.cellar/memory.sqlite ~/.cellar/memory.sqlite-wal ~/.cellar/memory.sqlite-shm

# Restart the daemon — it recreates an empty DB on first write.
launchctl load -w ~/Library/LaunchAgents/com.cellar.daemon.plist
```

The Memory tab will surface "Forget everything Cellar remembers about
me" as a single-click flow once the Tauri app ships (Phase 4 of the
memory plan).

## Targets and budgets

| Operation                              | p50 target | p95 target |
|----------------------------------------|------------|------------|
| `retrieve` (k=8, session-tier corpus)  | 20 ms      | 60 ms      |
| `retrieve` (k=8, full corpus)          | 50 ms      | 150 ms     |
| `write` (single chunk, inline embed)   | 8 ms       | 30 ms      |
| `write_batch` (32 chunks)              | 100 ms     | 250 ms     |
| `summarize_session` (Haiku, ~3000 chunks) | 4 s     | 10 s       |
| `run_aging_sweep`                      | 200 ms     | 800 ms     |

These are reasonable on M2 Pro. `cellar doctor memory` checks the write
p95 against the 30 ms budget and warns when corpus growth approaches
the configured cap (default 500k chunks).

## Status

| Phase                          | Status |
|--------------------------------|--------|
| 0 — Foundations                | ~95% complete |
| 1 — Persistence and writes     | 100% complete |
| 2 — Retrieval                  | ~75% complete |
| 3 — Sessions, summaries, rollups | not started |
| 4 — Multi-agent + MCP surface  | not started |
| 5 — Quality, eval, polish      | this PR (~80% complete after merge) |

Phase 5 ships:

- `cellar eval memory` CLI subcommand and a 19-pair placeholder
  corpus in `eval/memory/queries.jsonl`. Grow toward 200 pairs over
  time; see `eval/memory/README.md`.
- The `redact_memory` named action — sugar layer over `veto` on
  memory writes, surfaced through the NL compiler.
- `cellar doctor memory` subcommand — DB readable, corpus inside
  cap, embedding model present, write p95 inside budget.
- This document.

The 30-task delegation benchmark, MCP `cel_remember` / `cel_recall` /
`cel_forget` surfaces, and the Memory tab UI are deferred to later
phases.
