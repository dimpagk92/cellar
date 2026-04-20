/** Bounds in screen coordinates. */
export interface Bounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Element state flags from the accessibility tree. */
export interface ElementState {
  focused: boolean;
  enabled: boolean;
  visible: boolean;
  selected: boolean;
  /** For expandable elements (trees, accordions). null if not expandable. */
  expanded?: boolean | null;
  /** For checkable elements (checkboxes, radio buttons). null if not checkable. */
  checked?: boolean | null;
}

/** A single UI element from the unified context API. */
export interface ContextElement {
  id: string;
  label?: string;
  /** Accessibility description (tooltip / secondary label). */
  description?: string;
  element_type: string;
  value?: string;
  bounds?: Bounds;
  /** Element state flags (focused, enabled, visible, etc.). */
  state: ElementState;
  /** ID of the parent element, preserving tree hierarchy. */
  parent_id?: string | null;
  /** Available actions from AT-SPI2 Action interface: "click", "press", "activate", etc. */
  actions?: string[];
  confidence: number;
  source: "accessibility_tree" | "native_api" | "vision" | "merged";
  /**
   * Content role for prompt injection defense.
   * "interactive" = safe to act on, "content" = untrusted text (READ only),
   * "decorative" = ignore, "system" = chrome UI.
   */
  content_role?: "interactive" | "content" | "decorative" | "system";
  /** Extended properties from accessibility or native API (e.g., url, placeholder, required). */
  properties?: Record<string, string>;
}

/** Clipboard state from the signal bus. */
export interface ClipboardState {
  text?: string;
  has_image: boolean;
  has_files: boolean;
}

/** A visible window on screen (from CGWindowList). */
export interface WindowState {
  app_name: string;
  title: string;
  x: number;
  y: number;
  width: number;
  height: number;
  layer: number;
  is_on_screen: boolean;
  pid: number;
}

/** Audio output state. */
export interface AudioState {
  volume: number;
  is_muted: boolean;
}

/** Battery/power state. */
export interface PowerState {
  battery_level?: number;
  is_charging: boolean;
  is_plugged_in: boolean;
}

/** A running GUI application. */
export interface RunningApp {
  name: string;
  is_frontmost: boolean;
}

/** A recently changed file in Downloads/Desktop. */
export interface RecentFile {
  name: string;
  directory: string;
  age_secs: number;
}

/** A short speech transcript emitted by the audio capture layer. */
export interface TranscriptEntry {
  text: string;
  start_ms: number;
  end_ms: number;
  source: string;
  speaker?: string;
  confidence?: number;
}

/** The unified screen context returned by CEL. */
export interface ScreenContext {
  app: string;
  window: string;
  elements: ContextElement[];
  network_events?: ConnectionEvent[];
  /** Real HTTP events from CDP (method, URL, status, timing). */
  http_events?: HttpEvent[];
  timestamp_ms: number;
  screen_width?: number;
  screen_height?: number;
  /** Clipboard contents. */
  clipboard?: ClipboardState;
  /** All visible windows on screen. */
  window_list?: WindowState[];
  /** Audio output state. */
  audio?: AudioState;
  /** Battery/power state. */
  power?: PowerState;
  /** Running GUI applications. */
  running_apps?: RunningApp[];
  /** Recently changed files (Downloads/Desktop). */
  recent_files?: RecentFile[];
  /** Recent microphone/system-output transcript segments. */
  transcripts?: TranscriptEntry[];
}

/** Coarse spatial region for resilient element targeting. */
export interface BoundsRegion {
  quadrant: string;
  relative_x: number;
  relative_y: number;
}

/** A resilient, multi-signal reference to a UI element.
 * Unlike element IDs (ephemeral per snapshot), references survive across
 * context snapshots by combining multiple identifying signals. */
export interface ContextReference {
  element_type: string;
  label?: string;
  ancestor_path?: string[];
  bounds_region?: BoundsRegion;
  value_pattern?: string;
}

/** High-fidelity context for a single element — the "zoom in" view. */
export interface FocusedContext {
  element: ContextElement;
  subtree: ContextElement[];
  ancestor_path: string[];
}

