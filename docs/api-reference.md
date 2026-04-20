# API Reference

CEL exposes its functionality at three levels: the MCP server (for AI agents), the TypeScript library (for custom agents), and the Rust core (for low-level integration). The MCP server provides 4 tools: `cel_see`, `cel_act`, `cel_think`, and `cel_perceive`.

## MCP Tools (v0.2.0)

See [mcp-server.md](mcp-server.md) for usage examples with each tool.

### cel_see — Read the Screen

| Mode | Parameters | Description |
|------|-----------|-------------|
| `context` | `filter?` (element_types, min_confidence, detail) | Screen context with UI elements |
| `screenshot` | — | PNG screenshot as base64 image |
| `windows` | — | Visible window list |
| `monitors` | — | Display list |
| `focused` | `element_id` | High-fidelity element detail |
| `element_at` | `x`, `y` | Hit-test screen coordinates |
| `is_settable` | `element_id` | Check direct value support |
| `make_reference` | `element_id` | Create resilient element reference |
| `cursor_position` | — | Current mouse position |
| `cdp_status` | — | CDP setup status + browser targets |
| `cdp_page` | — | Full page content via CDP |
| `wait_for_element` | `element_type?`, `label_contains?`, `timeout_ms`, `poll_interval_ms` | Poll for element |
| `wait_for_idle` | `timeout_ms`, `poll_interval_ms` | Poll for stable screen |
| `watch` | `events[]`, `timeout_ms`, `poll_interval_ms` | Event-driven waiting |

### cel_act — Execute Actions

**Single action:**

| Action | Parameters | Description |
|--------|-----------|-------------|
| `click` | `x, y` or `target_ref` | Left-click |
| `right_click` | `x, y` or `target_ref` | Right-click |
| `double_click` | `x, y` or `target_ref` | Double-click |
| `mouse_move` | `x, y` or `target_ref` | Move cursor |
| `type` | `text` | Type text |
| `key_press` | `key` | Press key (Enter, Tab, etc.) |
| `key_combo` | `keys[]` | Key combination (["Ctrl", "C"]) |
| `scroll` | `dx, dy`, optional `x, y` | Scroll |
| `drag` | `from_x, from_y, to_x, to_y` | Drag and drop |
| `ax_action` | `element_id`, `ax_action` | Native accessibility action |
| `set_value` | `element_id`, `value` | Direct value injection |
| `cdp_eval` | `expression` | Execute JavaScript in browser via CDP |

**Batch actions:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `actions` | `Action[]` (1-4) | Array of single actions |
| `delay_between_ms` | `number` (default: 100) | Delay between actions |

### cel_think — Plan, Remember, Track

| Mode | Parameters | Description |
|------|-----------|-------------|
| `run_goal` | `goal`, `max_steps?`, `timeout_ms?`, `enable_vision?`, `self_heal?`, `context_lazy?`, `decompose?`, `workflow_name?`, `enable_notebook?` | Full autonomous see→plan→act loop |
| `plan` | `goal`, `history?`, `max_steps?`, `loop_warning?` | LLM step planning |
| `plan_with_vision` | `goal`, `history?`, `max_steps?` | Plan with screenshot |
| `search_knowledge` | `query`, `workflow_scope?`, `limit` | FTS5 search |
| `store_knowledge` | `content`, `source`, `workflow_scope?`, `tags?` | Store fact |
| `memory_get` | `workflow_name` | Read working memory |
| `memory_set` | `workflow_name`, `content` | Update working memory |
| `observe` | `workflow_name`, `content`, `priority`, `source_run_ids?` | Record observation |
| `get_observations` | `workflow_name`, `limit?` | Get observations |
| `run_start` | `workflow_name`, `steps_total` | Start tracking run |
| `run_finish` | `run_id`, `status` | Finish run |
| `run_log_step` | `run_id`, `step_index`, `step_id`, `action`, `success`, `confidence`, `context_snapshot?`, `error?` | Log step |
| `run_history` | `limit` | Recent runs |
| `run_steps` | `run_id` | Steps for a run |
| `llm_complete` | `system_prompt`, `user_prompt`, `max_tokens?` | Text LLM call |
| `llm_complete_with_image` | `system_prompt`, `user_prompt`, `image_base64`, `max_tokens?` | Vision LLM call |
| `eviction` | `run_retention_days?`, `knowledge_retention_days?` | TTL cleanup |

