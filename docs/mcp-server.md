# CEL MCP Server

CEL exposes its capabilities as an [MCP](https://modelcontextprotocol.io/) server, making it available to Claude Code, Cursor, Codex, GPT-based tool callers, LangGraph runtimes, and any other MCP-compatible client.

The MCP server is the main agent-facing boundary for the platform:

- `CEL` owns perception, execution, context fusion, and adapter-backed truth
- the MCP host owns planning by default
- `cel_think` remains available as an optional built-in planner / memory layer when you explicitly want CEL to take over the loop

The server sends **instructions** to every client on connect so an agent can use CEL without bespoke glue docs.

## Setup

### Claude Code / Cursor / Any MCP Client

```bash
# Build everything
pnpm install && pnpm -r build
```

Add to your MCP client settings:

```json
{
  "mcpServers": {
    "cel": {
      "command": "node",
      "args": ["/path/to/cellar/mcp-server/dist/index.js"]
    }
  }
}
```

Or via CLI: `cellar mcp`

### Manual / Debugging

```bash
# Stdio mode
cellar mcp

# MCP Inspector (visual testing)
npx @modelcontextprotocol/inspector node mcp-server/dist/index.js
```

## Surface: See → Act → Perceive + Optional Think

CEL uses four tools organized by intent:

| Tool | Purpose | Modes/Actions |
|------|---------|---------------|
| **cel_see** | Read screen state | 14 modes |
| **cel_act** | Execute actions | native input, CDP, and deterministic app actions |
| **cel_think** | Optional built-in planning, memory, autonomous execution | 17 modes |
| **cel_perceive** | Always-on perception (Cortex) | 7 modes |

Plus 5 [**prompts**](#prompts--reusable-quick-start-templates) — quick-start templates the host surfaces as commands (`cellar/setup-task`, `cellar/inspect-app`, `cellar/debug-hung-action`, `cellar/extract-table`, `cellar/run-numbers-write`).

---

## cel_see — Read the Screen

Returns the current screen state as structured JSON. Always use this **before** acting.

### Modes

| Mode | What it returns |
|------|----------------|
| `context` | Complete `ScreenContext` with all detected elements. Supports filtering by type, confidence, and detail level (full/compact/actionable_only/summary). Enriched with CDP page content when available. |
| `screenshot` | PNG screenshot as base64 image |
| `windows` | List of visible windows (id, title, app, bounds) |
| `monitors` | List of monitors (id, resolution, position) |
| `focused` | High-fidelity detail for a single element (subtree + ancestors) |
| `element_at` | Hit-test: which accessibility element is at screen coordinates (x, y) |
| `is_settable` | Check if an element supports direct value setting via `set_value` |
| `make_reference` | Create a resilient reference from an element ID |
| `cursor_position` | Current mouse cursor coordinates |
| `cdp_status` | CDP setup status and available browser debug targets |
| `cdp_page` | Full page content from focused browser tab via Chrome DevTools Protocol |
| `wait_for_element` | Poll until a matching element appears on screen |
| `wait_for_idle` | Poll until the screen stops changing |
| `watch` | Event-driven waiting (18 event types including tree_changed, focus_changed, value_changed, menu_opened, etc.) |

### Examples

**Get full context:**

```json
{ "mode": "context" }
```

**Filter to buttons and links above 70% confidence, compact format:**

```json
{
  "mode": "context",
  "filter": {
    "element_types": ["button", "link"],
    "min_confidence": 0.7,
    "detail": "compact"
  }
}
```

**Take a screenshot:**

```json
{ "mode": "screenshot" }
```

**Check if an element supports direct value setting:**

```json
{ "mode": "is_settable", "element_id": "a11y:42" }
```

**Wait for a submit button to appear:**

```json
{
  "mode": "wait_for_element",
  "element_type": "button",
  "label_contains": "Submit",
  "timeout_ms": 10000
}
```

**Watch for focus changes:**

```json
{
  "mode": "watch",
  "events": ["focus_changed", "value_changed"],
  "timeout_ms": 5000
}
```

---

## cel_act — Execute Actions

Click, type, scroll, drag, and interact via the native accessibility API.

### Key Patterns

- **Prefer `ax_action` over `click`** for buttons/checkboxes — uses the native accessibility API, more reliable than coordinate-based clicking.
- **Prefer `set_value` over `type`** for form fields — faster and more reliable, bypasses keyboard entirely. Use `cel_see` `is_settable` mode to check first.
- **Prefer `write_cells` / `read_cells` over AX text guessing in Numbers** — these use the Numbers document model directly.
- For coordinate-based actions, provide `(x, y)` or a `target_ref` from `cel_see` `make_reference`.
- Batch up to 4 actions, then re-observe with `cel_see` between batches.

### Actions

| Action | Parameters | Description |
|--------|-----------|-------------|
| `click` | `x, y` or `target_ref` | Left-click |
| `right_click` | `x, y` or `target_ref` | Right-click |
| `double_click` | `x, y` or `target_ref` | Double-click |
| `mouse_move` | `x, y` or `target_ref` | Move cursor |
| `type` | `text` | Type text via keyboard |
| `key_press` | `key` | Press a single key (Enter, Tab, Escape) |
| `key_combo` | `keys[]` | Key combination (["Ctrl", "C"]) |
| `scroll` | `dx, dy`, optional `x, y` | Scroll at position |
| `drag` | `from_x, from_y, to_x, to_y` | Drag and drop |
| `ax_action` | `element_id, ax_action` | Native accessibility action (click, activate, press, increment, decrement, cancel, show_menu, scroll_to_visible, raise, pick, delete) |
| `set_value` | `element_id, value` | Direct value injection (text for fields, "true"/"false" for checkboxes) |
| `cdp_eval` | `expression` | Execute JavaScript in browser via Chrome DevTools Protocol — best for cookie banners, iframes, overlays, and elements invisible to the accessibility tree |
| `write_cells` | `app?, sheet?, table?, writes[], verify?` | Deterministic spreadsheet write via app model (currently Numbers) |
| `read_cells` | `app?, sheet?, table?, cell_refs[]` | Deterministic spreadsheet read via app model (currently Numbers) |

### Examples

**Click using accessibility action (preferred):**

```json
{ "action": "ax_action", "element_id": "a11y:42", "ax_action": "click" }
```

**Set a form field value directly (preferred for inputs):**

```json
{ "action": "set_value", "element_id": "a11y:43", "value": "my-username" }
```

**Click by coordinates:**

```json
{ "action": "click", "x": 520, "y": 420 }
```

**Click using element reference (survives layout changes):**

```json
{
  "action": "click",
  "target_ref": { "element_type": "button", "label": "Sign in" }
}
```

**Drag and drop:**

```json
{ "action": "drag", "from_x": 100, "from_y": 200, "to_x": 400, "to_y": 200 }
```

**Execute JavaScript in browser via CDP (cookie banners, iframes, overlays):**

```json
{ "action": "cdp_eval", "expression": "document.querySelector('.cookie-banner .accept')?.click()" }
```

**Batch actions (fill a form):**

```json
{
  "actions": [
    { "action": "set_value", "element_id": "a11y:10", "value": "my-username" },
    { "action": "set_value", "element_id": "a11y:11", "value": "my-password" },
    { "action": "ax_action", "element_id": "a11y:12", "ax_action": "click" }
  ],
  "delay_between_ms": 100
}
```

**Write Numbers cells deterministically:**

```json
{
  "action": "write_cells",
  "app": "Numbers",
  "writes": [
    { "cell_ref": "A1", "value": "BTC" },
    { "cell_ref": "B1", "value": "ETH" },
    { "cell_ref": "C1", "value": "SOL" }
  ],
  "verify": true
}
```

**Read Numbers cells back from the document model:**

```json
{
  "action": "read_cells",
  "app": "Numbers",
  "cell_refs": ["A1", "B1", "C1"]
}
```

---

## cel_think — Optional Built-In Planner / Memory Layer

Optional built-in layer for delegated autonomy, planning, knowledge management, workflow tracking, and LLM calls.

### Modes

| Mode | Parameters | Description |
|------|-----------|-------------|
| `run_goal` | `goal`, `max_steps?` (default 80), `timeout_ms?` (default 900000) | Canonical see→plan→act loop. Vision, self-healing, decomposition, and notebook behavior are implicit in the canonical loop — no per-invocation knobs (see [canonical-agent-plan.md](canonical-agent-plan.md)). |
| `plan` | `goal`, `history?`, `max_steps?` | LLM-powered step planning given current screen context |
| `plan_with_vision` | `goal`, `history?` | Same as plan but also sends a screenshot for visual grounding |
| `search_knowledge` | `query`, `workflow_scope?`, `limit` | FTS5 full-text search over knowledge base |
| `store_knowledge` | `content`, `source`, `workflow_scope?`, `tags?` | Save a fact to the knowledge base |
| `memory_get` | `workflow_name` | Read per-workflow working memory |
| `memory_set` | `workflow_name`, `content` | Update per-workflow working memory |
| `observe` | `workflow_name`, `content`, `priority`, `source_run_ids?` | Record an observation from past runs |
| `get_observations` | `workflow_name`, `limit?` | Get active observations |
| `run_start` | `workflow_name`, `steps_total` | Begin tracking a workflow run |
| `run_finish` | `run_id`, `status` | Mark run as completed or failed |
| `run_log_step` | `run_id`, `step_index`, `step_id`, `action`, `success`, `confidence` | Log a step result |
| `run_history` | `limit` | Recent workflow run history |
| `run_steps` | `run_id` | Step-by-step results for a specific run |
| `llm_complete` | `system_prompt`, `user_prompt` | Text-only LLM completion |
| `llm_complete_with_image` | `system_prompt`, `user_prompt`, `image_base64` | LLM completion with image |
| `eviction` | `run_retention_days?`, `knowledge_retention_days?` | TTL cleanup of old data |

### Examples

**Autonomous execution (run_goal):**

```json
{
  "mode": "run_goal",
  "goal": "Open Safari and search for weather in Athens",
  "max_steps": 30,
  "enable_vision": true,
  "self_heal": true,
  "workflow_name": "weather-check"
}
```

**Store knowledge:**

```json
{
  "mode": "store_knowledge",
  "content": "The SAP login page requires employee ID in field 'MANDT'",
  "source": "user-observation",
  "tags": "sap,login"
}
```

**Search knowledge:**

```json
{ "mode": "search_knowledge", "query": "login credentials", "limit": 5 }
```

**Track a workflow run:**

```json
{ "mode": "run_start", "workflow_name": "daily-report", "steps_total": 5 }
```

---

## cel_perceive — Always-On Perception (Cortex)

CEL's always-on perception engine. Maintains a continuously-updated mental model via background event streams with periodic accessibility tree refreshes on significant changes, and vision/screenshots when flagged as needed.

**Singleton** — only one perception session can be active at a time. `cel_see` `watch` mode is unavailable during an active session.

### Workflow: Perceive → Act → Feed

1. **`start`** — Boot the Cortex with a goal. Starts background event monitoring.
2. **`read`** — Get the mental model snapshot (instant — kept warm by background events).
3. **Act** — Execute actions via `cel_act`.
4. **`feed`** — Report the action back. Cortex waits for screen to settle, diffs against current model, returns verification.
5. Repeat 2-4 until goal achieved, then **`stop`**.

Use `cel_perceive` for multi-step tasks where continuous awareness matters. Use `cel_see` for quick one-off observations.

### Modes

| Mode | Parameters | Description |
|------|-----------|-------------|
| `start` | `goal`, `enable_suggestions?` (default: true) | Boot the Cortex with a goal. LLM-powered next-action suggestions on each read. |
| `read` | — | Instant mental model snapshot. Includes element stability (stable vs volatile), temporal state (loading, errors, focus trail), and optional next-action suggestions. |
| `feed` | `action`, `target`, `expected` | Report action taken. Cortex waits for screen to settle, diffs against model, returns verification. |
| `checkpoint` | `summary` | Summarize completed work and reset action history. Use between phases of multi-step tasks. |
| `configure` | `goal?`, `enable_suggestions?` | Update goal or suggestion settings mid-session. |
| `status` | — | Cortex health — confidence score, uptime, cycle count, element counts, temporal state. |
| `stop` | — | Shutdown the Cortex and get a summary. |

### Examples

**Start a perception session:**

```json
{ "mode": "start", "goal": "Fill out the registration form", "enable_suggestions": true }
```

**Read current screen model (instant):**

```json
{ "mode": "read" }
```

**Report an action and verify:**

```json
{ "mode": "feed", "action": "set_value", "target": "email field", "expected": "value should be filled" }
```

**Checkpoint between phases:**

```json
{ "mode": "checkpoint", "summary": "Completed personal info section, moving to payment" }
```

**Stop the session:**

```json
{ "mode": "stop" }
```

---

## Prompts — Reusable Quick-Start Templates

In addition to tools, the CEL server exposes the third MCP primitive: **prompts**. These are static templates the host surfaces to the user as quick-start commands (e.g. `/cellar/setup-task` in Claude Code) so they don't need to remember tool names or compose long instructions.

Prompts are returned regardless of cel-napi availability — they're static templates that don't touch the native module, so they work in schema-only mode too.

| Prompt | Arguments | What it does |
|--------|-----------|--------------|
| `cellar/setup-task` | `goal` (required) | Boots Cortex with the goal and walks the host through the recommended perceive → see → act → feed loop |
| `cellar/inspect-app` | none | Identifies the focused app + its accessibility surface (windows, focused element, monitor scale_factor) |
| `cellar/debug-hung-action` | `action` (optional) | Diagnoses why an action didn't land — reads Cortex status, recent diffs, and lists common causes |
| `cellar/extract-table` | `app_hint` (optional) | Pulls a structured table from the screen, with branches for AX-friendly apps, Numbers, and browser DOM |
| `cellar/run-numbers-write` | `sheet` (optional), `cells` (required JSON) | Writes cells deterministically into Numbers via the structured app-truth path (`write_cells`) |

### How hosts surface them

In Claude Code: type `/` and prompts appear as commands you can pick.
In MCP Inspector: the **Prompts** tab lists them with their arguments.
Programmatically: send `prompts/list` for the catalog and `prompts/get` to materialize one with arguments.

### Adding more

Prompts live in [`mcp-server/src/prompts.ts`](../mcp-server/src/prompts.ts). To add one, append a `PromptDefinition` to the `PROMPTS` array — the registration loop wires it up automatically.

---

## Context References

Element IDs are ephemeral — they change between context snapshots. Context references solve this by identifying elements through multiple signals:

1. **Element type** — must match exactly
2. **Label** — fuzzy matched (case-insensitive, partial)
3. **Bounds region** — coarse spatial position (quadrant-based)
4. **Value pattern** — expected content

**Workflow:**

1. Get context with `cel_see` `context` mode
2. Create a reference: `cel_see` with `mode: "make_reference"` and the element's `id`
3. Use the reference in actions: `cel_act` with `target_ref` instead of `x`/`y`

## How Context Fusion Works

When you call `cel_see` with `mode: "context"`, CEL merges data from multiple sources:

1. **Native API** (highest priority) — deterministic, precise
2. **Accessibility tree** — structured, reliable on modern apps
3. **Vision** (fallback) — triggered when a11y tree is sparse
4. **Network** (supplementary) — connection state signals
5. **CDP** (enrichment) — page content from Chrome DevTools Protocol

Each element gets a confidence score (0.0-1.0). The agent can use confidence to decide how to act:
- 0.9+ — act immediately
- 0.7-0.9 — act and verify
- Below 0.7 — pause and ask the user

## Programmatic Usage (Node.js)

```typescript
import { Cel } from "@cellar/agent";

const cel = new Cel();

// Read the screen
const ctx = cel.getContext();

// Prefer accessibility actions over coordinates
const btn = ctx.elements.find(
  (el) => el.element_type === "button" && el.label?.includes("Submit")
);
if (btn) {
  cel.axPerformAction(btn.id, "click");
}

// Direct value setting for form fields
const input = ctx.elements.find(
  (el) => el.element_type === "input" && el.label?.includes("Username")
);
if (input && cel.axIsSettable(input.id)) {
  cel.axSetValue(input.id, "my-username");
}
```

### Starting the MCP server programmatically

```typescript
import { createCelMcpServer } from "@dpagk/cellar-mcp/server.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";

const server = createCelMcpServer();
const transport = new StdioServerTransport();
await server.connect(transport);
```

## Environment Variables

| Variable | Description | Example |
|----------|-------------|---------|
| `CEL_LLM_PROVIDER` | LLM provider for vision/planner | `openai`, `anthropic`, `gemini`, `ollama`, `compatible` |
| `CEL_LLM_API_KEY` | API key for the LLM provider | `sk-...` |
| `CEL_LLM_MODEL` | Model name | `gpt-4o`, `claude-sonnet-4-6` |
| `CEL_LLM_ENDPOINT` | Custom endpoint | `http://localhost:11434/v1` |

## Requirements

- **macOS** with Accessibility permissions granted to the host process
- **CDP features**: Chrome/Chromium with remote debugging (`cel setup cdp`)
- **Knowledge store**: SQLite at `~/.cellar/cel-store.db` (created automatically)