/** Events emitted by the ContextWatchdog when screen state changes. */
export type CelEvent =
  | { type: "TreeChanged"; added: string[]; removed: string[] }
  | { type: "NetworkIdle" }
  | { type: "FocusChanged"; old: string | null; new: string | null }
  | { type: "ValueChanged"; element_id: string; new_value?: string }
  | { type: "WindowCreated"; title?: string }
  | { type: "MenuOpened" }
  | { type: "MenuClosed" }
  | { type: "SheetCreated" }
  | { type: "LayoutChanged" }
  | { type: "TitleChanged"; new_title?: string }
  | { type: "AppActivated"; app_name?: string }
  | { type: "AppDeactivated"; app_name?: string }
  | { type: "WindowMoved" }
  | { type: "WindowResized" }
  | { type: "WindowMinimized" }
  | { type: "WindowRestored" }
  | { type: "SelectionChanged" }
  | { type: "RowCountChanged" };

/** CDP page content extracted from Chromium-based apps. */
export interface PageContent {
  title: string;
  url: string;
  body_text: string;
  text_blocks: TextBlock[];
  interactive_elements: DomElement[];
  console_logs: ConsoleMessage[];
  network_requests: ResourceEntry[];
  load_time_ms?: number;
  dom_ready_ms?: number;
}

export interface ConsoleMessage {
  level: string;
  text: string;
}

export interface ResourceEntry {
  url: string;
  duration_ms: number;
  status?: number;
  size: number;
}

export interface TextBlock {
  block_type: string;
  text: string;
  level?: number;
}

export interface DomElement {
  tag: string;
  element_type: string;
  text: string;
  href?: string;
  input_type?: string;
  value?: string;
  placeholder?: string;
}

/** A network event captured by the network monitor. */
/** A raw TCP/UDP connection from lsof or /proc (honest OS-level data). */
export interface ConnectionEvent {
  timestamp_ms: number;
  protocol: string;
  local_addr: string;
  local_port: number;
  remote_addr: string;
  remote_port: number;
  state: string;
  service?: string;
  process_name?: string;
  pid?: number;
}

/** A real HTTP request/response from CDP or Performance API. */
export interface HttpEvent {
  timestamp_ms: number;
  method: string;
  url: string;
  status_code?: number;
  content_type?: string;
  duration_ms?: number;
  size_bytes?: number;
  source: string;
}

/** Backwards-compatible alias. */
export type NetworkEvent = ConnectionEvent;

/** A menu bar item — discoverable app command. */
export interface MenuBarItem {
  path: string;
  label: string;
  shortcut?: string;
  enabled: boolean;
}

/** A trackpad gesture event observed during recording. */
export type GestureEvent =
  | { gesture_type: "pinch_zoom"; scale: number }
  | { gesture_type: "swipe"; direction: string; finger_count: number }
  | { gesture_type: "rotate"; angle_degrees: number }
  | { gesture_type: "smart_zoom" }
  | { gesture_type: "momentum_scroll"; dx: number; dy: number };

/** A single step in a workflow. */
export interface WorkflowStep {
  id: string;
  description: string;
  action: WorkflowAction;
  /** Expected context after this step completes. */
  expected?: Partial<ScreenContext>;
  /** Minimum confidence required to proceed. */
  min_confidence?: number;
}

/** An action the agent can take. */
export type WorkflowAction =
  | { type: "click"; target: string; button?: "left" | "right" }
  | { type: "type"; target: string; text: string }
  | { type: "set_value"; target: string; value: string }
  | { type: "ax_action"; target: string; action: string }
  | { type: "drag"; fromX: number; fromY: number; toX: number; toY: number }
  | { type: "select"; from_x: number; from_y: number; to_x: number; to_y: number }
  | { type: "key"; key: string }
  | { type: "key_combo"; keys: string[] }
  | { type: "wait"; ms: number }
  | { type: "scroll"; dx: number; dy: number }
  | { type: "custom"; adapter: string; action: string; params: Record<string, unknown> };

