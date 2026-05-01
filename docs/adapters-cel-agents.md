# Adapters / CEL / Agents

Date: April 24, 2026

## North Star

Cellar should be built as a three-layer system:

1. `Adapters` — app- and domain-specific capabilities
2. `CEL / crates` — device understanding, context fusion, and execution
3. `Agents` — pluggable planners and orchestrators

The durable value of the repository is not "our planner."
The durable value is:

- understanding the device
- fusing context from multiple streams
- exposing stable execution primitives
- routing execution into the right substrate or adapter
- making those capabilities usable by any agent

For now, planning should be treated as pluggable.

## Layer 1: Adapters

Adapters are how app-specific intelligence enters the system.

Examples:

- Numbers
- Figma
- Slides / PowerPoint
- Cursor
- Docker Desktop
- Slack
- browsers with richer domain logic

Adapters should be designed so:

- first-party maintainers can extend existing adapters
- third parties can build new adapters
- adapters can be used from any agent runtime, not only one built-in planner

Adapters are where app-specific structured truth should live.

Example:

- `AX` is good for generic desktop navigation, windows, dialogs, focus, and controls
- a `Numbers` adapter should expose spreadsheet/model truth such as deterministic cell reads and writes

So the rule is:

- keep improving the base AX and stream fusion layers
- but move application-specific structured operations into adapters

## Layer 2: CEL / Crates

CEL is the core platform layer.

It owns:

- context fusion across AX, CDP, vision, signals, network, audio, and adapters
- stream normalization into stable shared types
- freshness, anomaly, and state tracking
- adapter lifecycle and dispatch
- canonical action execution
- MCP / CLI / SDK / N-API tool surfaces
- memory and context management when those serve device understanding and execution

CEL should not be defined by one planner or one orchestration framework.

Built-in planners and runners may still exist in-tree, but they are clients of CEL, not CEL's identity.

The CEL boundary should stay useful even if:

- LangGraph disappears
- Mastra is replaced
- Claude Code becomes the main user
- someone drives CEL through Codex, GPT, Gemini, Cursor, or n8n

## Layer 3: Agents

Agents are consumers of CEL.

Examples:

- LangGraph
- Mastra
- Codex
- GPT-based tool callers
- Claude
- Claude Code
- Gemini
- Cursor
- n8n
- future in-house runtimes

Agents can use:

- MCP
- CLI entrypoints
- SDKs
- N-API / programmatic bindings
- adapter-backed tools exposed through CEL

Agents own:

- planning
- orchestration
- retries
- branching
- checkpointing
- human approval policies
- done / stop policies

CEL should support them all without forcing one planning style.

## Design Rules

1. Keep planning pluggable.
   Built-in planner code is optional, reference, or transitional unless proven otherwise.

2. Keep contracts stable.
   Canonical context, actions, results, and adapter interfaces matter more than any one runtime.

3. Prefer app truth over UI guesswork.
   If an app has a structured model, that should live in an adapter instead of being forced through AX alone.

4. Keep AX strong anyway.
   AX remains the cross-app substrate for generic desktop understanding, handoffs, dialogs, and focus management.

5. Make adapters extensible.
   The platform should make it easy to add or extend adapters without rewriting CEL or a planner.

6. Treat agent runtimes as clients.
   LangGraph is an integration option. So is Mastra. So are MCP-native agents. None of them should define the platform boundary.

## Eval Principle

Evals should primarily measure CEL and adapter capabilities, not loyalty to one planner.

That means:

- prefer agent-agnostic scenarios where possible
- evaluate device understanding and execution contracts
- isolate runtime-specific evals under clearly named folders when needed
- keep scenario formats reusable across different agent backends

Runtime-specific evals are allowed, but they should be secondary.
The main eval question should be:

"Can any competent agent use CEL to do this task reliably?"

not:

"Did one specific planner implementation pass?"

## Current Implications

- `Numbers` should be treated as an adapter-backed surface, not a pure AX problem.
- `cel-planner` and in-tree runners are useful, but they are not the main architectural bet.
- LangGraph work should be framed as one agent integration, not the definition of the repository.
- MCP and tool surfaces should stay generic enough for many agents.
- Future work should make the core crates and adapters stronger before deepening planner ownership.

## Repository Reading Order

When making design decisions, read these first:

1. [docs/adapters-cel-agents.md](./adapters-cel-agents.md)
2. [docs/architecture.md](./architecture.md)
3. [AGENTS.md](../AGENTS.md) or [CLAUDE.md](../CLAUDE.md)
4. [eval/scenarios/README.md](../eval/scenarios/README.md)

If another document conflicts with this one, treat this document as the current repo direction and update the conflicting document.
