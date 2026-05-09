# Cognition Layer + Persistent Memory - Trimmed Design Plan

**Status:** Proposal, trimmed to match the current Adapters / CEL-Cortex / Agents architecture
**Author:** dimpagk92 + Claude, revised with Codex
**Closes / reframes:** [dimpagk92/cellar#33](https://github.com/dimpagk92/cellar/issues/33) (Cortex persistent memory)
**Date:** 2026-05-04
**Updated:** 2026-05-07 — post-PR3 weakness backlog cleared (WK1+WK3+WK4+WK5 shipped); PR4 reframed from "cel-cognition crate" to a tiered, eval-driven approach.

---

## Status (2026-05-07)

**Status conventions:**
- ✅ **merged** — landed on main, OSS-synced, in production
- 📝 **PR'd** — code-complete, all local gates green (fmt + clippy + tests), PR open and awaiting CI/merge (currently blocked on GitHub Actions billing — see end of section)
- 🔄 **reframed** — design changed; see referenced section
- ⏳ **next** — actively in flight or up next

| Phase | What | Status |
|---|---|---|
| PR1a.0 | Extract `cel-contracts` (mechanical) | ✅ merged |
| PR1a / PR1b | `PlanningView` contract + canonical runner + N-API/MCP/SDK surfaces | ✅ merged |
| PR1c | Cross-backend convergence test | ✅ merged |
| PR2.0 | `cortex_memories` storage layer | ✅ merged |
| PR2 | Memory consumers (N-API + MCP + auto-write) | ✅ merged |
| PR3 | Memory-aware `PlanningView` (deterministic selector) | ✅ merged |
| **WK4** | **`CortexMemoryStore` trait + open-once-per-run** | 📝 PR'd (#30) — gates green, +3 tests |
| **WK1** | **FTS5 keyword recall (schema v3) + planning_view pre-filter** | 📝 PR'd (#31) — gates green, +8 tests |
| **WK5** | **Stabilize 8 pre-existing flaky tests** | 📝 PR'd (#32) — gates green, both flakes verified fixed locally |
| **WK3 / PR5** | **Migrate `langgraph/tools.ts` see-tool to `PlanningView`** | 📝 PR'd (#33) — gates green, +4 tests, backward-compat preserved (legacy fallback) |
| **Plan reframe** | Tiered PR4 + status snapshot doc | 📝 PR'd (#34) |
| **Tier A1** | Populate `PlanningView.knowledge` from `knowledge_fts` | 📝 PR'd (#35) — gates green, +8 tests, separate `KnowledgeStore` trait |
| **WK2** | Vector embedding infrastructure (Embedder trait + storage search + selector plumbing) | 📝 PR'd (#36) — gates green, +14 tests; un-deferred 2026-05-07 as infrastructure-only (no bundled embedder); subsumes B2 |
| **Tier A2** | Populate `PlanningView.recent_events` from cortex `observations` (priority + recency ordered, workflow-scoped) | 📝 PR'd — gates green, +7 tests, separate `RecentEventStore` trait |
| **Tier A3** | Populate `PlanningView.blockers` + `anomalies` from cortex `MentalModel.anomaly_queue` + `freshness` (Dialog/AuthPrompt → blocker; HardStale → blocker; SoftStale → anomaly) | 📝 PR'd — gates green, +9 tests, new `StepExecutor::cortex_anomalies()` + `cortex_freshness()` methods |
| **Recall eval** | Memory-recall eval in `cel-eval` (`recall-eval` feature) — 10 scenarios × 2 modes, baseline numbers established | ✅ merged — scenarios + IR metrics (P@k / R@k / MRR), Path B added 5 harder cases |
| **Tier A4 (infrastructure)** | `MemoryEnricher` trait + write-time hook + always-safe fallback to plain summary | ✅ merged (#41) — +4 runner tests + 3 trait tests; mirrors WK2 pattern (seam shipped, no bundled LLM impl) |
| **Tier B1 (infrastructure)** | `MemorySelector` trait + read-time re-rank + always-safe fallback to WK1 deterministic ordering | 📝 PR'd — gates green, +4 runner tests + 3 trait tests; mirrors A4 + WK2 pattern (seam shipped, no bundled LLM impl) |
| Tier B3 | Cross-workflow priors | deferred — needs privacy decision before code |
| ⏳ **Next** | Production dogfooding / harder eval scenarios / platform breadth (adapters, benchmark server) | next — strategic call |
| ~~Tier B2~~ | ~~Vector embeddings~~ | retired — subsumed by WK2 above |
| PR4 (umbrella) | Cognition/enricher work | 🔄 reframed into Tier A/B/C — see PR4 section below |

The four post-PR3 weaknesses (WK1/WK3/WK4/WK5) cleared the items called out in the PR3 honest-assessment review. `CortexMemoryStore` (WK4) is the substitution seam any future cognition runtime would have introduced; FTS5+decay (WK1) is the deterministic recall the runtime would have layered on top of; PlanningView via see-tool (WK3) is the contract every future enricher would write into. WK2 was originally deferred but later shipped as **infrastructure-only** vector embedding plumbing (trait + storage search + selector hook + null default), which subsumes the original Tier B2 entry. The remaining cognition work is now mostly "fill in the empty PlanningView fields" plus optional eval-gated upgrades — a much narrower surface than the original "cel-cognition crate" framing.

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

### PR 4: Cognition / Enricher Work — Reframed (2026-05-07)

**Status: scope re-tiered after WK1/WK3/WK4/WK5 shipped.** The original PR4
framing — "build the `cel-cognition` crate, add an enricher trait + runtime,
ship two concrete enrichers" — was right in spirit but too monolithic.
Three of the architectural pieces it would have introduced are now done:
the substitution seam (`CortexMemoryStore` trait, WK4), the deterministic
recall layer (FTS5 + decay, WK1), and the see-tool contract (PlanningView
via WK3 / PR5). What's left splits cleanly along an eval-gated boundary.

**Architectural anchor: cognition lives in the Cortex column, not the
agent column.** `cel-cognition` (if/when it exists) is part of the
in-house Cortex layer that *every* main agent (LangGraph, Mastra, Codex,
Claude Code, GPT, Gemini, n8n) consumes via `PlanningView`. It is **not
a planner.** It does not own goal interpretation, decide_next, or user
interaction. Those stay with the pluggable agent runtime per CLAUDE.md.

```text
Main Agent (pluggable: LangGraph, Mastra, Codex, ...)
  Owns: interaction, goal, planning, reflection
        |
        | consumes PlanningView; calls cel-cortex via N-API / MCP
        v
Cortex (in-house, single canonical impl)
  Owns: perception, context fusion, memory, PlanningView builder
  This column is where cognition / enricher work lives.
        |
        v
Adapters (pluggable per app)
```

Ship in tiers. Tier A first; Tiers B and C only after measurement
justifies them.

#### Tier A — Cortex-side gaps with clear value (build now)

These items fill PlanningView fields that exist in the contract but are
currently always empty. They do NOT require a new crate; each lands as
a small focused PR in `cel-cortex` (same shape as the WK PRs).

- **A1 — Populate `PlanningView.knowledge`** from the existing
  `knowledge_fts` FTS5 store (`cel-store::memory`). Same goal-keyword
  query the WK1 selector uses. Currently always `[]` — wiring is the
  whole change. Small PR, lives in cel-cortex/planning_view.

- **A2 — Populate `PlanningView.recent_events`** from cortex
  observations + the existing anomaly queue. Currently `[]`.

- **A3 — Populate `PlanningView.blockers` + `anomalies`** from the
  anomaly detector + freshness assessment already running in cel-cortex.
  Currently `[]`.

- **A4 — Memory enrichment background pass.** Auto-generate tags,
  upgrade summaries, link evidence across memories at write time (not
  per turn). The `tags` column on `cortex_memories` is empty today.
  Mid-size PR; needs a wired `cel-llm` provider; gated by the recall
  eval (does enrichment actually improve hydration quality?).

#### Tier B — Eval-gated upgrades (don't build until measurement justifies)

Build a memory-recall eval first (lives in `cel-eval`): seed a workflow
with N memories spanning relevant + irrelevant + adjacent topics, run
`select_memories` against a fixed set of goals, score the hydrated
selection against a hand-labeled gold set. Then decide:

- **B1 — LLM-based memory selector.** Re-rank WK1's FTS5+decay
  candidates via an LLM call. Cost: 200–1000 ms + tokens per turn.
  Justified only if the deterministic top-N misses memories the eval
  marks as relevant. Slots in via a new `MemorySelector` trait;
  deterministic path stays as the default and the fallback.

- ~~**B2 — Vector embeddings (semantic recall).**~~ **Retired 2026-05-07.**
  Subsumed by the un-deferred WK2 (which ships the *infrastructure* —
  trait + storage path + selector plumbing — without committing to a
  specific embedder). What remains here is the choice of which
  embedder to wire by default (cel-llm provider call per insert vs
  local ONNX/candle model ~50 MB binary). That choice is now a
  product/deployment decision rather than a build decision, and stays
  eval-gated: bundle a default embedder only if the recall eval shows
  semantic recall is doing real work over FTS5.

- **B3 — Cross-workflow priors.** Let the selector look at memories
  from "similar" workflows (similarity defined by some signal — app
  overlap, goal-text overlap, etc.). Privacy decision required first
  — current strict workflow-scoping is a deliberate default.

#### Tier C — Already covered or won't move the needle (do not build)

- **Selector trait abstraction** — done as `CortexMemoryStore` in WK4.
- **PlanningView contract** — done in PR1.
- **Open-once handle** — done in WK4.
- **Caching layer in front of SQLite** — rusqlite already caches
  prepared statements; reads are µs-scale post-WK4. Complexity beats
  benefit.
- **A new `cel-cognition` crate as the entry point.** Premature.
  Tier A lives fine in `cel-cortex`. If Tier B (especially B1+B2) lands
  AND Tier A4 enrichment grows non-trivially, *then* spinning out
  `cel-cognition` makes sense as a home for the cross-cutting stuff.
  Until then, a dedicated crate is overhead without payoff.

#### Does not ship under PR4 (any tier)

- A mandatory planner.
- Retry / branching / checkpoint policy.
- `goal_decomposer` as core Cortex behavior.
- Hidden adapter-owned planners.
- Anything that promotes one main-agent runtime above the others.

#### Sequencing recommendation

1. A1 → A2 → A3 (small, additive, fill empty fields)
2. Memory-recall eval in `cel-eval`
3. A4 (enrichment) once eval baseline exists
4. Re-evaluate B1 / B2 / B3 against eval data
5. Only then decide whether `cel-cognition` as a crate is worth its weight

### PR 5: Migrate `langgraph/tools.ts` to PlanningView — shipped as WK3 (#33)

**Status: shipped 2026-05-07** (queued for merge — see Status table at top
of file for full list of pending PRs).

Routed the LangGraph `see` tool through `driver.buildPlanningView`. The
deferral risk that originally tabled this work — the `react-agent.test.ts`
regression noted in PR1c — turned out to be a separate environment-
dependent issue (vitest + Claude CLI subprocess race), not a contract
mismatch. WK5 fixed that root cause; WK3 then landed cleanly with a
fall-back-on-failure design rather than a hard dependency on
`buildPlanningView`.

Shipped:

- `langgraph/tools.ts`: `see` tool prefers `driver.buildPlanningView`
  when both `options.goal` and `options.driver.buildPlanningView` are
  provided. Falls back to the legacy `compressContext` +
  `serializeContextForLLM` path otherwise (no breaking change for
  callers that haven't migrated their drivers).
- `renderPlanningView()` helper produces `{ text, indexMap,
  elementCount }` from a `PlanningView`, preserving the numeric-index
  pattern `act()` relies on.
- New PlanningView-only fields surfaced to the planner:
  `selection_rationale`, `omitted_counts`, and a compact `memories`
  list (id / kind / summary).
- Builder failures (`buildPlanningView` throws) `console.warn` and fall
  back to the legacy path — never black-hole the see() call.
- 4 new tests in `agent/src/langgraph/tools.test.ts` covering both the
  new path and all three fallback conditions.

Did not ship (intentional):

- Hard removal of the legacy compress-context path. Kept as the
  fallback for drivers that don't implement `buildPlanningView`.
- `react-agent.test.ts` rewrite. The existing test continues to exercise
  the legacy path (its driver mock has no `buildPlanningView`); the new
  path is covered by `tools.test.ts` instead. Backward-compat proof.

Acceptance (met):

- `pnpm test`: 330 passed (was 326 — +4 new tests).
- `pnpm lint` (tsc --noEmit) clean.
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
| Should selection use an LLM immediately? | **Build the seam now; the LLM-backed impl is opt-in.** B1 ships as a `MemorySelector` trait with WK1 as the always-safe fallback (mirroring WK2's embedder seam). Wiring the trait is unconditional; *bundling* a particular LLM impl is what the eval gates — not the seam itself. (Earlier "no" framing was over-conservative; reframed 2026-05-09.) |
| Should `cel-cognition` be created immediately? | **No (confirmed by post-WK reframe).** Tier A work (fill empty PlanningView fields, memory enrichment) lives fine in `cel-cortex`. A new crate becomes worth its weight only if Tier B (LLM selector + embeddings) and richer enrichment land — and only after a recall eval shows they're needed. Until then, no crate. |
| Should adapters register sub-agents? | Not initially. Let adapters expose facts/capabilities first; revisit if Tier A4 (memory enrichment) shows an adapter-specific gap. |
| Where should analytics live? | Raw transcripts/eval JSONL first; DuckDB can be a later offline analytics layer, not hot-path planning storage. |
| Who owns `PlanningBudget` defaults? | `cel-cortex` provides sensible defaults (token/element/memory/adapter-fact ceilings). Each planner overrides per-call when it knows better — e.g. host with a 128K context can request a larger budget. Defaults must keep typical prompts under common LLM context windows. |
| What gates Tier B (LLM selector / embeddings / cross-workflow)? | A memory-recall eval in `cel-eval`: seed a workflow with relevant + irrelevant + adjacent memories, score `select_memories` against a hand-labeled gold set. If the deterministic baseline scores well, Tier B is dead. If not, the eval shows WHICH part to invest in. |

---

## Acceptance Criteria

For the trimmed plan to be considered successful:

- [x] Agents can request raw context or selected planning context explicitly. *(PR1a)*
- [x] PR1a proves `PlanningView` through `cel-cortex` and the canonical Rust runner.
- [x] PR1b migrates direct N-API, LangGraph, and MCP/SDK surfaces to the same `PlanningView` builder.
- [x] Typical planning prompts stay under the configured budget. *(PR1a — `PlanningBudget` enforced)*
- [x] Relevant elements, stateful anchors, adapter facts, blockers, and evidence refs survive selection. *(elements + selection_rationale + omitted_counts shipped; **knowledge / recent_events / blockers / anomalies still always-empty — Tier A1/A2/A3 closes these**)*
- [x] Persistent memories can be stored without being automatically injected into prompts. *(PR2 — opt-in via `RunLimits.workflow_id_for_memory` + `RunLimits.memory_db_path`)*
- [x] Memory selection is budgeted and rationale-backed before it reaches an LLM prompt. *(PR3 + WK1)*
- [x] Evals verify the context contract across more than one agent backend. *(PR1c convergence test)*
- [ ] ~~PR4 introduces `cel-cognition` as a context/memory enrichment runtime with concrete enrichers, not as a planner.~~ **REFRAMED:** Tier A work (A1–A4) lives in `cel-cortex` directly; cel-cognition crate deferred until eval data justifies it. See PR4 section above.
- [x] Docs clearly state that agents own planning and Cortex owns context/memory/execution support. *(this doc + CLAUDE.md)*

New post-WK acceptance criteria:

- [x] **Tier A1**: `PlanningView.knowledge` populated from `knowledge_fts` for goals with extractable keywords. *(PR'd #35; gates green)*
- [x] **WK2 (subsumed B2)**: vector embedding infrastructure shipped without bundled embedder — `Embedder` trait, byte-ser, cosine helper, cortex selector cosine boost wired through `PlanningViewInputs.goal_embedding`. *(PR'd #36; gates green; the 5 WK2 selector tests include a sanity-verified strengthened test that fails when the cosine path is removed)*
- [x] **Tier A2**: `PlanningView.recent_events` populated from cortex `observations` table, priority + recency ordered, workflow-scoped. Anomaly-queue surfacing moved to A3 (the anomaly queue is in cortex's MentalModel, not the store; A3 will surface it into both `anomalies` and `blockers` via the same selector pass). *(PR'd; gates green; 7 new tests; same `Mutex<CelStore>` handle satisfies the new `RecentEventStore` trait alongside `CortexMemoryStore` + `KnowledgeStore`)*
- [x] **Tier A3**: `PlanningView.blockers` and `PlanningView.anomalies` populated from cortex `MentalModel.anomaly_queue` + `freshness` assessment. Mapping: every anomaly → `AnomalyRef`; `Dialog` / `AuthPrompt` ALSO → `Blocker`; `HardStale` → `Blocker`; `SoftStale` → `AnomalyRef`; `Fresh` → nothing. NOT budgeted (per the `OmittedCounts` docstring "first-class blocker that should not be lost to compression"). *(PR'd; gates green; 9 new tests covering each kind + combined; new `StepExecutor::cortex_anomalies()` + `cortex_freshness()` methods with empty defaults for test executors)*
- [x] **Memory-recall eval** lives in `cel-eval` (`recall-eval` feature) and produces baseline scores for the WK1 deterministic selector and WK1+WK2-stub. *(PR'd; gates green; 5 hand-built scenarios spanning easy keyword match / distractor density / semantic alignment / kind bias / decay floor; `print_full_report` test pretty-prints the numbers via `--nocapture`)*

### Baseline numbers (post-Path-B, 10 scenarios × 2 modes × 3 k values, captured 2026-05-07)

The first eval (5 scenarios) was too easy — all hand-built memories shared keyword vocabulary with their goals, so WK1 aced everything and the WK2 stub had nothing to differentiate. **Path B** added 5 harder scenarios designed to actually break WK1 if the weakness exists; the data below is the result.

#### k=1 (the strictest test — does the relevant memory rank #1?)

| Scenario | Mode | P@1 | R@1 | MRR |
|---|---|---|---|---|
| easy_keyword_match | wk1_only / wk1+wk2_stub | 1.00 | 1.00 | 1.00 |
| distractor_density | wk1_only / wk1+wk2_stub | 1.00 | 1.00 | 1.00 |
| semantic_alignment | wk1_only / wk1+wk2_stub | 1.00 | 1.00 | 1.00 |
| kind_bias | wk1_only / wk1+wk2_stub | 1.00 | 0.50 | 1.00 |
| decay_floor | wk1_only / wk1+wk2_stub | 1.00 | 1.00 | 1.00 |
| **pure_semantic_gap** | wk1_only / wk1+wk2_stub | **0.00** | **0.00** | **0.00** |
| heavy_distractor_density | wk1_only / wk1+wk2_stub | 1.00 | 1.00 | 1.00 |
| quoted_phrase_precision | wk1_only / wk1+wk2_stub | 1.00 | 1.00 | 1.00 |
| long_tail_recall | wk1_only / wk1+wk2_stub | 1.00 | 1.00 | 1.00 |

(P@k drops at k=5 on the multi-candidate scenarios, but only because the budget admits all candidates including distractors. MRR is the ranking-quality signal; P@k is the precision-at-budget signal.)

**What this tells us:**

1. **WK1 is robust on every keyword-tractable scenario** — 8 / 9 scenarios at MRR=1.0 across both modes. This includes the genuinely hard ones: bm25 correctly picks the right "submit X report" out of 5 dense distractors, the `+30` quoted-phrase boost works, the kind-bias multiplier puts Failure ahead of Outcome, and the long-tail finds 1 needle in 31 memories.

2. **`pure_semantic_gap` is a confirmed WK1 failure**: 0 / 0 / 0 across all k. The relevant memory says "Uploaded the documents" with goal "file the receipts" — zero keyword overlap means zero recall. This is a real, reproducible weakness.

3. **The stub embedder doesn't fix the semantic gap.** Same 0 / 0 / 0 in `wk1+wk2_stub` mode. The hash-bucketed stub has no semantic awareness; only a real embedder (cel-llm provider call or local ONNX) could catch this.

4. **WK2 cosine boost adds zero measurable signal on every scenario.** Identical numbers in both modes everywhere. Confirms WK1 is doing the work; the stub embedder is genuinely too weak to differentiate.

**Implication — sharpened by Path B data:**

- **A4 (memory enrichment)**: still NOT justified. Richer summaries / tags don't help when the underlying issue is *vocabulary mismatch between memory and goal*, not summary quality.
- **B1 (LLM-based selector)**: NOT justified by these scenarios. WK1 already ranks correctly when there's any keyword signal at all; an LLM selector would just re-rank what WK1 already gets right.
- **B3 (cross-workflow priors)**: NOT justified yet. The pure_semantic_gap scenario is most realistic *across* workflows (where vocabulary varies more) — but we have no production data showing how often that pattern shows up.
- **Bundle a default embedder**: **CONDITIONALLY justified.** If production data shows pure-semantic-gap-like cases happen with non-trivial frequency, a real embedder (cel-llm or local ONNX) shipping bundled would close the only confirmed gap. Without that production data, it's still speculative — but we now know exactly *what* a real embedder would buy us. The decision becomes "is fixing the semantic-gap case worth the binary / per-call cost?", which is a much sharper question than before.

**Caveat — production realism:**

In current cellar usage, memories are written at run-end by `canonical_runner` using a summary derived from the *goal text* itself ("Completed: submit invoice in Concur"). That construction guarantees keyword overlap on future runs of the same goal in the same workflow — so within-workflow recall will look much more like `easy_keyword_match` than `pure_semantic_gap`. The gap matters most for **cross-workflow** retrieval (memories from workflow A surfacing for goal in workflow B), which is exactly what B3 covers and what we're not building yet.

**Net:** the cognition layer is in good shape. WK1+WK2-seam is sufficient for everything we can currently measure. The eval is now a *useful production tool* — when we ship and accumulate real memories, we can grade them against this scenario suite and see whether pure-semantic-gap patterns show up enough to justify B2/B3 / A4.
- [x] **Tier A4 (infrastructure)**: Memory enrichment seam — `MemoryEnricher` trait in `cel-llm` + `MemoryEnrichmentInput`/`Output` shapes + `with_memory_enricher` builder on `CanonicalGoalRunner` + write-time hook in `write_outcome_memory_if_enabled` with always-safe fallback (plain summary + `["canonical_runner"]` tag set on enricher absence/failure/empty-output). Mirrors WK2 pattern: ship the seam, no bundled LLM impl. *(Shipped this PR; 4 new tests pin the contract: no enricher → plain; success → enriched + merged tags; failure → plain fallback; empty-output → plain fallback. 3 stub-trait tests in cel-llm.)*
- [x] **Tier B1 (infrastructure)**: LLM-based memory selector — `MemorySelector` trait in `cel-llm` + `MemoryRerankContext`/`MemoryRerankItem` shapes + `with_memory_selector` builder on `CanonicalGoalRunner` + read-time re-rank in `run_inner` after `build_planning_view`, with always-safe fallback (selector failure / unknown ids → WK1 ordering preserved). Mirrors A4 + WK2 pattern: ship the seam, no bundled LLM impl. *(Shipped this PR; 4 new runner tests pin the contract: no selector → WK1 order; success → re-ordered; failure → WK1 fallback; invents-ids → defensive drop. 3 stub-trait tests in cel-llm.)*
- [ ] **Tier B3**: Cross-workflow priors — needs an explicit privacy decision before code. Stays deferred until that product call lands.

**Reframe note (2026-05-09):** Earlier wording marked A4 + B1 as "eval-gated → don't build until eval forces it." That was over-conservative — the original architectural intent (visible in the PR4 reframe section above) is "build the seam, keep the deterministic path as fallback, eval validates the LLM impl's *quality*." This block of acceptance criteria now matches that intent. Only B3 stays as a true deferral (different reason — privacy boundary needs deciding first).

---

## What Changed From The Original Cognition Proposal

| Original direction | Trimmed direction (PR1–PR3 era) | Post-WK reframe (2026-05-07) |
|---|---|---|
| New `cel-cognition` crate first | Shared `PlanningView` first; `cel-cognition` planned for PR4 | Tier A work in `cel-cortex` directly; cel-cognition crate deferred indefinitely (eval-gated) |
| Broad sub-agent framework | Context/memory enrichment runtime with concrete enrichers, no planning ownership | Same — but no separate runtime. Enrichers (Tier A4) live in cel-cortex / cel-llm |
| Cortex framed as "the mind" | Cortex framed as grounding substrate for pluggable agents | Same — reinforced. Cognition is part of the Cortex column, not the agent column |
| `goal_decomposer` in rollout | Deferred to agent/reference helper territory | Same — confirmed not Cortex's job |
| LLM selector in first PR | Deterministic selector first; LLM selector optional later | WK1 ships deterministic FTS5+decay; LLM selector (B1) is now eval-gated, not scheduled |
| Persistent memory plus framework plus MCP plus telemetry in one PR | Split into PR1a/PR1b PlanningView, memory store, memory-aware selection, cognition runtime | Further split: PR1–PR3 + WK1/WK3/WK4/WK5 done; cognition becomes Tier A (small PRs) + Tier B (eval-gated) |
| `cel_perceive read` enriched by default | Raw read remains raw; planning view is explicit | Same |

The core insight remains: persistent memory matters, but selection matters more at prompt time. The post-WK reframe sharpens it: **selection is a Cortex service, not a runtime.** Cognition work lives where the memory and context live. The four weakness PRs proved we can deliver real cognition wins (FTS5 ranking, store handle abstraction, see-tool migration) without spinning up a new crate. PR4's "cel-cognition crate" framing was right that more work is needed; it was wrong that the work needs its own crate to be coherent. We'll create that crate when (and only when) the eval data shows the work outgrew its current home.
