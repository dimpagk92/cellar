# Bring Your Own LLM

CEL should never be a hidden LLM dependency for an external agent. If an agent caller is paying for their own tokens, they keep control. This doc is the practical guide to "how do I use CEL from my agent without surprising billing or behavior from `cel-llm`?"

It covers four usage patterns, what CEL does in each, and the concrete env-var and config flags to reach each one.

## How CEL Uses LLMs Today

CEL has its own LLM layer in `cel/cel-llm/` (`client.rs` + `config.rs`). It is used in two places:

1. **`cel_think`** — the optional built-in planner and memory layer. When an agent explicitly invokes `cel_think` (e.g., to run a goal end-to-end inside CEL), `cel-llm` drives reasoning, planning, and replanning.
2. **Vision fallback inside Cortex / goal-runner** — when perception is stale or targets are ghosted, the goal-runner may call a vision LLM to ground a missing element. This is gated by the per-run `enable_vision: bool` flag in `GoalConfig` and by Cortex's `vision_needed` signal; see `cel/cel-goal-runner/tests/vision_gating.rs`.

That's it. `cel-llm` is **not** invoked by:

- `cel_see` (read the screen)
- `cel_act` (execute actions, including custom adapter actions)
- `cel_perceive` (always-on perception / Cortex)

So external agents that do their own planning and drive CEL as an execution substrate will never implicitly pay CEL's LLM tokens — as long as they don't call `cel_think` and they leave `enable_vision: false` if they take the goal-runner path.

## Pattern A — External Agent Owns Reasoning (Recommended)

**Use this when**: you have your own agent runtime (LangGraph, Mastra, Claude Code, Codex, GPT tool calling, Gemini, Cursor, n8n, or in-house). You want CEL for perception and action only.

**Flow**:

```
Your agent → LLM of choice → cel_see (read) → your decision → cel_act (execute)
                                      ↑                              ↑
                            no CEL LLM invoked          no CEL LLM invoked
```

**Setup**:

- Connect to CEL via MCP, CLI, SDK, or N-API.
- Do not call `cel_think`.
- If you use the goal-runner path, set `enable_vision: false` in `GoalConfig`.
- No `CEL_LLM_*` env vars needed. `cel-llm` stays idle.

**Cost**: only your own LLM calls. CEL adds zero token cost.

**Latency**: bounded by your agent's planning step plus whatever CEL takes for perception (sub-second for `cel_see`) and action execution.

This is the pattern the three-layer north star ([adapters-cel-agents.md](./adapters-cel-agents.md)) is designed around.

## Pattern B — Agent Uses Its Own Provider, but Lets CEL Handle Vision Grounding

**Use this when**: you want CEL's goal-runner to do the whole loop (observe → plan → act → verify), but you want it to use *your* provider and *your* key, not some default.

**Flow**:

```
CEL goal-runner → cel-llm (with your provider) → plan/observe/validate → act
```

**Setup**: via env vars resolved by `LlmProviderConfig::from_env` in `cel/cel-llm/src/config.rs`:

```bash
export CEL_LLM_PROVIDER=anthropic            # or openai | gemini | ollama | huggingface | custom
export CEL_LLM_API_KEY=sk-ant-...            # your key (or ANTHROPIC_API_KEY / OPENAI_API_KEY / GEMINI_API_KEY etc.)
export CEL_LLM_MODEL=claude-sonnet-4-20250514  # optional; defaults per provider
export CEL_LLM_ENDPOINT=https://...          # optional; defaults per provider
```

Per-role overrides give you finer control. The `LlmRole` enum in `cel-llm` defines seven roles (from `config.rs`):

- `Planner` — reasoning, step planning, self-healing
- `Observer` — quick verification, context analysis
- `Vision` — screenshot interpretation
- `General` — base fallback
- `Validator` — action success/failure judgment
- `Localizer` — visual element grounding
- `Orchestrator` — goal decomposition and replanning

Each role reads `CEL_LLM_{ROLE}_*` env vars, falling back to `CEL_LLM_*`:

```bash
# Use Claude Sonnet for planning, Gemini Flash for observation
export CEL_LLM_PLANNER_PROVIDER=anthropic
export CEL_LLM_PLANNER_MODEL=claude-sonnet-4-20250514
export CEL_LLM_OBSERVER_PROVIDER=gemini
export CEL_LLM_OBSERVER_MODEL=gemini-2.5-flash
```

Provider-specific key fallbacks are also honored:

- `OPENAI_API_KEY`
- `ANTHROPIC_API_KEY` (and `CLAUDE_CODE_OAUTH_TOKEN` as a last resort)
- `GEMINI_API_KEY` / `GOOGLE_GEMINI_API_KEY` / `GOOGLE_API_KEY`
- `HUGGINGFACE_API_KEY` / `HF_API_KEY`