/** A complete workflow definition. */
export interface Workflow {
  name: string;
  description: string;
  app: string;
  version: string;
  steps: WorkflowStep[];
  /** Context map from training phase. */
  context_map?: Record<string, unknown>;
  /** Runtime variables for {{placeholder}} substitution in type actions. */
  variables?: Record<string, string>;
  created_at: string;
  updated_at: string;
}

/** Workflow execution status. */
export type WorkflowStatus = "idle" | "running" | "paused" | "completed" | "failed" | "queued";

/** Priority levels for the workflow queue. */
export type Priority = "low" | "normal" | "high" | "critical";

// --- Planner types (from cel-planner) ---

/** How much context the planner requests for the NEXT step. */
export type ContextTier = "none" | "minimal" | "full";

/** A notebook write from the LLM — records data discovered during execution. */
export interface NotebookWriteEntry {
  key: string;
  value: string;
  /** Category: "data" (prices, names), "url" (links), "observation", "error" */
  category: string;
}

/** A single step planned by the LLM. */
export interface PlannedStep {
  /** Evaluation of the previous step's result. Forces self-reflection. */
  evaluation?: string;
  /** Working memory — 1-3 sentences tracking progress across steps. */
  memory?: string;
  /** Updated plan with status markers: [x]=done, [>]=current, [ ]=pending. */
  plan?: string[];
  reasoning: string;
  action: PlannedAction;
  /** Additional actions to execute after the primary (up to 4 more). Reduces LLM calls. */
  additional_actions?: PlannedAction[];
  expected_outcome: string;
  /** LLM's self-assessed confidence (0.0-1.0). Optional — defaults to 0.5 if omitted. */
  confidence: number;
  /** Context tier requested for the NEXT step. Defaults to "full". */
  context_tier?: ContextTier;
  /** Alternative actions the planner considered (for backtracking queue). */
  alternatives?: Array<{ description: string; action: PlannedAction; priority?: number }>;

  // ── Cognitive loop extensions ──────────────────────────────────────────

  /** Free-form internal monologue. Replaces evaluation+memory+reasoning when present. */
  thinking?: string;
  /** Proactive progress assessment: "on_track", "stalled", "wrong_approach", "milestone:<label>" */
  progress?: string;
  /** Data discovered during this step to persist in the notebook. */
  notebook_writes?: NotebookWriteEntry[];
  /** When true, the system skips context re-gathering on the next step. */
  batch_next?: boolean;
}

/** An action the planner wants to execute. */
export type PlannedAction =
  | { type: "click"; target_id: string }
  | { type: "type"; target_id?: string; text: string }
  | { type: "set_value"; target_id: string; value: string }
  | { type: "ax_action"; target_id: string; action: string }
  | { type: "activate_app"; app_name: string }
  | { type: "key"; key: string }
  | { type: "key_combo"; keys: string[] }
  | { type: "scroll"; dx: number; dy: number }
  | { type: "drag"; from_x: number; from_y: number; to_x: number; to_y: number }
  | { type: "select"; from_x: number; from_y: number; to_x: number; to_y: number }
  | { type: "wait"; ms: number }
  | { type: "custom"; adapter: string; action: string; params: Record<string, unknown> }
  | { type: "extract"; goal: string; data: string }
  | { type: "batch"; actions: PlannedAction[] }
  | { type: "act"; instruction: string }
  | { type: "done"; summary: string; evidence_ids?: string[] }
  | { type: "fail"; reason: string }
  | { type: "notebook_writes"; key?: string; value?: string; category?: string };

/** A recorded step from the planner's history. */
export interface PlannerStepRecord {
  step_index: number;
  action: PlannedAction;
  success: boolean;
  error?: string;
  /** Human-readable label of the target element (e.g. "Submit", "Username"). */
  element_label?: string;
}

// --- Perception Socket types ---

/** Configuration for a perception session. */
export interface PerceptionConfig {
  goal: string;
  /** CelEvent types to listen for. Defaults to common UI events. */
  eventFilter?: CelEvent["type"][];
  /** Minimum significance threshold for reporting diffs (0-1). Default 0.1. */
  relevanceThreshold?: number;
  /** Whether to generate LLM suggestions on pulse. Default true. */
  enableSuggestions?: boolean;
  /** Max settle wait time after feed (ms). Default 2000. */
  settleTimeoutMs?: number;
}

