# Claude Code Working Memory

This repository should be built around three layers:

- `Adapters`
- `CEL / crates`
- `Agents`

The durable value is in device understanding and execution, not in owning one planner.

The product direction is CEL as the trust and execution layer for AI-operated computers.
Agents can plan in many ways; Cellar should make their observations, actions, verifications,
and receipts reliable.

## Repo Direction

- Adapters should be easy to build and extend.
- CEL should own context fusion, stream normalization, execution, and adapter routing.
- Agents should be pluggable: LangGraph, Mastra, Codex, Claude Code, GPT, Gemini, Cursor, n8n, or future in-house runtimes.

## What CEL Owns

- fused context from AX, CDP, vision, signals, network, audio, and adapters
- screenshot capture and runtime capability reporting
- adapter lifecycle and dispatch
- canonical action execution
- execution receipts and evidence references for trust/audit (these prove the
  dispatch / observed-execution path, not task completion)
- stable MCP / CLI / SDK / N-API surfaces
- live context fusion and observed effects. The durable cross-turn memory
  *contract* is the `cel-memory` crate (backend: `cel-memory-sqlite`), and
  per-turn LLM brief assembly / budgeting is the `cel-brief` crate — the live
  perception core never owns brief or prompt construction. See
  [docs/architecture.md](docs/architecture.md) § "Context, memory, and briefing
  crates".

## What CEL Does Not Need To Own Right Now

- one mandatory planner
- one mandatory orchestration runtime
- retry / branching / checkpoint policy as a repo-defining concern

Built-in planners and runners can exist, but they should be treated as clients, examples, or transitional implementations unless proven otherwise.

## Boundary Rules

- Keep the agent boundary generic.
- Preserve stable context, action, result, receipt, and adapter contracts.
- Keep improving AX and the shared crates even when an app later gets an adapter.
- Prefer app-specific structured truth in adapters over forcing everything through generic UI perception.
- Do not make LangGraph, Mastra, or any single runtime the identity of the platform.
- Do not design evals so they only make sense for one agent backend.
- Treat `intent -> dispatch -> observed effect -> evidence` as the core trust loop.

## Eval Rule

Prefer agent-agnostic evals that test CEL and adapter capabilities.
Runtime-specific evals are allowed, but they should be clearly isolated and secondary.

## Adapter Layout (June 2026)

Adapters live under `adapters/`. Two parallel languages, same `AdapterDriver` contract:

- **Rust adapters** (in-process via the cortex tick loop, or out-of-process
  via `ProcessDriver` against a `cargo build`-produced binary):
  `adapters/numbers`, `adapters/excel`, `adapters/sap-gui`, `adapters/bloomberg`,
  `adapters/metatrader`, `adapters/browser-rs`, plus the Apple-app productivity
  set `adapters/calendar`, `adapters/mail`, `adapters/messages`, `adapters/notes`,
  `adapters/reminders` (all macOS, AppleScript-backed document-model adapters).
- **TypeScript adapters** (out-of-process via `ProcessDriver`, used by the
  LangGraph runtime): `adapters/browser`.
- **Shared crates**: `cel/cel-adapter-sdk` is the canonical contract — the
  `AdapterDriver` trait, manifest types, `ActionResult`/`AdapterError`, and the
  discovery/registration helpers. It depends only on `cel-context`/`cel-contracts`/
  `cel-cdp` (not the cortex engine); `cel-cortex` depends on it and re-exports its
  surface. `adapters/adapter-common` holds the older, narrower `Adapter` trait,
  retained only for the four Windows-finance adapters + the NAPI registry (pending
  retirement into the SDK). `adapters/cel-adapter-runtime` turns a Rust adapter
  binary into a `ProcessDriver`-compatible stdio-JSON server.

Browser perception is provided by **two** browser adapters that share the same
conceptual contract: `adapters/browser/` (TS, Playwright + watchdogs) and
`adapters/browser-rs/` (Rust, in-process via `cel-cdp`). Both declare
`truth_surface: "browser_dom"` so the cortex tags their elements as
`ContextSource::Cdp`. See `docs/adapters-cel-agents.md` § "Browser perception"
for the unification roadmap.

## IPC surface (daemon JSON-RPC)

Daemon RPC methods are defined ONCE, on the `Handler` trait in
`cellar-ipc/src/handler.rs`. Annotate a method with `#[method("wire.name")]`;
the `#[ipc_dispatch]` proc-macro (`cellar-ipc-macros`) reads the trait and
generates the dispatch route from the signature — so a method without a route
is impossible by construction (that gap is exactly what `events.publish`
hit when it shipped broken). To add a method: add the `#[method(...)]`-annotated
trait method + its params/result types, then implement it on the daemon's
handler. Never hand-wire a dispatch `match` arm.

## Files To Read First

- [docs/adapters-cel-agents.md](docs/adapters-cel-agents.md)
- [docs/building-adapters.md](docs/building-adapters.md) — adapter authoring (Rust + any-language process adapters)
- [docs/adapter-sdk.md](docs/adapter-sdk.md) — the `cel-adapter-sdk` contract + which trait to implement
- [docs/architecture.md](docs/architecture.md)
- [eval/scenarios/README.md](eval/scenarios/README.md)

Keep this file and `AGENTS.md` aligned.
