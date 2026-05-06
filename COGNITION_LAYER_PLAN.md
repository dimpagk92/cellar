# Cognition Layer + Persistent Memory - Trimmed Design Plan

**Status:** Proposal, trimmed to match the current Adapters / CEL-Cortex / Agents architecture
**Author:** dimpagk92 + Claude, revised with Codex
**Closes / reframes:** [dimpagk92/cellar#33](https://github.com/dimpagk92/cellar/issues/33) (Cortex persistent memory)
**Date:** 2026-05-04
**Updated:** 2026-05-06

---

## TL;DR

Cellar should store memory and context durably, but it should not pass all of that stored history to an LLM.

The clean split is:

1. **Durable storage is broad.** Keep raw run history, selected memories, knowledge, adapter facts, and context snapshots where they are useful for replay, debugging, analytics, and future recall.
2. **LLM delivery is narrow.** Agents should receive a selected, budgeted `PlanningView`, not a 60K+ token dump of Cortex state.
3. **Planning stays with agents.** Cognition/context selection supports Codex, LangGraph, Mastra, n8n, Claude Code, GPT, Gemini, Cursor, or any other agent runtime. It does not become the platform's mandatory planner.

This plan keeps the useful part of the original cognition proposal: context and memory selection. It trims the ambition by making `PlanningView` the first deliverable and moving the cognition/enricher runtime to a planned later PR, after it has concrete memory and context enrichers to run.

---

## Relationship To The Repo Direction

The active architecture is:

1. `Adapters` - app- and domain-specific truth and capabilities.
2. `CEL / Cortex` - context fusion, memory/context management, adapter routing, and canonical execution.
3. `Agents` - pluggable planners and orchestrators.

This plan fits that split:

```text
Agents
  Plan goals, talk to the user, choose next actions, decide retries/checkpoints
        |
        | read selected context, execute actions, store/retrieve memory
        v
CEL / Cortex
  Fuse device state, store memory/context, build PlanningView, route actions
        |
        | call adapters through stable contracts
        v
Adapters
  Provide app-specific structured truth and execution capabilities
```

The durable value is not "Cortex owns a planner." The durable value is that any competent agent can ask Cortex for the right context and execute through stable CEL contracts.

---

## Core Principle: Store Broadly, Select Narrowly

The storage layer and the prompt layer are different products.

### Store Broadly

Cortex/CEL should be able to retain:

- Raw run transcripts for replay and debugging.
- Checkpoint summaries for compact session history.
- Explicit memories written by the user or agent.
- Outcome/failure/prior memories from important workflow checkpoints.
- Knowledge records and retrieved docs.
- Adapter facts such as spreadsheet metadata, browser DOM facts, app-specific object IDs, and capability reports.
- Context snapshots or references when they help explain why an action was taken.

This storage is allowed to grow because it is not automatically sent to the LLM.

### Select Narrowly

Before an LLM plans or acts, Cortex should project the broad stored state into a small `PlanningView`.

The `PlanningView` should:

- Fit a caller-provided token or element budget.
- Include evidence references back to stored records.
- Prefer current screen and adapter truth over stale memory.
- Include only memories and knowledge that are useful for the current goal.
- Preserve enough surrounding context to avoid over-filtering useful anchors.
- Explain why important memories or facts were selected.

The prompt gets the selected view. The store keeps the full history.

---

## Non-Goals

This plan intentionally does not make Cortex/CEL responsible for:

- Owning one mandatory planner.
- Owning one orchestration runtime.
- Replacing LangGraph, Mastra, Codex, Claude Code, n8n, or future agents.
- Deciding retry, branching, checkpoint, approval, or stop policies.
- Running a general-purpose sub-agent framework before `PlanningView` and memory-aware selection give it concrete work to coordinate.
- Sending all memories, transcripts, or raw context to an LLM by default.

Built-in planners and runners can still exist, but they should consume the same `PlanningView` as external agents.

---

## The Immediate Problem

Current planning paths can end up giving the LLM too much Cortex context. That creates three problems:

- **Token waste:** prompts can grow to 60K+ tokens even when the task needs a handful of elements and memories.
- **Bad decisions:** too much context can bury the relevant fact instead of helping the model.
- **Planner fragmentation:** different runtimes filter context differently, so fixes land in one planner but not the others.

The fix is not "store less." The fix is "centralize context selection."

---

## PlanningView

`PlanningView` is the shared, agent-facing projection of Cortex state.

It is not a database table and not a planner. It is a compact view built from current perception, adapter truth, memory, knowledge, and recent run history.

### Shape

```rust
pub struct PlanningView {
    pub goal: String,
    pub budget: PlanningBudget,

    pub screen: PlanningScreen,
    pub elements: Vec<PlanningElement>,
    pub adapter_facts: Vec<AdapterFactRef>,
    pub capabilities: Vec<CapabilityRef>,

    pub memories: Vec<MemoryRef>,
    pub knowledge: Vec<KnowledgeRef>,
    pub recent_events: Vec<EventRef>,

    pub blockers: Vec<Blocker>,
    pub anomalies: Vec<AnomalyRef>,
    pub evidence: Vec<EvidenceRef>,

    pub selection_rationale: Option<String>,
    pub omitted_counts: OmittedCounts,
}
```

### Required Properties

- **Budgeted:** callers can request limits such as `max_tokens`, `max_elements`, `max_memories`, and `max_adapter_facts`.
- **Grounded:** selected items carry references to the raw source record, element ID, adapter fact, transcript span, or memory ID.
- **Planner-neutral:** the same view can be consumed by the canonical Rust runner, LangGraph, Mastra, Codex, MCP clients, or n8n.
- **Safe to skip:** agents can still request raw context for debugging or specialized workflows.
- **Freshness-aware:** current screen and adapter facts outrank old memories unless the memory is clearly task-relevant.

### Location

`PlanningView` types should live in `cel-cortex`.

Do not create a `cel-planning-view` mini-crate for the first slice. Keeping the types in `cel-cortex` preserves the boundary: Cortex owns context fusion and selected context views; planners consume those views.

Serialization can be exposed through `cel-napi`, MCP, CLI, and SDK layers, but the canonical builder and data model should stay in `cel-cortex`.

### What The LLM Should See

The LLM prompt should receive a serialized `PlanningView`, not:

- the full `MentalModel`
- the full AX tree
- every memory
- every transcript event
- every adapter fact
- every historical observation

This is the main cleanup needed for the planning stack.

---

## MentalModel

`MentalModel` remains Cortex's live state model. It can include current perception, adapter truth, confidence/freshness, anomalies, and selected cognition/context fields.

The important boundary is:

- `MentalModel` can be rich.
- `PlanningView` must be small.

Suggested shape:

```rust
pub struct MentalModel {
    pub screen: ScreenState,
    pub adapters: HashMap<AdapterId, AdapterTruth>,
    pub recent_diffs: VecDeque<Diff>,
    pub anomalies: Vec<Anomaly>,
    pub confidence: f32,
    pub focus_trail: Vec<FocusEvent>,

    // Selected, derived support state. Empty until a view/enrichment runs.
    pub cognition: CognitionState,
}

pub struct CognitionState {
    pub last_planning_view: Option<PlanningViewSummary>,
    pub relevant_memories: Vec<MemoryRef>,
    pub relevant_knowledge: Vec<KnowledgeRef>,
    pub selection_rationale: Option<String>,
    pub last_updated_at: Option<Instant>,
    pub last_updated_by: Option<&'static str>,
}
```

Rules:

- Perception writes perception fields.
- Adapters write adapter truth through stable adapter contracts.
- Context selection writes only derived `cognition.*` fields.
- Agents read `PlanningView` and decide what to do.

Avoid language that makes Cortex "the planner" or "the agent mind." Cortex is the substrate that keeps agents grounded.

---

## Adapter Facts And Capabilities

`PlanningView` should include adapter facts and capabilities, but PR1 should not require every adapter to grow a new rich API.

### Minimal Read Surface

The deterministic selector needs a stable, small surface shaped like this:

```rust
pub struct AdapterFactRef {
    pub adapter_id: AdapterId,
    pub fact_id: String,
    pub kind: String,
    pub summary: String,
    pub freshness: Freshness,
    pub confidence: f32,
    pub source_ref: Option<String>,
}

pub struct CapabilityRef {
    pub adapter_id: AdapterId,
    pub capability: String,
    pub summary: String,
    pub input_schema_ref: Option<String>,
}
```

PR1a should use any active adapter truth/capability reporting that already exists. If Cortex does not currently expose that in a selector-friendly shape, PR1a should add only the minimal adapter context snapshot needed by `PlanningView`.

### Rules

- Current adapter facts should outrank stale memories when both are relevant.
- Adapter facts should be summaries plus refs, not full app-model dumps.
- App-specific structured truth still belongs in adapters.
- `PlanningView` should reference adapter facts; it should not copy every adapter payload into the LLM prompt.
- Adding the minimal read surface is part of PR1a if it does not already exist.

---

## Context Selection

The first cognition capability should be context selection, not a broad sub-agent framework.

### First Implementation: Deterministic Selector

Start with a cheap selector that can be shared across all planning paths:

1. Distill visible/current context against the goal.
2. Keep exact goal matches and semantically close labels/text.
3. Preserve ancestors, siblings, focused/selected/checked/expanded elements, and page-level text anchors.
4. Include active adapter facts and capabilities relevant to the current app.
5. Include recent failures, blockers, and anomalies.
6. Fill remaining budget with visible actionable elements.
7. Return omitted counts so agents know the view was compressed.

This matches the low-complexity fix already started in planner-specific code, but moves it into the shared CEL/Cortex boundary so every agent benefits.

### Later Implementation: Memory-Aware Selector

After the shared `PlanningView` contract is stable:

1. Vector pre-filter memories/knowledge/run summaries to a candidate catalog.
2. Optionally use a small LLM call to choose the most relevant IDs.
3. Hydrate only selected records.
4. Attach a concise rationale and source references.
5. Fall back to deterministic/vector selection on LLM timeout.

The LLM selector is an enhancement, not the foundation.

---

## Persistent Memory And Context Storage

Persistent storage is still important. It just should not define prompt size.

### Storage Types

| Store | Purpose | Prompt behavior |
|---|---|---|
| Raw transcripts | Replay, debugging, audit, offline analysis | Never injected wholesale |
| Checkpoint summaries | Compact session history | Selected when goal-relevant |
| `cortex_memories` | Durable user/workflow memory | Selected by PlanningView |
| Knowledge records | Retrieved docs/facts | Selected by PlanningView |
| Adapter facts | App-specific truth/capabilities | Current relevant facts selected first |
| Context snapshots/refs | Explain why an action happened | Referenced as evidence, not dumped |

SQLite is the right local operational store. DuckDB can still be useful later for offline analytics over JSONL transcripts and eval outputs, but it should not be required for the hot planning path.

### Memory Schema

```sql
CREATE TABLE cortex_memories (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    workflow_id      TEXT NOT NULL,
    kind             TEXT NOT NULL, -- 'outcome' | 'prior' | 'failure' | 'preference'
    content          TEXT NOT NULL, -- structured JSON
    summary          TEXT,
    tags             TEXT,          -- JSON array
    embedding        BLOB,
    source_ref       TEXT,          -- transcript span, checkpoint id, adapter fact id, etc.
    created_at       INTEGER NOT NULL,
    last_accessed_at INTEGER NOT NULL,
    decay_score      REAL
);

CREATE INDEX idx_memories_workflow ON cortex_memories(workflow_id);
CREATE INDEX idx_memories_decay ON cortex_memories(workflow_id, decay_score DESC);
```

### Content Shapes

```jsonc
{ "kind": "outcome", "action": "click", "target": "Save button", "result": "ok", "ts": "..." }
{ "kind": "prior", "statement": "Concur uses a 2-step submit: Save, then Submit for Approval." }
{ "kind": "failure", "what_failed": "click submit", "why": "modal opened first", "workaround": "dismiss modal then click" }
{ "kind": "preference", "statement": "User prefers confirmations before destructive app actions." }
```

### Write Policy

Use a lean write policy:

- Always keep raw transcripts/run history for replay.
- Auto-write memories only at meaningful checkpoints or final outcomes.
- Write failures when they change future behavior.
- Allow explicit memory writes through a tool such as `cel_think store_memory`.
- Do not auto-write every action as a memory.
- Store references to context snapshots when useful instead of copying the whole screen state into memory records.

This keeps storage useful without turning memory into an unfiltered prompt landfill.

### Decay

Use exponential half-life, default 90 days:

```text
score = e^(-ln(2) * age_days / 90)
```

Decay affects ranking and pruning. It should not be a hard rule that prevents an old but clearly relevant memory from being selected.

### Privacy / Scope

- Memories are local by default.
- `workflow_id` is required in v1; no global cross-workflow recall initially.
- Memory writes should be opt-in or product-configured explicitly.
- Raw transcripts and memory records should remain inspectable and deletable.

---

## MCP / SDK Surface

The external surface should make the selected view explicit.

### Raw Context

Raw reads remain available for debugging and specialized agents:

```jsonc
cel_perceive { "mode": "read" }
```

This should not invoke LLM selection by default.

### Planning View

Add an explicit planning/context view:

```jsonc
cel_perceive {
  "mode": "read",
  "view": "planning",
  "goal": "Submit this invoice",
  "budget": {
    "max_tokens": 8000,
    "max_elements": 80,
    "max_memories": 8,
    "max_adapter_facts": 12
  }
}
```

Possible response:

```jsonc
{
  "view": "planning",
  "goal": "Submit this invoice",
  "screen": { "summary": "...", "active_app": "Browser" },
  "elements": [...],
  "adapter_facts": [...],
  "capabilities": [...],
  "memories": [...],
  "knowledge": [...],
  "recent_events": [...],
  "blockers": [...],
  "evidence": [...],
  "selection_rationale": "Selected current form controls, submit-related prior failure, and active browser capability facts.",
  "omitted_counts": {
    "elements": 431,
    "memories": 27,
    "adapter_facts": 9
  }
}
```

### Memory Tools

Keep memory operations explicit:

```jsonc
cel_think { "mode": "store_memory", "memory": { ... } }
cel_think { "mode": "search_memory", "query": "...", "workflow_id": "..." }
cel_think { "mode": "prune_memory", "workflow_id": "...", "decay_below": 0.01 }
```

Once the planned PR4 `cel-cognition` crate exists, it can expose:

```jsonc
cel_think { "mode": "select_context", "goal": "...", "budget": { ... } }
```

But the first stable contract should be `PlanningView`, not "call arbitrary sub-agents."

---

## Crate Boundaries

### Dependency Boundary (introduced in PR1a.0, before PR1a)

`cel-cortex` historically depended on `cel-planner` for boundary types like `PlannedAction`, `Step`, `NextMove`, and `PlanProducer`. That made the platform layer (Cortex) depend on a planner implementation, which is the wrong direction once we want planners to be peers.

PR1a.0 extracts those types into a new `cel-contracts` crate. After PR1a.0:

```
cel-context     foundational perception types (ScreenContext, ContextElement)
   ↑
cel-contracts   boundary types (PlannedAction, NextMove, PlanProducer, ...
                 + later: PlanningView, PlanningBudget)
   ↑
cel-cortex      depends on context + contracts (NO longer on cel-planner)
cel-planner     depends on context + contracts (peer, not parent)
   ↑
cel-goal-runner wires the two together
```

`cel-contracts` depends only on `cel-context`, `serde`/`serde_json`, and `async-trait`. No cel-llm, no prompt logic, no planner internals. PR1a then adds `PlanningView` / `PlanningBudget` to `cel-contracts` without creating cycles.

### First Slice (PR1a)

| Area | Owns |
|---|---|
| `cel-contracts` | Boundary types: action contract, runner outcomes, `PlanProducer` trait, plus `PlanningView`/`PlanningBudget` from PR1a |
| `cel-cortex` | `MentalModel`, planning-view builder, current context selection |
| `cel-planner` | Consumes `PlanningView` (and produces `NextMove`); re-exports moved types from `cel-contracts` for backward compat |
| `cel-store` | `cortex_memories`, checkpoint summaries, query APIs, decay/pruning |
| `cel-napi` | Exposes planning view to JS/TS callers and MCP/CLI paths |
| `cel-goal-runner` | Consumes `PlanningView` instead of serializing full context |
| `agent/` LangGraph path | Consumes the same `PlanningView` instead of custom full-context compression |
| `mcp-server` | Exposes raw context and explicit planning view modes |

### Deferred Slice

| Area | Owns |
|---|---|
| `cel-cognition` | Planned selector/enricher runtime after `PlanningView` and memory-aware selection exist |
| `cel-llm` | Optional LLM-based selection and embedding provider abstraction |
| Adapters | Optional context enrichers/capability providers through stable CEL contracts |

Do not introduce `cel-cognition` just to create a framework. PR4 should introduce it with concrete enrichers and runtime responsibilities: lifecycle, budgets, fallback, caching, and telemetry for context/memory support. It still must not own planning, retries, branching, checkpoint policy, or stop policy.

---

## Planner Cleanup

Today, planning/context assembly is spread across several paths:

- Canonical Rust goal runner.
- LangGraph TypeScript planner.
- Direct N-API planner helpers such as `planStep` and `buildPlanPrompt`.
- MCP `cel_think` planning/debug paths.

That is why a token-budget fix can land in one place without helping all agents.

### Target State

Every planner path should do this:

```text
goal + budget
  -> CEL/Cortex PlanningView
  -> planner-specific prompt/action policy
  -> CEL canonical action execution
  -> result + updated context/memory
```

### Rules

- Planner-specific code may format prompts differently.
- Planner-specific code may choose different models.
- Planner-specific code may implement different retry or checkpoint policies.
- Planner-specific code must not own the canonical context selection algorithm.
- Built-in planners should be examples/clients of `PlanningView`, not privileged owners of context logic.

### Migration Order

1. PR1a: move the current compact context filtering into a shared `PlanningView` builder in `cel-cortex`.
2. PR1a: wire the canonical Rust goal runner to that builder to prove the contract.
3. PR1b: wire direct N-API planner helpers to that builder.
4. PR1b: wire LangGraph context assembly to that builder.
5. PR1b: expose the same view through MCP/SDK.
6. PR2: add durable memory storage.
7. PR3: add memory-aware selection into `PlanningView`.
8. PR4: add `cel-cognition` as the shared runtime for concrete enrichers.

---

## Evals

Evals should stay agent-agnostic where possible.

Add or update scenarios that assert:

- Planning prompts stay under a configured token budget.
- Goal-relevant elements survive compression.
- Focused/selected/stateful anchors survive compression.
- Current adapter facts outrank stale memory.
- A relevant memory is selected when it changes the correct next action.
- Irrelevant memories are omitted even when they are semantically nearby.
- The same scenario can run against canonical and LangGraph backends using the same CEL planning view.

Runtime-specific evals are allowed, but they should be secondary.

### Cross-Backend Eval Clarification

PR1a should not block on full cross-backend eval coverage. It should prove the contract with unit tests and the canonical Rust runner.

PR1b should add the cross-backend assertion that canonical and LangGraph can consume the same CEL planning view. If the existing eval harness cannot express that cleanly, add a small eval-enabler before or inside PR1b. Do not let eval harness plumbing bloat PR1a.

---

## Trimmed Implementation Plan

### PR 1a.0: Extract `cel-contracts` (mechanical, no behaviour change)

Goal: fix the `cel-cortex` → `cel-planner` dep direction so PR1a can land `PlanningView` without circular dependencies.

Ships:

- New `cel-contracts` crate. Depends only on `cel-context`, `serde`/`serde_json`, and `async-trait`. No `cel-llm`, no prompt types, no planner internals.
- Move boundary types out of `cel-planner`: `PlannedAction`, `CellWrite`, `Step`, `StepKind`, `StepResult`, `RuntimeCaps`, `RunLimits`, `NextMove`, `AttemptRecord`, `GoalOutcome`, `FailureReport`, `PlanProducer` trait, `DoneVerdict`.
- `cel-planner` retains `Plan`/`SubGoal` (legacy) and re-exports the moved types via `pub use cel_contracts::{...}` for backward compatibility — one release cycle, then prune.
- `cel-cortex` drops its `cel-planner` dependency entirely, depends on `cel-contracts` instead.
- `cel-goal-runner` and `cel-napi` add `cel-contracts` and update imports.
- Workspace builds clean; all existing tests pass.

Does not ship:

- `PlanningView` types or builder.
- Any behaviour change.
- Any new tests beyond the moved ones.

### PR 1a: PlanningView Contract + Canonical Runner

Goal: prove the selected-context contract with the smallest useful vertical slice.

Ships:

- `PlanningView` data model in `cel-cortex`.
- Deterministic current-context selector in `cel-cortex`.
- Evidence refs and omitted counts.
- Minimal adapter fact/capability read surface if the existing Cortex adapter truth is not selector-friendly.
- Shared serialization for LLM prompts.
- Canonical Rust runner uses `PlanningView`.
- `cel-planner` accepts `PlanningView` as planning input (replaces direct MentalModel serialization). Without this the prompt-shrinking does not actually happen.
- Unit tests for selection and budget behavior.
- Canonical-runner smoke/eval coverage for compact context.

Does not ship:

- Direct N-API helper migration.
- LangGraph migration.
- MCP/SDK planning view mode.
- Persistent memory store.
- General sub-agent runtime.
- LLM-based memory selector.
- Adapter-registered sub-agents.
- Telemetry panel for cognition.

### PR 1b: PlanningView For All Planner Surfaces

Goal: remove planner fragmentation by making all built-in planner paths consume the same selected context view.

Ships:

- Direct N-API planner helpers use `PlanningView`.
- LangGraph path uses `PlanningView`.
- MCP/SDK can request `view: "planning"`.
- Prompt serialization stays shared where possible, while planner-specific prompt wording remains allowed.
- Cross-backend eval verifies canonical and LangGraph can run the same scenario against the same CEL planning view.
- Small eval harness enabler if the existing runner cannot express the cross-backend assertion.

Does not ship:

- New memory schema.
- LLM-based memory selection.
- General cognition runtime.

### PR 2: Persistent Memory Store

Goal: store durable, high-signal workflow memory without bloating prompts.

Ships:

- `cortex_memories` migration.
- CRUD/query APIs.
- Explicit `store_memory`, `search_memory`, and pruning tools.
- Checkpoint/final-outcome memory write path.
- Decay scoring.
- Tests for write/read/prune.
- Docs for memory lifecycle and deletion.

### PR 3: Memory-Aware PlanningView

Goal: let `PlanningView` select from durable memory.

Ships:

- Candidate retrieval from memories/knowledge/checkpoint summaries.
- Budgeted memory hydration.
- Selection rationale.
- Fallback behavior.
- Tests for relevant/irrelevant memory selection.

### PR 4: Cognition/Enricher Runtime

Goal: make cognition a real support layer for context and memory enrichment, not a nice-to-have and not a planner.

Ships:

- `cel-cognition` crate.
- Enricher trait/contract with typed input/output.
- Runtime support for lifecycle, budgets, fallback, caching, and telemetry.
- At least two concrete enrichers so the crate is justified.
- Likely first enrichers: `memory_tagger`, `summarizer`, or `stale_detector`.
- Integration with `PlanningView` as an enrichment source, not a planning owner.

Does not ship:

- A mandatory planner.
- Retry/branching/checkpoint policy.
- `goal_decomposer` as core Cortex behavior.
- Hidden adapter-owned planners.

### PR 5: Migrate `langgraph/tools.ts` to PlanningView

Goal: finish the planner-fragmentation cleanup by routing the LangGraph
`see` tool through the same cortex `PlanningView` the canonical Rust
runner uses. Deferred from PR1c after a first cut hit a regression in
`react-agent.test.ts` (the test's mocked `buildPlanningView` returned
an empty view, which broke `act()`'s indexed-target resolution and
caused the agent loop to time out).

Ships:

- `langgraph/tools.ts`: `see` tool calls `driver.buildPlanningView` and
  renders the result (replacing `compressContext` +
  `serializeContextForLLM` on this path).
- `renderPlanningView()` helper that produces `{ text, indexMap,
  elementCount }` from a `PlanningView`, preserving the LLM-facing
  numeric-index pattern act() relies on.
- Test fixtures (`react-agent.test.ts`, `graph.test.ts`) updated so
  their mocked `buildPlanningView` mirrors the perception's elements
  into the view — this is what the original PR1c attempt missed.
- `compressContext` / `serializeContextForLLM` may be marked
  deprecated in `agent/src/index.ts` once tools.ts no longer uses them.
  External callers retain access for one release cycle.

Does not ship:

- Any new MCP modes.
- Any change to the canonical Rust runner or its tests.

Why deferred: PR1b's main planner path (`CelLlmPlanner`) already
converges on the canonical Rust planner via N-API (PR1c convergence
test proves it). Migrating the LangGraph **tool** surface is a
correctness improvement, not a blocker — until the regression is
fully understood, leaving `tools.ts` on the legacy compression path is
safer than racing a half-fixed migration.

Acceptance:

- `react-agent.test.ts` passes alongside the migration (i.e. fix the
  test or the interaction the test caught).
- `pnpm --filter @cellar/agent test` green end-to-end (no excluded
  files).
- Cross-backend convergence test still passes.

---

## Deferred Enrichers

These are useful, but they should not expand the first slice.

### `memory_tagger`

Runs at memory write time. Produces 3-7 tags to improve future selection.

### `memory_fuser`

Runs periodically or on demand. Collapses near-duplicate memories so catalogs stay small.

### `stale_detector`

Runs before action execution. Produces a signal like `{ stale, reason }`; the agent decides whether to re-read, retry, or continue.

### `action_verifier`

Runs after action execution. Produces evidence about whether the action landed and whether side effects appeared. It should not own retry policy.

### `summarizer`

Runs at checkpoint. Produces compact summaries for future memory selection.

### Adapter Context Enrichers

Adapters can expose structured facts or optional enrichers, but they should do so through CEL capability/context contracts. Avoid letting adapters register arbitrary hidden planners.

### `goal_decomposer`

Goal decomposition belongs primarily to agents. It can exist as an optional helper/reference tool, but it should not be part of the core Cortex cognition path.

---

## Open Decisions

| Question | Current recommendation |
|---|---|
| Should `enable_cognition` default to true? | No for the first slice. Make planning view explicit/on-demand. Built-in runners can request it by default. |
| Should memory be enabled by default? | Keep safe v1 default explicit/product-configured. Storage exists, but writes should be intentional. |
| Should selection use an LLM immediately? | No. Start deterministic, then add optional LLM selection after the shared view is stable. |
| Should `cel-cognition` be created immediately? | No. Start with `PlanningView`; create `cel-cognition` in PR4 with concrete enrichers and a strict non-planning boundary. |
| Should adapters register sub-agents? | Not initially. Let adapters expose facts/capabilities first; PR4 can add scoped adapter context enrichers if needed. |
| Where should analytics live? | Raw transcripts/eval JSONL first; DuckDB can be a later offline analytics layer, not hot-path planning storage. |
| Who owns `PlanningBudget` defaults? | `cel-cortex` provides sensible defaults (token/element/memory/adapter-fact ceilings). Each planner overrides per-call when it knows better — e.g. host with a 128K context can request a larger budget. Defaults must keep typical prompts under common LLM context windows. |

---

## Acceptance Criteria

For the trimmed plan to be considered successful:

- [ ] Agents can request raw context or selected planning context explicitly.
- [ ] PR1a proves `PlanningView` through `cel-cortex` and the canonical Rust runner.
- [ ] PR1b migrates direct N-API, LangGraph, and MCP/SDK surfaces to the same `PlanningView` builder.
- [ ] Typical planning prompts stay under the configured budget.
- [ ] Relevant elements, stateful anchors, adapter facts, blockers, and evidence refs survive selection.
- [ ] Persistent memories can be stored without being automatically injected into prompts.
- [ ] Memory selection is budgeted and rationale-backed before it reaches an LLM prompt.
- [ ] Evals verify the context contract across more than one agent backend.
- [ ] PR4 introduces `cel-cognition` as a context/memory enrichment runtime with concrete enrichers, not as a planner.
- [ ] Docs clearly state that agents own planning and Cortex owns context/memory/execution support.

---

## What Changed From The Original Cognition Proposal

| Original direction | Trimmed direction |
|---|---|
| New `cel-cognition` crate first | Shared `PlanningView` first; `cel-cognition` planned for PR4 |
| Broad sub-agent framework | Context/memory enrichment runtime with concrete enrichers and no planning ownership |
| Cortex framed as "the mind" | Cortex framed as grounding substrate for pluggable agents |
| `goal_decomposer` in rollout | Deferred to agent/reference helper territory |
| LLM selector in first PR | Deterministic selector first; LLM selector optional later |
| Persistent memory plus framework plus MCP plus telemetry in one PR | Split into PR1a/PR1b PlanningView, memory store, memory-aware selection, cognition runtime |
| `cel_perceive read` enriched by default | Raw read remains raw; planning view is explicit |

The core insight remains: persistent memory matters, but selection matters more at prompt time. The cleanup is making that selection a shared CEL/Cortex service instead of planner-specific prompt glue.
