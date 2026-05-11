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

## Browser perception: TS adapter + Rust adapter (May 2026)

Browser DOM perception is provided by **two parallel adapter implementations sharing one conceptual contract**:

- **`adapters/browser/`** (TypeScript, `runtime: "process"`) — full Playwright + raw CDP hybrid with mutation tracking, four watchdogs (popup / download / security / storage), URL mapping. Used by the LangGraph runtime via the `ProcessDriver` adapter loader. ~1500 LOC.
- **`adapters/browser-rs/`** (Rust, `runtime: "native"`) — implements the same `AdapterDriver` trait in-process. Uses `cel-cdp` as the transport. Both eager (`with_cdp_client`, eval harness) and lazy (`new`, MCP server) construction modes. ~750 LOC.

Both adapters declare `truth_surface: "browser_dom"` via the shared `adapters/browser/manifest.json` so the cortex tags their elements as `ContextSource::Cdp` (distinct from `NativeApi` for telemetry). Both produce `dom:*` element_ids, though the exact format differs (see "Known divergence" below).

**Why two adapters:**
- The LangGraph runtime is JS-native and already pays the IPC cost of `ProcessDriver` for every adapter — Playwright + watchdogs make sense in that out-of-process driver.
- The canonical (Rust) runtime can't shell out to a JS process per perception tick without losing latency budget. It needs an in-process Rust peer.

**Migration path toward unification:**
1. ~~Today: two adapters, two `adapter.json` files, no shared schema.~~ *(done — see steps 2–3)*
2. ~~Soon: link the two `adapter.json` files (`manifest_alias` field naming the peer) so docs / discovery / dashboards can present them as one logical adapter with two implementations.~~ **Done (Cut A, May 2026):** both `adapter.json` files declare `manifest_alias` bidirectionally; `cel_cortex::group_paired_manifests` surfaces the pair. One-way aliases are *not* paired — they surface as two unpaired rows so typos stay loud.
3. **Done (Cut B, May 2026):** shared fields (`name`, `platform`, `app_patterns`, `truth_surface`, `confidence`, `verification.truth_surface`) live in `adapters/browser/manifest.json`. Both `adapter.json` files reference it via `manifest_extends` and the cortex `load_manifest` resolves + merges. The Rust adapter's `default_browser_manifest()` `include_str!`s both layers and runs them through `cel_cortex::merge_manifest_layers` — same JSON, same merge, same result whether you're reading via disk discovery or in-Rust construction. Drift is now structurally impossible for shared fields. Element-ID divergence (below) still requires its own resolution before Cut C.
4. Later: resolve the element-ID format divergence (see "Known divergence" below) — either port the TS scheme to Rust or vice versa. Verify with a golden test that survives both mappers. Until that lands, scenarios that hard-code one format won't match against the other adapter.
5. Later: extract the JS-side perception walker to a smaller core (DOM extractor + element mapper) and either port to Rust or call from Rust via a thin shim. Keep watchdogs/Playwright as TS-only enrichments the Rust adapter can opt into.
6. Eventually: one adapter manifest with two implementations declared, picked by runtime preference. The TS one keeps Playwright richness; the Rust one keeps in-process latency.

**Capabilities not exposed as actions today.** Some browser-adapter capabilities aren't planner-callable because the cortex ProcessDriver protocol has only `execute(action, params)` for dispatch — no symmetric `query(name, params)` path:

- **Network events** — `BrowserAdapter.getNetworkEvents()` returns the buffered CDP Network domain log, but it's a method on the JS object, not an `execute()` arm. Same for `getPopupEvents()` (dialogs auto-handled by the popup-watchdog), `getDownloadEvents()`, `getSecurityEvents()`, and `storageState`. Adding planner access requires extending the protocol with a `query` method; tracked as a follow-up.
- **Dialog accept/dismiss** — handled automatically by `popup-watchdog.ts` via `page.on('dialog')` with `auto_accept_confirm` / `auto_dismiss_beforeunload` config flags. The planner doesn't choose accept-vs-dismiss on a per-dialog basis today.
- **Console capture** — not implemented. No console watchdog exists; `page.on('console')` is not wired.

**Known divergence (element_id format).** The two element-mappers produce structurally different `dom:*` ids today:
- **TypeScript** (`adapters/browser/src/element-mapper.ts:217` `generateId`) → `dom:${raw.id}` if the element has an HTML id, else `dom:${tag}:${backendNodeId}`. Iframe and shadow-DOM elements get `iframe:`/`shadow:` prefixes instead, dropping the `dom:` namespace entirely.
- **Rust** (`adapters/browser-rs/src/element_mapper.rs:42` `dom_element_to_context_element`) → `dom:${element_type}:${id_part}` always, where `id_part` falls back through HTML id → name → aria-label → text → `n{backend_node_id}` → `i{walk_index}`.

A `<button id="submit-btn">` becomes `dom:submit-btn` (TS) vs `dom:button:submit-btn` (Rust). A scenario asserting `target_contains: "submit-btn"` matches both, but `target_id: "dom:submit-btn"` only matches one. This is a real source of cross-runtime portability bugs and is the gating issue for step 4.

The features that exist only on the TS side today (mutation tracking, popup/download/security/storage watchdogs, hybrid Playwright snapshot) are the long-term backlog for the Rust adapter to absorb. Pure DOM perception is at parity for *what* gets surfaced — the divergence is in *how* it's identified.

## Repository Reading Order

When making design decisions, read these first:

1. [docs/adapters-cel-agents.md](./adapters-cel-agents.md)
2. [docs/architecture.md](./architecture.md)
3. [AGENTS.md](../AGENTS.md) or [CLAUDE.md](../CLAUDE.md)
4. [eval/scenarios/README.md](../eval/scenarios/README.md)

If another document conflicts with this one, treat this document as the current repo direction and update the conflicting document.