/** An action reported back to the perception socket via feed. */
export interface ActionEntry {
  action: string;
  target?: string;
  expected?: string;
  timestamp: number;
  landed: boolean;
}

/** An anomaly detected by the perception socket. */
export interface Anomaly {
  type: "dialog" | "error" | "app_switch" | "auth_prompt" | "crash" | "unexpected_navigation";
  title?: string;
  description: string;
  timestamp: number;
  /** Element IDs involved, if any. */
  elementIds?: string[];
}

/** Lightweight diff summary for perception results (avoids circular import with context-differ). */
export interface PerceptionDiff {
  addedCount: number;
  removedCount: number;
  changedCount: number;
  unchangedCount: number;
  /** Labels of newly added elements. */
  addedLabels: string[];
  /** Labels of changed elements. */
  changedLabels: string[];
}

/** Result returned by perception pulse. */
export interface PulseResult {
  /** Compact context summary: app, window, element counts, focused element. */
  contextSummary: {
    app: string;
    window: string;
    elementCount: number;
    actionableCount: number;
    focusedElement?: string;
  };
  /** What changed since last pulse. Null if nothing changed. */
  diff: PerceptionDiff | null;
  /** Detected anomalies since last pulse. */
  anomalies: Anomaly[];
  /** Current goal and action state. */
  goalState: {
    goal: string;
    currentAction: string | null;
    actionsCompleted: number;
    actionsFailed: number;
  };
  /** LLM-generated next-action suggestion. Null if suggestions disabled. */
  suggestion: string | null;
  /** Whether Claude should request a screenshot for this step. */
  screenshotNeeded: boolean;
  /** Raw events received since last pulse. */
  events: CelEvent[];
}

/** Result returned by perception feed. */
export interface FeedResult {
  /** Whether the action produced a visible state change. */
  actionLanded: boolean;
  /** Diff between pre-action and post-action context. */
  diff: PerceptionDiff | null;
  /** The element that has focus after the action. */
  nextFocusedElement?: string;
  /** Any anomalies detected during/after the action. */
  anomalies: Anomaly[];
}

/** A history checkpoint — summarizes completed work and resets working history. */
export interface CheckpointEntry {
  id: number;
  summary: string;
  timestamp: number;
  actionsBeforeCheckpoint: number;
}

/** Summary returned when stopping a perception session. */
export interface PerceptionSummary {
  totalActions: number;
  successfulActions: number;
  failedActions: number;
  totalAnomalies: number;
  totalPulses: number;
  durationMs: number;
  /** Observations collected during the session. */
  observations: string[];
}

// --- Cortex types (always-on perception) ---

/** Temporal patterns tracked by the cortex over time. */
export interface TemporalFlags {
  /** Loading spinner/skeleton detected. Null if not loading. */
  loading: { detected: boolean; durationMs: number; elementId?: string } | null;
  /** Error state persisting. Null if no error. */
  errorPersisting: { detected: boolean; durationMs: number; message?: string } | null;
  /** Timestamp when screen last became idle (no changes). Null if actively changing. */
  idleSince: number | null;
  /** Recent focus trail — element labels/IDs that had focus, most recent last. */
  focusTrail: string[];
  /** Number of consecutive perception cycles with no significant diff. */
  stagnantCycles: number;
}

/** Element stability classification — which elements are reliable targets. */
export interface ElementStability {
  /** Element IDs that haven't changed in 5+ cycles — reliable click targets. */
  stable: Set<string>;
  /** Element IDs that change every cycle — unreliable, avoid targeting. */
  volatile: Set<string>;
}

export type FreshnessState = "fresh" | "soft-stale" | "hard-stale";

export type StalenessCause = "time" | "event" | "confidence" | "verification";

export interface FreshnessAssessment {
  state: FreshnessState;
  causes: StalenessCause[];
  ageMs: number;
  confidence: number;
  lastUpdateMs: number;
  lastEventMs: number | null;
  lastSignificantEventMs: number | null;
}

export interface DiffSummary {
  addedCount: number;
  removedCount: number;
  changedCount: number;
  unchangedCount: number;
}