### cel_perceive — Always-On Perception (Cortex)

Singleton perception engine that maintains a continuously-updated mental model via background event streams. Only one perception session can be active at a time. `cel_see` `watch` mode is unavailable during an active session.

| Mode | Parameters | Description |
|------|-----------|-------------|
| `start` | `goal`, `enable_suggestions?` (default: true) | Boot the Cortex with a goal. Starts background event monitoring with periodic accessibility tree refreshes. |
| `read` | — | Get the mental model snapshot (instant — model kept warm by background events). Includes LLM-powered next-action suggestions if enabled. |
| `feed` | `action`, `target`, `expected` | Report an action you took. Cortex waits for screen to settle, diffs against current model, returns verification. |
| `checkpoint` | `summary` | Summarize completed work and reset action history. Use between phases of multi-step tasks. |
| `configure` | `goal?`, `enable_suggestions?` | Update goal or enable_suggestions mid-session. |
| `status` | — | Cortex health — confidence score, uptime, cycle count, element counts (stable vs volatile), temporal state (loading, errors, focus trail). |
| `stop` | — | Shutdown the Cortex and get a summary. |

## TypeScript API (@cellar/agent)

### Cel Class

```typescript
import { Cel } from "@cellar/agent";
const cel = new Cel(dbPath?: string); // default: ~/.cellar/cel-store.db
```

#### Properties

| Property | Type | Description |
|----------|------|-------------|
| `isNativeAvailable` | `boolean` | Whether the Rust native module is loaded |

#### Context

| Method | Returns | Description |
|--------|---------|-------------|
| `getContext()` | `ScreenContext` | Unified screen context with all elements |
| `captureScreen()` | `Buffer` | PNG screenshot buffer |
| `listMonitors()` | `MonitorInfo[]` | Available monitors |
| `listWindows()` | `WindowInfo[]` | Visible windows |
| `getContextFocused(elementId)` | `FocusedContext \| null` | High-fidelity element detail |
| `axElementAtPosition(x, y)` | `ContextElement \| null` | Hit-test coordinates |
| `axIsSettable(elementId)` | `boolean` | Check direct value support |
| `mousePosition()` | `[number, number]` | Current cursor position |

#### Input Actions

| Method | Parameters | Description |
|--------|-----------|-------------|
| `click(x, y)` | `number, number` | Left-click |
| `rightClick(x, y)` | `number, number` | Right-click |
| `doubleClick(x, y)` | `number, number` | Double-click |
| `mouseMove(x, y)` | `number, number` | Move cursor |
| `typeText(text)` | `string` | Type text |
| `keyPress(key)` | `string` | Press key |
| `keyCombo(keys)` | `string[]` | Key combination |
| `scroll(dx, dy)` | `number, number` | Scroll |
| `drag(fromX, fromY, toX, toY)` | `number × 4` | Drag and drop |

#### Accessibility Actions

| Method | Parameters | Description |
|--------|-----------|-------------|
| `axPerformAction(elementId, action)` | `string, string` | Native a11y action (more reliable than click) |
| `axSetValue(elementId, value)` | `string, string` | Direct value injection (faster than type) |

#### Context References

| Method | Returns | Description |
|--------|---------|-------------|
| `makeReference(element)` | `ContextReference` | Create stable reference |
| `resolveReference(context, ref)` | `ContextElement \| null` | Resolve reference |

#### Knowledge Store

| Method | Returns | Description |
|--------|---------|-------------|
| `searchKnowledge(query, scope?, limit?)` | `ScoredKnowledgeRecord[]` | FTS5 search |
| `addScopedKnowledge(content, source, scope?, tags?)` | `number` | Store scoped fact |

#### Working Memory

