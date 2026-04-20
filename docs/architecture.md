# CEL Runtime Architecture

## Overview

CEL (Computer Experience Layer) is a computer-use automation framework. It observes any application on screen, plans actions via LLM, executes them through app-specific adapters, and verifies the results — all in a tight loop.

The architecture has four components with strict ownership boundaries.

## Components

### Cortex — The I/O Layer

The Cortex owns all interaction with the outside world. It is CEL's nervous system.

**Perception**: Every 200ms, the Cortex reads from all available sources and builds one unified mental model:

```
Cortex tick (200ms):
  ├── Accessibility tree (macOS AXUIElement / Linux AT-SPI2)
  ├── Vision (screenshot → LLM, when < 5 actionable elements)
  ├── CDP (browser DOM extraction, when Chromium focused)
  ├── Adapter: Excel (COM / AppleScript)           ← plugin
  ├── Adapter: SAP GUI (scripting API)              ← plugin
  └── ... any registered adapter                    ← plugin
      │
      ▼
  Confidence scoring + dedup + noise suppression
      │
      ▼
  MentalModel {
      current_context    — all elements from all sources, unified
      element_adapter_index — which adapter owns which element
      temporal           — loading state, errors, idle tracking
      stability          — stable vs volatile elements
      anomalies          — dialogs, auth prompts, app switches
      freshness          — how stale the model is
  }
```

**Execution dispatch**: When the Goal Runner says "execute this action", the Cortex looks up which adapter owns the target element and routes the call:

```
cortex.execute(Click { target_id: "cell:A1" })
  → lookup "cell:A1" in element_adapter_index
  → found: adapter "excel"
  → excel_adapter.execute("click", { target_id: "cell:A1" })
  → return ActionResult { success: true }
```

The Cortex is a **dumb router** for execution. No retry, no escalation, no policy. Just dispatch.

**Adapter lifecycle**: The Cortex discovers adapters from `~/.cellar/adapters/`, reads their manifests, activates them when their target app is frontmost, and deactivates when the app loses focus.

**Owns**: adapter lifecycle, context fusion, execution dispatch, mental model, freshness, anomalies.
**Does NOT own**: planning, retry policy, escalation, verification decisions.

---

### Goal Runner — The Intelligence Layer

The Goal Runner is the main execution loop. It is the decision-making executive.

```
loop {
    // PERCEIVE — read Cortex mental model (0ms, Arc<RwLock>)
    context = cortex.read_model()

    // PLAN — call Planner (2-15s, LLM call)
    step = planner.plan_step(goal, context, history)

    // EXECUTE — dispatch through Cortex (routes to adapter, ~10ms)
    result = cortex.execute(step.action, context)

    // VERIFY — read fresh model, diff (0ms)
    after = cortex.read_model()
    verified = diff(context, after)

    // REFLECT — update cognitive state
    history.record(step, result, verified)
    notebook.write(step.notebook_writes)
    trail.add(step.reasoning, result)

    // GATE — should we continue?
    if done → return success
    if budget exhausted → return max_steps
    if loop_detected → escalate or replan
    if consecutive_failures → replan (T1-T4 tiers)
}
```

**Execution policy** (absorbed from the former TS runtime kernel):
- Strategy routing: structured → semantic → vision → terminal_failure
- Escalation: if structured fails, try semantic; if semantic fails, try vision
- Terminal failure: vision ceiling reached, stop
- Refresh: if context is stale, re-read before acting
- Side-effect detection: cross-app shift, no-diff warnings

**Cognitive loop** (orchestration intelligence):
- Loop detection: same action repeated? ping-pong? stale context?
- Replanning tiers:
  - T1 (1-2 failures): nudge in next prompt
  - T2 (3+ failures): new strategy, reset loop detector
  - T3 (strategy exhausted): backtrack to checkpoint
  - T4 (multiple milestones fail): full goal re-assessment
- Notebook: persist data (prices, URLs, confirmation numbers) across replans
- Cognitive trail: narrative log of decisions
- Strategy tracker: prevent trying the same failed approach twice
- Checkpoint manager: snapshot/restore for T3 backtracking

**Owns**: planning calls, execution policy, verification, cognitive loop, replanning.
**Does NOT own**: adapter details, context extraction, I/O dispatch.

---

### Planner — The Decision Maker

The Planner is a pure function: `(goal, context, history) → PlannedStep`.

It does NOT run its own loop — it is called by the Goal Runner on each step.

**Internals**:
1. Detect task type (navigation, extraction, form fill, comparison, general)
2. Build composable system prompt (rules tailored to task type)
3. Build user prompt with numbered element table
4. Inject adapter-specific actions when available (e.g., `write_cell` for Excel)
5. Call LLM (Gemini Flash default, Claude Sonnet for escalation)
6. Parse PlannedStep from response
7. Resolve numbered indices back to real element IDs

**Context distillation**: Before sending context to the LLM, the Planner scores elements by goal relevance (keyword matching, semantic synonyms, phrase boost, extraction-goal awareness) and sends only the top N elements.

**Owns**: prompt construction, model routing, context distillation, index resolution.
**Does NOT own**: when to plan (Runner decides), what to do on failure (Runner decides).

---

### Adapters — Cortex Drivers

Adapters are I/O + execution plugins for specific applications. They are managed by the Cortex.

**Declaration** (`adapter.json`):
```json
{
  "name": "excel",
  "display_name": "Microsoft Excel",
  "app_patterns": ["Microsoft Excel", "LibreOffice Calc"],
  "platform": ["macos", "windows"],
  "context": {
    "element_types": ["cell", "sheet_tab", "formula_bar", "ribbon_button"],
    "refresh_ms": 500,
    "confidence": 0.95
  },
  "actions": {
    "read_cell": { "params": {"row": "number", "col": "number"} },
    "write_cell": { "params": {"row": "number", "col": "number", "value": "string"} },
    "select_range": { "params": {"range": "string"} }
  }
}
```

