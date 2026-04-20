# Deployment Topology

How Cellar is deployed — from a single Mac to a managed cloud — and where Docker fits.

## The Two Axes

Cellar has two independent choices:

1. **Execution backend** — where the automation *runs* (reads screen, clicks, types).
2. **Model backend** — where the LLM *runs* (plans, extracts, validates).

They compose freely. A user on their Mac can use a local Ollama/Gemma 4 model for extraction while delegating execution to a remote browser worker for scale. The model's location has nothing to do with the automation's location.

This orthogonality is a design constraint: any new execution backend must work with every model backend, and vice versa.

## Execution Backends

### Local (today, Phase 0) ✅

The dilipod CLI / MCP server runs on the user's Mac. `cel-goal-runner` executes in-process. macOS AX, screen capture, CDP, adapters all run against the local machine.

- Full access to the user's actual desktop (Excel, Slack, Finder, SAP GUI, etc.).
- No infrastructure to manage.
- Cannot scale horizontally or run unattended while the user is away.

Current entry points: `dilipod run`, `dilipod mcp` (MCP server for Claude Code / Cursor).

### Remote worker (Phase 1) ⏳

A `cellar-worker` daemon runs on a server the user controls. The user's local dilipod sends goals over HTTP; the worker executes in its own container / VM / Mac and streams results back.

- Runs unattended, 24/7.
- Fans out across many workers for bulk jobs.
- Headless browser workloads are the sweet spot.
- Cannot access the user's personal desktop.
- macOS workloads need real Mac hardware (Docker can't host macOS).

Configured via `~/.cellar/config.toml`:

```toml
[runtime]
backend = "remote"
url = "http://my-server:7777"
token = "..."
```

Or env vars: `CEL_RUNTIME_BACKEND=remote`, `CEL_RUNTIME_URL=...`, `CEL_RUNTIME_TOKEN=...`.

### Managed cloud — Cellar Cloud (Phase 3) 📋

Same wire protocol as the remote worker, operated by Cellar. The customer gets a hosted fleet, billing, and a dashboard. No DevOps work.

Key insight: **Managed = Remote pointed at our infrastructure.** We don't build a second codepath. One worker protocol, different operator.

## Docker: What It Covers, What It Doesn't

Docker is the **package format for the Linux remote worker**. It's how we ship a reproducible browser-automation runtime that anyone can `docker run`.

**Covered by Docker:**
- Browser automation (Chromium via Playwright).
- Linux native apps via Xvfb + AT-SPI2 (already supported in `cel-accessibility`).
- Parallel workers, ephemeral jobs, CI integration.

**Not covered by Docker:**
- macOS applications. Apple's EULA forbids macOS on non-Apple hardware; Docker has no macOS container runtime.
- Any workload that needs GPU acceleration beyond Chromium.

For macOS automation at scale: EC2 Mac instances, MacStadium, or a rack of Mac minis. Same worker protocol runs on top — the transport doesn't care about the kernel.

## Model Backends

Independent of where automation runs. Configured via env vars or `~/.cellar/config.toml`.

| Provider | Where it runs | Use case | Cost model |
|---|---|---|---|
| **Ollama (local)** | User's Mac (or on worker) | Privacy, offline, high-volume cheap calls | Free after disk cost |
| **Gemini / Anthropic / OpenAI** | Cloud (BYOK) | Best quality for planning; user owns the relationship | Pay-per-token to provider |
| **Managed (Phase 3)** | Cellar Cloud proxy | Single bill, simpler for teams, tier-based | Subscription / usage-bundled |

### Per-role routing

Cellar supports role-level model routing so you can mix providers within a single job:

```bash
CEL_LLM_PLANNER_PROVIDER=anthropic      # Sonnet for step planning
CEL_LLM_PLANNER_MODEL=claude-sonnet-4-20250514
CEL_LLM_OBSERVER_PROVIDER=ollama        # Gemma for cheap observation
CEL_LLM_VISION_PROVIDER=gemini          # Flash for screenshots
```

Roles: `Planner`, `Observer`, `Vision`, `General`, `Validator`, `Localizer`, `Orchestrator`. See `cel-llm/src/config.rs`.

## Configuration Precedence

For any LLM field (`provider`, `model`, `endpoint`, `api_key`, `temperature`):

1. Role-specific env var — `CEL_LLM_PLANNER_MODEL`
2. Base env var — `CEL_LLM_MODEL`
3. Provider-specific env var — `GEMINI_API_KEY`, `ANTHROPIC_API_KEY`, etc.
4. `~/.cellar/config.toml` `[llm]` section — written by `dilipod init`
5. Provider defaults — e.g., `gemini-2.5-flash` for Gemini, `gemma4:e4b` for Ollama

Runtime backend follows a parallel chain (Phase 1):

1. `CEL_RUNTIME_BACKEND` + `CEL_RUNTIME_URL` + `CEL_RUNTIME_TOKEN`
2. `[runtime]` section of `~/.cellar/config.toml`
3. `Local` default

## Common Deployment Shapes

### Shape A: Solo developer, all local
- Execution: Local Mac
- Model: BYOK Gemini via `GEMINI_API_KEY`, or local Ollama for privacy

Setup: `node cli/dist/index.js init` → option 1 (Gemini) or 4 (Gemma 4 E4B).

### Shape B: Team of N, shared worker on Hetzner
- Execution: Remote worker on Hetzner box
- Model: BYOK Anthropic, keys injected into the worker's env

Setup: `docker run -e ANTHROPIC_API_KEY -e CEL_LLM_PROVIDER=anthropic -p 7777:7777 cellar/worker` on the Hetzner box, then each teammate's `dilipod` points at it via config.toml.

### Shape C: Enterprise, managed + BYOK
- Execution: Cellar Cloud (managed fleet)
- Model: BYOK Anthropic, keys stored in the customer's Cellar vault

Setup (Phase 3): log in with SSO, upload API keys once, run workflows from dilipod or the Cellar Cloud dashboard.

### Shape D: Privacy-sensitive, fully local
- Execution: Local Mac
- Model: Ollama + Gemma 4 E4B

Setup: `dilipod init` → option 4. Nothing leaves the machine.

## Related

- [ROADMAP.md](ROADMAP.md) — phased delivery of each execution backend.
- [oss-boundary.md](oss-boundary.md) — which parts of the above are OSS vs commercial.
- [quickstart.md](quickstart.md) — hands-on setup for Shape A.
- [architecture.md](architecture.md) — internal runtime architecture (Cortex / Goal Runner / Planner / Adapters).