| Method | Returns | Description |
|--------|---------|-------------|
| `getWorkingMemory(workflowName)` | `string` | Read scratchpad |
| `updateWorkingMemory(workflowName, content)` | `void` | Update scratchpad |

#### Observations

| Method | Returns | Description |
|--------|---------|-------------|
| `addObservation(workflowName, content, priority, sourceRunIds)` | `number` | Record observation |
| `getObservations(workflowName, limit?)` | `ObservationRecord[]` | Get observations |

#### Run Tracking

| Method | Returns | Description |
|--------|---------|-------------|
| `startRun(workflowName, stepsTotal)` | `number` | Start tracking |
| `finishRun(runId, status)` | `void` | Finish run |
| `logStep(runId, stepIndex, stepId, action, success, confidence, snapshot?, error?)` | `number` | Log step |
| `getRunHistory(limit?)` | `RunRecord[]` | Recent runs |
| `getStepResults(runId)` | `StepRecord[]` | Steps for a run |

#### Planner

| Method | Returns | Description |
|--------|---------|-------------|
| `planStep(goal, context, history?)` | `Promise<PlannedStep>` | LLM step planning |
| `planStepWithVision(goal, context, screenshot, history?)` | `Promise<PlannedStep>` | Plan with screenshot |
| `buildPlanPrompt(goal, context, history?)` | `{system, user, index_map}` | Build prompts without LLM call |

#### LLM

| Method | Returns | Description |
|--------|---------|-------------|
| `llmComplete(systemPrompt, userPrompt, maxTokens?)` | `Promise<string>` | Text completion |
| `llmCompleteWithImage(systemPrompt, imageBase64, userPrompt, maxTokens?)` | `Promise<string>` | Vision completion |

#### CDP

| Method | Returns | Description |
|--------|---------|-------------|
| `getCdpPageContent()` | `Promise<PageContent \| null>` | Page content via CDP |
| `discoverCdpTargets()` | `CdpTarget[]` | Available debug targets |
| `isCdpSetup()` | `boolean` | CDP setup status |

#### Watchdog

| Method | Returns | Description |
|--------|---------|-------------|
| `startWatchdog()` | `void` | Start change detection |
| `pollEvents()` | `CelEvent[]` | Poll for events |
| `stopWatchdog()` | `void` | Stop watchdog |

#### Eviction

| Method | Returns | Description |
|--------|---------|-------------|
| `runEviction(runRetentionDays?, knowledgeRetentionDays?)` | `EvictionResult` | TTL cleanup |

## Core Types

### ScreenContext

```typescript
interface ScreenContext {
  app: string;
  window: string;
  elements: ContextElement[];
  network_events?: NetworkEvent[];
  http_events?: HttpEvent[];
  timestamp_ms: number;
  screen_width?: number;
  screen_height?: number;
  clipboard?: string;
  window_list?: WindowInfo[];
  audio?: AudioInfo;
  power?: PowerInfo;
  running_apps?: string[];
  recent_files?: string[];
  transcripts?: TranscriptEntry[];
}
```

### TranscriptEntry

```typescript
interface TranscriptEntry {
  text: string;
  start_ms: number;
  end_ms: number;
  source: "microphone" | "system_output" | "both";
  speaker?: string;
  confidence?: number;
}
```

### ContextElement

```typescript
interface ContextElement {
  id: string;
  label?: string;
  description?: string;
  element_type: string;
  value?: string;
  bounds?: Bounds;
  state: ElementState;
  parent_id?: string;
  actions?: string[];
  confidence: number;
  source: "accessibility_tree" | "native_api" | "vision" | "merged";
}
```

### ContextReference

```typescript
interface ContextReference {
  element_type: string;
  label?: string;
  ancestor_path?: string[];
  bounds_region?: BoundsRegion;
  value_pattern?: string;
}
```

### Bounds

```typescript
interface Bounds {
  x: number;
  y: number;
  width: number;
  height: number;
}
```

## Rust API (cel-cortex)

### Cortex Builder