**Interface** (all adapters implement this, regardless of language):
```
activate()      — connect to the target app's API
deactivate()    — disconnect and release resources
get_context()   → ContextElement[]  (CEL's native type)
execute(action, params) → ActionResult
probe()         → bool  (is the target app running?)
```

**Three runtimes**:

| Runtime | Language | Overhead | Use case |
|---------|----------|----------|----------|
| Native (Rust) | Rust `.dylib` | 0ms, in-process | First-party, performance-critical |
| Process | Any (Python, TS, Go) | ~0.5ms, stdio JSON lines | Community adapters |
| WASM | WASM-compilable | ~1ms, wasmtime | Sandboxed, portable (future) |

**Owns**: app-specific I/O (reading context, executing actions), capability declaration.
**Does NOT own**: lifecycle (Cortex manages), routing (Cortex dispatches), policy (Runner decides).

---

## Data Flow

```
Goal Runner                    Cortex                         Adapters
    │                            │                              │
    │── read_model() ──────────►│                              │
    │◄── MentalModel ──────────│  (200ms tick reads from:)    │
    │                            │  ├── a11y tree              │
    │── plan_step() ──►Planner  │  ├── CDP                    │
    │◄── PlannedStep           │  ├── adapter.get_context() ─►│
    │                            │  └── merge + score          │◄── ContextElement[]
    │── cortex.execute(action) ─►│                              │
    │                            │── lookup element→adapter ───►│
    │                            │◄── adapter.execute(action) ──│
    │◄── ActionResult ─────────│                              │
    │                            │                              │
    │── read_model() (verify) ──►│                              │
    │◄── MentalModel (fresh) ──│                              │
```

The Goal Runner never touches adapters. It talks only to the Cortex and the Planner. The Cortex handles all I/O routing. The Planner handles all LLM interaction.

---

## Ownership Boundaries

| Concern | Owner | NOT |
|---------|-------|-----|
| Adapter lifecycle | Cortex | Runner, Planner |
| Context reading (perception) | Cortex | Runner |
| Execution dispatch | Cortex | Runner |
| Mental model | Cortex | Runner |
| Freshness tracking | Cortex | Runner |
| Planning (LLM calls) | Planner (called by Runner) | Cortex |
| Context distillation | Planner | Cortex |
| Prompt construction | Planner | Runner |
| Execution policy (retry, escalate) | Runner | Cortex, Planner |
| Verification (did it land?) | Runner | Cortex |
| Loop detection | Runner | Planner |
| Replanning (T1-T4) | Runner | Planner |
| Cognitive state (notebook, trail) | Runner | Cortex |
| App-specific I/O | Adapters | Cortex, Runner |

---

## Implementation

| Component | Language | Crate/Package | Key file |
|-----------|----------|--------------|----------|
| Cortex | Rust | `cel-cortex` | `cortex.rs`, `adapter.rs` |
| Goal Runner | Rust | `cel-goal-runner` | `runner.rs` |
| Planner | Rust | `cel-planner` | `planner.rs`, `prompt.rs` |
| Adapters (native) | Rust | `adapter-common` | per-adapter crate |
| Adapters (process) | Any | SDK packages | `cellar-adapter-sdk` |
| MCP Server | TypeScript | `mcp-server` | `server.ts` |
| NAPI Bridge | Rust | `cel-napi` | `goal_runner.rs`, `cortex.rs` |

The MCP server is the TS entry point. It boots the Cortex, creates the NAPI bridge, and exposes CEL's 4 MCP tools (cel_see, cel_act, cel_think, cel_perceive). The `run_goal` mode in cel_think calls the Rust Goal Runner via NAPI.

---

## Adapter Development

Community adapters communicate with CEL via a simple JSON-lines protocol over stdio:

```
← {"method":"activate"}
→ {"ok":true}

← {"method":"get_context"}
→ {"elements":[{"id":"A1","element_type":"cell","label":"Revenue","value":"1420000",...}]}

← {"method":"execute","action":"write_cell","params":{"row":1,"col":2,"value":"hello"}}
→ {"success":true}

← {"method":"deactivate"}
→ {"ok":true}
```

An adapter package is a directory in `~/.cellar/adapters/` with:
- `adapter.json` — manifest declaring capabilities
- An entrypoint (e.g., `adapter.py`, `adapter.ts`, `adapter` binary)

SDKs handle the protocol boilerplate:
- Python: `pip install cellar-adapter-sdk`
- TypeScript: `npm install @cellar/adapter-sdk`
- Rust: `cargo add cellar-adapter-sdk` (in-process, zero overhead)

---

## Benchmark Results (April 2026)

Hybrid suite — 5 tasks testing browser-desktop handoff, stale state recovery, ambiguous targets, side-effect detection, and terminal failure handling.

| Tool | Avg Time | LLM Calls | Cost/Task | Success |
|------|----------|-----------|-----------|---------|
| **CEL** | **20.8s** | **1.4** | **$0.0005** | **100%** |
| Browser-Use OSS | 23.4s | 3.0 | $0.001 | 100% |
| Stagehand v3 | 35.6s | 18.2 | $0.005 | 20% |
| Computer Use | 36.2s | 6.2 | $0.155 | 100% |
| Browser-Use Cloud | 46.5s | 5.6 | $0.003 | 100% |

CEL is **1.7x faster** and **310x cheaper** than Anthropic Computer Use, with the same accuracy.
