# Cellar Worker Protocol

Wire protocol spoken between `cellar` / `cel-goal-runner` (the client) and `cellar-worker` (the remote execution daemon). This is the contract that every execution backend — self-hosted Docker, Cellar Cloud, EC2 Mac pool — must honor.

Status: **v1-draft** — pinned during Phase 1 (Milestone 1.0). Breaking changes bump major.

## Design principles

- **Mirror the MCP tool surface.** A worker can execute anything the local MCP server can; the tool arguments and result schemas are identical. Clients written against the local MCP already understand the remote worker.
- **Stateless where possible.** A goal is submitted, a `job_id` is returned, results are polled or streamed. The server holds per-job state; the client holds none beyond the `job_id`.
- **JSON everywhere.** No protobuf, no custom binary format. Debuggable with `curl`.
- **Bearer auth, optional TLS.** If `CEL_WORKER_TOKEN` is set, every non-health request must include `Authorization: Bearer <token>`. If unset, the worker accepts unauthenticated requests (useful for localhost and isolated networks).
- **Ephemeral by default.** Milestone 1.0 stores jobs in memory. Later milestones persist to SQLite. A client must not assume jobs survive worker restarts.

## Endpoints

### `GET /health`

Liveness check. Always unauthenticated.

Response: `200 OK`
```json
{"status": "ok", "version": "0.1.0"}
```

### `POST /v1/goals`

Submit a new goal for execution.

Request body:
```json
{
  "goal": "Open github.com and search for 'anthropic'",
  "config": {
    "max_steps": 30,
    "step_delay_ms": 500,
    "timeout_ms": 120000,
    "llm_provider": "gemini",
    "llm_model": "gemini-2.5-flash"
  }
}
```

`goal` is required. `config` is an optional `GoalConfig` override — any fields omitted use worker-side defaults. See `cel-goal-runner/src/config.rs` for the full schema.

Response: `202 Accepted`
```json
{
  "job_id": "job_1739814245123456",
  "status": "queued",
  "created_at": 1739814245
}
```

`created_at` is unix epoch seconds.

### `GET /v1/jobs/{id}`

Poll a job's status.

Response: `200 OK`
```json
{
  "job_id": "job_1739814245123456",
  "status": "queued" | "running" | "succeeded" | "failed",
  "created_at": 1739814245,
  "updated_at": 1739814250,
  "result": { ... GoalResult when status == succeeded ... },
  "error": "message" | null
}
```

`result` is populated on terminal states. On `failed`, `error` contains a human-readable message; `result` may still be populated with partial state.

`404 Not Found` if `job_id` is unknown.

### `GET /v1/jobs/{id}/stream` (Phase 1.1 — not yet implemented)

Server-Sent Events stream of live job updates. Each event is JSON with a `type` discriminator:

```
event: status
data: {"status": "running", "step": 3, "phase": "plan"}

event: mental_model
data: { ... Cortex snapshot ... }

event: action
data: { ... ActionRecord ... }

event: done
data: { ... final GoalResult ... }
```

Clients that don't need live updates can always fall back to polling `GET /v1/jobs/{id}`.

### `POST /v1/tools/{tool_name}` (Phase 1.1 — not yet implemented)

Low-level passthrough for `cel_see` / `cel_act` / `cel_think` / `cel_perceive`. Arguments and return schemas match the MCP tool definitions verbatim.

Use cases: a client that already speaks MCP and wants to treat the worker as a drop-in for the local MCP server.

## Job lifecycle

```
     submit
       │
       ▼
   [queued] ─────► [running] ─────► [succeeded]
                        │
                        └─────────► [failed]
```

Milestone 1.0: jobs transition `queued → running → succeeded` with a stubbed result immediately (no actual goal execution yet). Milestone 1.1 wires the local Goal Runner and makes `succeeded`/`failed` correspond to real outcomes.

## Auth

Bearer token via `Authorization: Bearer <token>`.

The worker reads its expected token from `CEL_WORKER_TOKEN`. If that env var is unset, the worker accepts requests with no auth header (localhost / trusted network mode). If set, requests without a matching token return `401 Unauthorized`.

TLS: not terminated by the worker itself. Front with nginx / Caddy / ALB / Cloudflare for public deployments. Localhost and intra-VPC deployments can run plaintext.

## Error format

All non-2xx responses return:
```json
{"error": {"code": "string_tag", "message": "human-readable detail"}}
```

Codes used in v1-draft:
- `unauthorized` — missing or invalid bearer token (`401`)
- `not_found` — unknown job id (`404`)
- `bad_request` — malformed request body (`400`)
- `internal` — unexpected server error (`500`)

## Versioning

- `/v1/` prefix is the major version. Breaking changes bump to `/v2/`.
- Optional fields added to request/response bodies are **not** breaking.
- Field removals or type changes **are** breaking.
- The worker advertises its protocol version via `GET /health`'s `version` field.

## Client reference implementation

`cellar-worker/src/client.rs` — thin reqwest wrapper, used by the eventual `RuntimeBackend::Remote` path in `cel-goal-runner`.

## Related

- [ROADMAP.md](ROADMAP.md) — Phase 1 Milestone plan.
- [deployment.md](deployment.md) — topology and where the worker fits.
- `cel/cel-goal-runner/src/runtime_backend.rs` — client-side config types.
- `cellar-worker/` — server + client + protocol wire types.
