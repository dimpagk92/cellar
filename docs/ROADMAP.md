# Cellar Roadmap

Forward-looking plan for Cellar and the CEL runtime. Living document — priorities shift as we learn from users and customers. Supersedes scattered issue wishlists.

Last updated: 2026-04-17.

## Legend

- ✅ Done
- 🚧 In progress
- ⏳ Next up
- 📋 Planned
- 💭 Under consideration

## Context: Two Axes

Cellar has two independent deployment dimensions:

1. **Execution backend** — *where* the automation runs
2. **Model backend** — *where* the LLM runs

| Execution \ Model | Local (Ollama / Gemma) | BYOK API | Managed API |
|---|---|---|---|
| **Local Mac** | ✅ (private + offline) | ✅ (most common today) | 📋 (Phase 3) |
| **Remote worker** | ⏳ (Phase 1) | ⏳ (Phase 1) | 📋 (Phase 3) |
| **Managed cloud** | N/A | 📋 (Phase 3) | 📋 (Phase 3) |

Design constraint: the axes are orthogonal. Adding a new execution backend must not require changes to model resolution, and vice versa.

See [deployment.md](deployment.md) for the full topology reference.

---

## Phase 0 — Current State ✅

Shipped as of April 2026.

| Capability | Reference |
|---|---|
| Local execution on macOS with AX, CDP, screen capture, input injection | `cel/cel-goal-runner/`, `cel/cel-accessibility/` |
| MCP server exposing `cel_see` / `cel_act` / `cel_think` / `cel_perceive` | `mcp-server/` |
| Multi-provider LLM support — OpenAI, Anthropic, Gemini, HuggingFace | `cel/cel-llm/src/config.rs` |
| **Ollama provider** (local Gemma 4 E4B by default) | `ProviderKind::Ollama` |
| **Interactive first-run setup** (`cellar init`) | `cli/src/commands/init.ts` |
| **Config file** at `~/.cellar/config.toml` — env vars take precedence | `LlmProviderConfig::from_config_file` |
| Per-role LLM routing (Planner / Observer / Vision / Validator / …) | `LlmRole` enum |
| Benchmark suite — Hybrid (5 tasks) + general web (50+) | `benchmarks/` |
| First-party adapters — Excel, SAP GUI, Bloomberg, MetaTrader | `adapters/` |

---

## Phase 1 — Worker Protocol & Docker Image ⏳

**Target: 4–8 weeks.** Make Cellar runnable outside the user's Mac for browser-first workloads.

### Goals

- Define a stable worker protocol mirroring the MCP tool surface.
- Ship a `cellar/worker` Docker image — headless Linux CEL with Playwright/Chromium.
- Add a `RuntimeBackend` abstraction so `cellar` can target a remote worker.

### Deliverables

1. **`RuntimeBackend` enum in `cel-goal-runner`**

    ```rust
    pub enum RuntimeBackend {
        Local,                                    // today's path
        Remote { url: String, token: String },    // talks to worker over HTTP
    }
    ```

    Plus a `WorkerClient` trait so Remote is a drop-in replacement for Local.

2. **Worker protocol spec** — `docs/worker-protocol.md`
    - `POST /v1/goals` → submit goal, get `job_id`
    - `GET  /v1/jobs/{id}` → status + final result
    - `WS   /v1/jobs/{id}/stream` → live mental-model updates, action outcomes
    - `POST /v1/tools/{tool_name}` → low-level passthrough for `cel_see` / `cel_act` / `cel_think` / `cel_perceive`
    - Auth: bearer token; TLS optional but recommended
    - Wire format: JSON, matches existing MCP tool argument/result schemas verbatim

3. **`cellar-worker` crate** — new Rust binary that wraps `cel-goal-runner` + `cel-cortex` behind the HTTP server.

4. **Docker worker image** (`docker/worker/Dockerfile`)
    - Base: `debian:bookworm-slim` + Chromium + Xvfb
    - Accessibility: AT-SPI2 (already supported)
    - Exposed port: `7777`
    - Size target: < 800 MB

5. **Config file extension** — `~/.cellar/config.toml` gets a `[runtime]` section:

    ```toml
    [runtime]
    backend = "remote"
    url = "http://my-hetzner-box:7777"
    token = "..."
    ```

    And matching env vars (`CEL_RUNTIME_BACKEND`, `CEL_RUNTIME_URL`, `CEL_RUNTIME_TOKEN`).