```rust
use cel_cortex::Cortex;
use cel_audio::{CpalCapture, AudioConfig, AudioSource, WhisperApiConfig, WhisperApiTranscriber};
use std::sync::Arc;

// Minimal — accessibility only, no audio
let cortex = Cortex::new("my-cortex".into());

// With audio transcription
let transcriber = Arc::new(WhisperApiTranscriber::new(WhisperApiConfig {
    api_key: std::env::var("OPENAI_API_KEY").unwrap(),
    ..WhisperApiConfig::default()
}));
let mut capture = CpalCapture::new();
capture.set_transcriber(transcriber);

let cortex = Cortex::new("my-cortex".into())
    .with_audio(
        Box::new(capture),
        AudioConfig { source: AudioSource::Both, transcribe: true, ..Default::default() },
    )
    .with_tick_ms(200);

// Start the perception loop (merger = context source, observer = push-event source)
use cel_accessibility::create_tree;
let merger = cel_context::ContextMerger::new(create_tree());
let observer = create_tree();
cortex.boot(merger, observer).await?;

// Access the live mental model
let model = cortex.model(); // Arc<RwLock<MentalModel>>
let snapshot = model.read().await.current_context.clone(); // ScreenContext

// Shutdown
cortex.shutdown();
```

### AudioConfig Fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `source` | `AudioSource` | `Both` | `Microphone`, `SystemOutput`, or `Both` |
| `sample_rate` | `u32` | `16_000` | Target sample rate (Hz) |
| `channels` | `u16` | `1` | Channel count |
| `ring_buffer_secs` | `u32` | `60` | Rolling buffer window |
| `transcribe` | `bool` | `true` | Enable Whisper transcription |
| `redact_on_password_focus` | `bool` | `true` | Drop audio during password field focus |

### WhisperApiConfig Fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `endpoint` | `String` | OpenAI `/v1/audio/transcriptions` | Compatible with OpenAI, Groq, faster-whisper |
| `api_key` | `String` | `""` | Bearer token (may be empty for local servers) |
| `model` | `String` | `"whisper-1"` | Model name passed to the endpoint |
| `language` | `Option<String>` | `None` | ISO 639-1 hint (`"en"`, `"de"`, …) |
| `timeout_secs` | `u64` | `30` | HTTP request timeout |

### Test Seam

```rust
// Cortex::isolated() returns a (Cortex, ContextMerger) using StubAccessibility
// — no OS permissions required, suitable for unit/integration tests.
use cel_accessibility::StubAccessibility;
let (mut cortex, merger) = Cortex::isolated("test");
cortex.boot(merger, Box::new(StubAccessibility)).await?;
```

## Environment Variables

### LLM

| Variable | Description | Example |
|----------|-------------|---------|
| `CEL_LLM_PROVIDER` | LLM provider | `openai`, `anthropic`, `gemini`, `ollama`, `compatible` |
| `CEL_LLM_API_KEY` | API key | `sk-...` |
| `CEL_LLM_MODEL` | Model name | `gpt-4o`, `claude-sonnet-4-6` |
| `CEL_LLM_ENDPOINT` | Custom endpoint | `http://localhost:11434/v1` |

### Audio / Transcription

| Variable | Description | Example |
|----------|-------------|---------|
| `CEL_WHISPER_ENDPOINT` | Transcription API URL | `http://localhost:9000/v1/audio/transcriptions` |
| `CEL_WHISPER_API_KEY` | Bearer token for transcription API | `sk-...` (or empty for local) |
| `CEL_WHISPER_MODEL` | Whisper model name | `whisper-1`, `large-v3` |
| `CEL_WHISPER_LANGUAGE` | ISO 639-1 language hint | `en`, `de`, `fr` |

## Configuration File

`dilipod init` writes a config file at `~/.cellar/config.toml`. Environment variables override config file values; config file values override compiled defaults.

```toml
[llm]
provider = "gemini"
api_key  = "your-key"
model    = "gemini-2.0-flash"

[audio]
whisper_endpoint = "https://api.openai.com/v1/audio/transcriptions"
whisper_api_key  = "sk-..."
whisper_model    = "whisper-1"
whisper_language = "en"   # optional
```

**Precedence:** env vars > `~/.cellar/config.toml` > compiled defaults.