**Cost**: your provider, your rates, your key.

**Latency**: per-role routing is the main knob. Gemini Flash for Observer and Validator saves significant latency over running everything on a premium model.

## Pattern C — Agent Wants No LLM at the CEL Layer, At All

**Use this when**: your agent handles everything, and you want CEL to be an execution-only substrate with zero possibility of an LLM call happening under the hood.

**Flow**:

```
Your agent → cel_see → your LLM → cel_act
                 ↑                     ↑
       no CEL LLM              no CEL LLM
```

**Setup**:

- Do not set any `CEL_LLM_*` env vars. `LlmProviderConfig::from_env` returns `None` and the LLM client is not constructed. (Confirmed in `cel/cel-llm/src/config.rs::from_env`.)
- Do not call `cel_think`.
- If you use the goal-runner path, set `enable_vision: false` in `GoalConfig`.
- Do not call anything in the SDK that would internally invoke `cel-llm`.

**Gap — vision fallback via env flag**:

There is per-run gating (`GoalConfig.enable_vision`), but there is **not currently a global `CEL_VISION_ENABLED=0` or `CEL_DISABLE_VISION=1` env var** that forces vision off regardless of per-run config.

- TODO: confirm this is actually missing (search found `CEL_DISABLE_AUDIO` but no vision analog).
- TODO: if missing, add `CEL_DISABLE_VISION=1` by analogy. This is a prerequisite for a strict "Pattern C" guarantee via env alone; today the guarantee requires either not configuring a provider at all or controlling `GoalConfig` at the call site.

**Cost**: zero at CEL layer.

**Latency**: the lowest of any pattern — no extra LLM hop inside CEL.

## Pattern D — Self-Hosted or OpenAI-Compatible Endpoint

**Use this when**: you're running Ollama, vLLM, LM Studio, a private self-hosted server, or any OpenAI-compatible endpoint.

**Flow**: same as Pattern B, but pointing at your endpoint.

**Setup**:

```bash
# OpenAI-compatible server (vLLM, LM Studio, etc.)
export CEL_LLM_PROVIDER=custom
export CEL_LLM_ENDPOINT=http://localhost:8080/v1/chat/completions
export CEL_LLM_MODEL=your-local-model
# API key optional, depending on the server
export CEL_LLM_API_KEY=dummy
```

```bash
# Ollama (built-in provider preset, defaults to localhost:11434)
export CEL_LLM_PROVIDER=ollama
export CEL_LLM_MODEL=gemma4:e4b   # or llama3, mistral, whatever you have pulled
# No API key needed
```

The `ollama` provider preset has a default endpoint (`http://localhost:11434/v1/chat/completions`) and a default model (`gemma4:e4b`). You can override either.

You can also configure this via `~/.cellar/config.toml` (written by `cellar init`):

```toml
[llm]
provider = "ollama"
model = "gemma4:e4b"
```

Env vars always take precedence over the config file.

**Cost**: your hardware / your hosting bill.

**Latency**: depends on your setup. Local small models can be faster than a cloud round-trip for Observer/Validator roles.

## Cost and Latency Cheat Sheet

| Pattern | CEL token cost | Extra latency inside CEL | Control |
| --- | --- | --- | --- |
| A. External agent owns reasoning | 0 | 0 | Agent owns everything |
| B. Your provider, CEL goal-runner | Your rate | 1 LLM call per role per step | Per-role routing |
| C. No LLM at CEL layer | 0 | 0 | Strict; needs explicit config |
| D. Self-hosted / compatible | Your hosting | Depends | Full control, bring hardware |

## Explicit Commitments

- CEL will not silently call an LLM in `cel_see` or `cel_act` or `cel_perceive`.
- CEL will not hard-require any specific LLM provider. Every path that uses `cel-llm` either accepts user-configured providers or can be bypassed.
- If we ever add a CEL feature that requires an LLM to function, it will be opt-in and documented here.
- `adapter-common` does not depend on `cel-llm`. Adapters never need an LLM to run.

## Also See

- [adapters-cel-agents.md](./adapters-cel-agents.md) — the three-layer north star that makes this separation possible.
- [building-adapters.md](./building-adapters.md) — adapters never talk to an LLM; they're pure I/O.
- [adapter-sdk.md](./adapter-sdk.md) — the adapter contract is LLM-free by design.
- [mcp-server.md](./mcp-server.md) — the MCP surface external agents use.
- `cel/cel-llm/src/config.rs` — the authoritative source for env vars and roles.
