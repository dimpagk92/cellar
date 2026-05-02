# Claude Code Working Memory

This repository should be built around three layers:

- `Adapters`
- `CEL / crates`
- `Agents`

The durable value is in device understanding and execution, not in owning one planner.

## Repo Direction

- Adapters should be easy to build and extend.
- CEL should own context fusion, stream normalization, execution, and adapter routing.
- Agents should be pluggable: LangGraph, Mastra, Codex, Claude Code, GPT, Gemini, Cursor, n8n, or future in-house runtimes.

## What CEL Owns

- fused context from AX, CDP, vision, signals, network, audio, and adapters
- screenshot capture and runtime capability reporting
- adapter lifecycle and dispatch
- canonical action execution
- stable MCP / CLI / SDK / N-API surfaces
- memory/context management when it serves understanding and execution

## What CEL Does Not Need To Own Right Now

- one mandatory planner
- one mandatory orchestration runtime
- retry / branching / checkpoint policy as a repo-defining concern

Built-in planners and runners can exist, but they should be treated as clients, examples, or transitional implementations unless proven otherwise.

## Boundary Rules

- Keep the agent boundary generic.
- Preserve stable context, action, result, and adapter contracts.
- Keep improving AX and the shared crates even when an app later gets an adapter.
- Prefer app-specific structured truth in adapters over forcing everything through generic UI perception.
- Do not make LangGraph, Mastra, or any single runtime the identity of the platform.
- Do not design evals so they only make sense for one agent backend.

## Eval Rule

Prefer agent-agnostic evals that test CEL and adapter capabilities.
Runtime-specific evals are allowed, but they should be clearly isolated and secondary.

## Files To Read First

- [docs/adapters-cel-agents.md](docs/adapters-cel-agents.md)
- [docs/architecture.md](docs/architecture.md)
- [eval/scenarios/README.md](eval/scenarios/README.md)

Keep this file and `AGENTS.md` aligned.