6. **Integration tests** — end-to-end: local cellar → remote worker (docker-compose) → browser goal → verified outcome. Live in `e2e/remote/`.

### Success criteria

- `docker run -p 7777:7777 cellar/worker` followed by `cellar run <goal> --remote http://localhost:7777` succeeds on a browser task.
- All four MCP tools work through the remote path.
- Latency overhead vs local: < 50 ms per tool call on localhost, < 200 ms over LAN.
- 100% task-parity on the `general-web` benchmark subset (hybrid suite stays local-only for now — desktop apps).

### Open questions

- **Protocol**: HTTP+SSE (simpler, ubiquitous tooling) vs gRPC (better streaming, heavier). Leaning HTTP+SSE for v1.
- **Session lifecycle**: ephemeral worker per job (easy, safe) vs pooled workers (faster, stateful). v1 = ephemeral; pool as optimization in Phase 2.
- **macOS**: explicitly out of scope for this phase. Docker can't host macOS. Covered in Phase 3 via EC2 Mac / MacStadium.

---

## Phase 2 — Production-Ready Remote 📋

**Target: 2–3 months after Phase 1.**

- Rate limiting + quota per token
- Prometheus metrics + structured logs (`tracing` → OTEL)
- TLS + certificate auto-rotation (ACME / Caddy sidecar)
- Deployment guides: Hetzner, Fly.io, AWS ECS, self-hosted K8s
- Health checks + graceful shutdown
- Worker pool mode: a thin control-plane orchestrates N workers behind a load balancer
- Per-tenant filesystem isolation + clipboard scoping

Depends on [`PRODUCTION_HARDENING.md`](../PRODUCTION_HARDENING.md) landing.

---

## Phase 3 — Managed Cloud (Cellar Cloud) 📋

**Target: 6+ months. Triggered by customer demand, not a calendar date.**

- **Control plane** (new, private repo): auth, billing, fleet orchestration, workflow registry backend.
- **Managed LLM proxy** — bundled model access. "Use Cellar's models, one bill." Opt-in.
- **macOS-in-cloud** — pool of EC2 Mac / MacStadium hosts for customers who need native macOS automation remotely. Same wire protocol as Docker workers.
- **Hosted workflow marketplace** — backend for the community registry (client exists at `registry/`).
- **Wire protocol**: reuses Phase 1. Managed = Remote pointed at our infra. No new protocol.

---

## Phase 4 — Hard OSS/Commercial Repo Split 💭

**Target: when the commercial boundary stops moving. Likely 12+ months.**

Today: single private monorepo, OSS subset mirrored to `github.com/dimpagk92/cellar` on each release (see [oss-boundary.md](oss-boundary.md)).

Later: true two-repo split with the commercial product depending on published OSS crates/packages. Deferred because:

- The commercial/OSS boundary is still drifting as we figure out what's paid.
- Monorepo velocity is higher than a two-repo split would allow for a small team.
- The mirror pattern already produces a clean public artifact for grants (NGI) and adoption.

---

## Explicitly Not on the Roadmap

To avoid over-committing:

- **Full Windows UI Automation bridge** — designed, not prioritized. Linux workers cover Phase 1. Windows waits for demand.
- **Training or fine-tuning our own models** — not a differentiator. Frontier models + Gemma 4 locally cover the field.
- **Mobile automation (iOS / Android)** — out of scope indefinitely.

---

## Related Plans (active, parallel tracks)

- [`HYBRID_SUITE_FINALIZATION_PLAN.md`](../HYBRID_SUITE_FINALIZATION_PLAN.md) — benchmark stability. Blocks Phase 1 "buyer-ready proof artifact."
- [`PRODUCTION_HARDENING.md`](../PRODUCTION_HARDENING.md) — security/robustness fixes. Blocks Phase 2.
- [`RUNTIME_KERNEL_CONSOLIDATION_PLAN.md`](../RUNTIME_KERNEL_CONSOLIDATION_PLAN.md) — orchestration sprawl reduction. Ongoing, unblocks faster Phase 1 iteration.

---

## Changelog

- 2026-04-17: initial roadmap draft. Captures the OSS + runtime backend direction after Ollama/`cellar init` landed.