export type TaskPhase =
  | "idle"
  | "navigation"
  | "input"
  | "review"
  | "loading"
  | "blocked";

export interface SemanticInsight {
  currentActivity: string;
  recentTransition?: string | null;
  likelyBlocker?: string | null;
  suggestedNextStep?: string | null;
  taskPhase: TaskPhase;
}

export interface SourceSummary {
  accessibility: number;
  nativeApi: number;
  vision: number;
  merged: number;
  adapterBacked: number;
}

export interface ActionOutcome {
  action: string;
  route?: string;
  success: boolean;
  verified?: boolean;
  contradiction?: boolean;
  sideEffectSummary?: string;
  timestamp?: number;
}

/** The mental model — always-current understanding of the screen, maintained by the cortex. */
export interface MentalModel {
  /** Current screen context — always fresh from the latest perception cycle. */
  currentContext: ScreenContext;
  /** Currently focused element. */
  focusedElement: { id: string; label?: string } | null;
  /** Rolling window of recent diffs (last 10). */
  recentDiffs: PerceptionDiff[];
  /** Temporal patterns (loading, errors, focus trail, idle state). */
  temporal: TemporalFlags;
  /** Element stability classification (stable vs volatile). */
  stability: ElementStability;
  /** Proactively detected anomalies waiting to be consumed. */
  anomalyQueue: Anomaly[];
  /** Model confidence: 1.0 = just updated, decays toward 0 over time. */
  confidence: number;
  /** Whether vision (screenshot) is needed — streams couldn't resolve the situation. */
  visionNeeded: boolean;
  /** Milliseconds since last context update. */
  ageMs: number;
  /** Total number of perception cycles run. */
  cycleCount: number;
  /** Cortex uptime in ms. */
  uptimeMs: number;
  /** Current freshness classification used by the strategy router. */
  freshness?: FreshnessAssessment;
  /** Last diff summary, if a diff was observed recently. */
  lastDiffSummary?: DiffSummary | null;
  /** Which streams are currently wired into the default Cortex. */
  streamStatus?: StreamStatus;
  /** Names of the currently active adapters. */
  activeAdapters?: string[];
  /** Element ID → adapter source index for adapter-owned elements. */
  elementAdapterIndex?: Record<string, string>;
  /** Lightweight heuristic interpretation of the current fused device state. */
  semantic?: SemanticInsight;
  /** High-level element-source coverage for the current fused context. */
  sourceSummary?: SourceSummary;
}

/** High-level status for the streams currently fused by Cortex. */
export interface StreamStatus {
  accessibility: boolean;
  display: boolean;
  network: boolean;
  signals: boolean;
  vision: boolean;
  audioCapture: boolean;
}

/** Aggregated metrics for an entire goal run. */
export interface GoalMetrics {
  /** Total wall-clock time in milliseconds. */
  totalMs: number;
  /** Time spent on context extraction (getContext calls). */
  contextExtractionMs: number;
  /** Total LLM planning calls (text + vision). */
  llmCalls: number;
  /** How many of those used vision (screenshot). */
  visionCalls: number;
  /** Total errors encountered. */
  errorCount: number;
  /** How many times state changed mid-step (triggered re-plan). */
  stateChanges: number;
  /** How many loop warnings were issued. */
  loopWarnings: number;
  /** How many actions were successfully self-healed. */
  selfHealSuccesses?: number;
  /** How many self-heal attempts failed. */
  selfHealFailures?: number;
  /** How many heals involved a context shift (screen changed during failure). */
  healContextShifts?: number;
  /** How many actions were replayed from cache (zero LLM calls). */
  cacheHits?: number;
  /** Tier 2 replans: new-strategy invocations within the current milestone. */
  tier2Replans?: number;
  /** Tier 3 backtracks: restore-to-checkpoint + new strategy. */
  tier3Backtracks?: number;
  /** Tier 4 full goal re-assessments: re-decompose milestones, preserve notebook. */
  tier4Reassessments?: number;
  /** Strategy tracker hit the global cap (MAX_GLOBAL_REPLANS). */
  strategyExhaustedEvents?: number;
}
