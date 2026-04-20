/**
 * CEL Native Bindings Interface
 *
 * Type-safe wrapper around the napi-rs native module.
 * In production, these call into the Rust CEL core via cel-napi.
 * For development/testing, a mock implementation is used when the native module isn't available.
 */

import type {
  ScreenContext,
  ContextElement,
  ContextReference,
  FocusedContext,
  CelEvent,
  PageContent,
  ConnectionEvent,
  HttpEvent,
  Bounds,
  PlannedStep,
  PlannerStepRecord,
  MenuBarItem,
  GestureEvent,
} from "./types.js";
import type { ContextProvider } from "./interfaces/context-provider.js";
import type { InputController } from "./interfaces/input-controller.js";
import type { Planner } from "./interfaces/planner.js";
import type { KnowledgeStore } from "./interfaces/knowledge-store.js";
import type { BrowserBridge } from "./interfaces/browser-bridge.js";
import type { EventSource } from "./interfaces/event-source.js";

/**
 * Sanitize a ScreenContext before passing to Rust planner.
 * 1. If network_events contains HTTP data (method/url) instead of TCP
 *    ConnectionEvents (protocol/local_addr), move them to http_events.
 * 2. Optimize interactive element labels for the Rust prompt builder's
 *    40-char label truncation (prompt.rs Tree format). Puts the most
 *    discriminating info at the start of the label so it survives truncation.
 */
function sanitizeContextForRust(context: ScreenContext): ScreenContext {
  let ctx = context;

  // Fix network_events type mismatch
  if (
    ctx.network_events?.length &&
    (ctx.network_events[0] as any)?.method
  ) {
    ctx = {
      ...ctx,
      http_events: [
        ...(ctx.http_events ?? []),
        ...(ctx.network_events as any[]),
      ],
      network_events: [],
    };
  }

  // Optimize labels for Rust's 40-char truncation.
  // The Rust Tree-format prompt does: truncate(label, 40).
  // For interactive elements with long aria-labels, this cuts off
  // disambiguating info (e.g., "Remove Jamie Rodriguez (jamie.rodrigu"
  // loses the email and role). Move key differentiators to the front.
  if (ctx.elements?.length) {
    const optimized = ctx.elements.map((el) => {
      if (!el.label || el.label.length <= 40) return el;
      // Only optimize interactive elements — text/group labels can be truncated
      const isInteractive = el.element_type === "button" || el.element_type === "link" ||
        el.element_type === "input" || el.element_type === "checkbox" ||
        el.element_type === "combobox" || el.element_type === "menu_item";
      if (!isInteractive) return el;

      // If the label has parenthesized info like "Action (detail1, detail2)",
      // and the whole thing is >40 chars, compact the parenthesized part.
      // "Remove Jamie Rodriguez (jamie.rodriguez@acme.io, viewer)"
      //  → "Remove Jamie Rodriguez viewer jamie.rodriguez@acme.io"
      const parenMatch = el.label.match(/^(.+?)\s*\(([^)]+)\)\s*$/);
      if (parenMatch && el.label.length > 40) {
        const [, prefix, inner] = parenMatch;
        // Reverse the parts inside parens so the most unique info comes first
        const parts = inner.split(/,\s*/);
        const compacted = `${prefix} ${parts.reverse().join(" ")}`;
        return { ...el, label: compacted };
      }
      return el;
    });
    ctx = { ...ctx, elements: optimized };
  }

  return ctx;
}

/** CEL native module interface — matches the napi exports from cel-napi. */
export interface CelNative {
  celVersion(): string;
  getContext(): string;
  captureScreen(): Buffer;
  listMonitors(): string;
  listWindows(): string;
  mouseMove(x: number, y: number): void;
  click(x: number, y: number): void;
  rightClick(x: number, y: number): void;
  doubleClick(x: number, y: number): void;
  typeText(text: string): void;
  keyPress(key: string): void;
  keyCombo(keys: string[]): void;
  scroll(dx: number, dy: number): void;
  mousePosition(): number[];
  drag(fromX: number, fromY: number, toX: number, toY: number): void;
  axPerformAction(elementId: string, action: string): boolean;
  axSetValue(elementId: string, value: string): boolean;
  activateApp(appName: string): boolean;
  shellExec(command: string, args: string[]): string;
  axIsSettable(elementId: string): boolean;
  axElementAtPosition(x: number, y: number): string;
  axGetMenuBar(): string;
  axGetAllWindows(): string;
  tripleClick(x: number, y: number): void;
  keyDown(key: string): void;
  keyUp(key: string): void;
  paste(): void;
  selectAll(): void;
  mouseMoveSmooth(x: number, y: number, durationMs: number): void;
  gestureStart(): void;
  gestureDrain(): string;
  gestureStop(): void;
  queryKnowledge(dbPath: string, query: string): string;
  addKnowledge(dbPath: string, content: string, source: string): number;
  startRun(dbPath: string, workflowName: string, stepsTotal: number): number;
  finishRun(dbPath: string, runId: number, status: string): void;
  logStep(
    dbPath: string,
    runId: number,
    stepIndex: number,
    stepId: string,
    action: string,
    success: boolean,
    confidence: number,
    contextSnapshot: string | null,
    error: string | null,
  ): number;
  getRunHistory(dbPath: string, limit: number): string;
  getStepResults(dbPath: string, runId: number): string;
  // Memory: Working Memory
  getWorkingMemory(dbPath: string, workflowName: string): string;
  updateWorkingMemory(dbPath: string, workflowName: string, content: string): void;
  // Memory: Observations
  addObservation(dbPath: string, workflowName: string, content: string, priority: string, sourceRunIds: number[]): number;
  getObservations(dbPath: string, workflowName: string, limit: number): string;
  // Memory: Knowledge FTS5
  searchKnowledge(dbPath: string, query: string, workflowScope: string | null, limit: number): string;
  addScopedKnowledge(dbPath: string, content: string, source: string, workflowScope: string | null, tags: string | null): number;
  // Eviction / TTL
  runEviction(dbPath: string, runRetentionDays: number, knowledgeRetentionDays: number): string;
  // Quick context (app/window only, no tree walk)
  getQuickContext(): string;
  // Blind planner (no screen context, uses device baseline)
  planStepBlind(
    goal: string,
    historyJson: string,
    deviceBaselineJson: string,
    maxSteps?: number,
    loopWarning?: string,
  ): Promise<string>;
  // Context from external elements (browser adapter → Rust pipeline)
  buildContextFromElements(
    elementsJson: string,
    networkEventsJson: string,
    appName: string,
    windowTitle: string,
  ): string;
  // Context References
  makeReference(elementJson: string, screenWidth: number, screenHeight: number): string;
  resolveReference(contextJson: string, referenceJson: string): string;
  // Focused Context
  getContextFocused(elementId: string): string;
  // CDP
  cdpSetupInstall(): string;
  cdpSetupUninstall(): string;
  cdpIsSetup(): boolean;
  cdpDiscoverTargets(): string;
  cdpGetPageContent(): Promise<string>;
  cdpGetCookies(): Promise<string>;
  cdpGetLocalStorage(key: string): Promise<string>;
  cdpGetNetworkRequests(limit?: number): Promise<string>;
  cdpNavigate(url: string): Promise<void>;
  cdpEvaluate(expression: string): Promise<string>;
  // Watchdog
  startWatchdog(): void;
  pollEvents(): string;
  stopWatchdog(): void;
  // Cortex (Rust perception engine)
  bootCortex(): void;
  readCortexModel(): string;
  notifyCortexAction(action: string): void;
  reportCortexActionFailure(): void;
  reportCortexActionSuccess(): void;
  consumeCortexAnomalies(): string;
  isCortexRunning(): boolean;
  stopCortex(): void;
  // Cortex liveness (Phase 1)
  cortexTickCount(): number;
  cortexStalledTicks(): number;
  cortexLastTickAgeMs(): number | null;
  cortexRefreshNow(timeoutMs?: number): Promise<number>;
  runGoalRust(configJson: string): Promise<string>;
  // Planner
  planStep(
    goal: string,
    contextJson: string,
    historyJson: string,
    provider?: string,
    apiKey?: string,
    model?: string,
    endpoint?: string,
    maxTokens?: number,
    maxSteps?: number,
    loopWarning?: string,
    deviceBaselineJson?: string,
  ): Promise<string>;
  // Prompt builder (returns { system, user } JSON without calling LLM)
  buildPlanPrompt(
    goal: string,
    contextJson: string,
    historyJson: string,
    maxSteps?: number,
    loopWarning?: string,
    provider?: string,
    model?: string,
  ): string;
  // Text-only LLM call
  llmComplete(
    systemPrompt: string,
    userPrompt: string,
    provider?: string,
    apiKey?: string,
    model?: string,
    endpoint?: string,
    maxTokens?: number,
  ): Promise<string>;
  // Role-aware LLM call (validator, orchestrator, localizer, etc.)
  llmCompleteWithRole(
    systemPrompt: string,
    userPrompt: string,
    role: string,
    maxTokens?: number,
  ): Promise<string>;
  // Vision LLM call
  llmCompleteWithImage(
    systemPrompt: string,
    imageBase64: string,
    userPrompt: string,
    provider?: string,
    apiKey?: string,
    model?: string,
    endpoint?: string,
    maxTokens?: number,
  ): Promise<string>;
  // Multi-turn LLM call (conversation thread)
  llmCompleteWithMessages(
    messagesJson: string,
    maxTokens?: number,
  ): Promise<string>;
  // Goal decomposition
  decomposeGoal(
    goal: string,
    contextJson: string,
    totalStepBudget: number,
    historyAdvice?: string,
  ): Promise<string>;
}

/** Monitor info from CEL display layer. */
export interface MonitorInfo {
  id: number;
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
  is_primary: boolean;
}

/** Window info from CEL display layer. */
export interface WindowInfo {
  id: number;
  title: string;
  app_name: string;
  x: number;
  y: number;
  width: number;
  height: number;
  is_minimized: boolean;
}

/** Knowledge fact from CEL Store. */
export interface KnowledgeFact {
  id: number;
  content: string;
  source: string;
  created_at: string;
}

/** Run history record from CEL Store. */
export interface RunRecord {
  id: number;
  workflow_name: string;
  started_at: string;
  finished_at: string | null;
  status: string;
  steps_completed: number;
  steps_total: number;
  interventions: number;
}

/** Observation from CEL Store. */
export interface ObservationRecord {
  id: number;
  workflow_name: string;
  content: string;
  priority: "high" | "medium" | "low";
  source_run_ids: string;
  observed_at: string;
  referenced_at: string | null;
  superseded_by: number | null;
  created_at: string;
}

/** Scored knowledge from FTS5 search. */
export interface ScoredKnowledgeRecord {
  id: number;
  content: string;
  source: string;
  workflow_scope: string | null;
  score: number;
  created_at: string;
}

/** Eviction result from TTL cleanup. */
export interface EvictionResult {
  superseded_observations: number;
  old_runs: number;
  old_knowledge: number;
}

/** Step result record from CEL Store. */
export interface StepRecord {
  id: number;
  run_id: number;
  step_index: number;
  step_id: string;
  action: string;
  success: boolean;
  confidence: number;
  context_snapshot: string | null;
  error: string | null;
  executed_at: string;
}

let _native: CelNative | null = null;

/** Load the native CEL module. Returns null if not available. */
function loadNative(): CelNative | null {
  if (_native) return _native;
  try {
    // Try to load the napi-rs compiled module
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    _native = require("@cellar/cel-napi") as CelNative;
    return _native;
  } catch {
    return null;
  }
}

/**
 * Resolve numbered target_id indices in a PlannedStep back to real element IDs.
 * The Rust planner uses numbered indices [1], [2] in prompts; after parsing the
 * LLM response, this maps "1" → real element ID like "a11y:19".
 */
export function resolveStepIndices(step: PlannedStep, indexMap: string[]): void {
  const action = step.action as Record<string, unknown>;
  if (action.target_id && typeof action.target_id === "string") {
    const idx = parseInt(action.target_id, 10);
    if (!isNaN(idx) && idx >= 1 && idx <= indexMap.length) {
      action.target_id = indexMap[idx - 1];
    }
  }
  if (Array.isArray(action.evidence_ids)) {
    action.evidence_ids = (action.evidence_ids as string[]).map((eid: string) => {
      const idx = parseInt(eid, 10);
      if (!isNaN(idx) && idx >= 1 && idx <= indexMap.length) {
        return indexMap[idx - 1];
      }
      return eid;
    });
  }
}

/**
 * High-level CEL API — wraps native bindings with proper TypeScript types.
 *
 * Implements all 6 composable interfaces. Consumers should depend on
 * the narrowest interface they need (e.g., `Planner`, `InputController`)
 * rather than the full `Cel` class. This enables type-safe mocking
 * and dependency injection.
 *
 * @see {@link import("./interfaces/index.js")} for the interface definitions.
 */
export class Cel implements
  ContextProvider,
  InputController,
  Planner,
  KnowledgeStore,
  BrowserBridge,
  EventSource {
  private native: CelNative | null;
  private dbPath: string;

  constructor(dbPath = "~/.cellar/cel-store.db") {
    this.native = loadNative();
    this.dbPath = dbPath.replace("~", process.env.HOME ?? "");
  }

  /** Whether the native module is available. */
  get isNativeAvailable(): boolean {
    return this.native !== null;
  }

  /** Get CEL version. */
  version(): string {
    return this.native?.celVersion() ?? "0.1.0-mock";
  }

  // --- Display ---

  /** Get the unified screen context. */
  getContext(): ScreenContext {
    if (!this.native) {
      return { app: "", window: "", elements: [], timestamp_ms: Date.now() };
    }
    return JSON.parse(this.native.getContext());
  }

  /** Get minimal context: app name + window title only. No tree walk (~50ms). */
  getQuickContext(): ScreenContext {
    if (!this.native) {
      return { app: "", window: "", elements: [], timestamp_ms: Date.now() };
    }
    return JSON.parse(this.native.getQuickContext());
  }

  /** Plan a step WITHOUT screen context (blind mode).
   *  Uses device baseline instead of element table. */
  async planStepBlind(
    goal: string,
    history: PlannerStepRecord[] = [],
    deviceBaselineJson: string,
    options?: { maxSteps?: number; loopWarning?: string },
  ): Promise<PlannedStep> {
    if (!this.native) {
      throw new Error("Native module not available — planStepBlind requires cel-napi");
    }
    const resultJson = await this.native.planStepBlind(
      goal,
      JSON.stringify(history),
      deviceBaselineJson,
      options?.maxSteps,
      options?.loopWarning,
    );
    return JSON.parse(resultJson);
  }

  /** Capture a screenshot as PNG buffer. */
  captureScreen(): Buffer {
    if (!this.native) {
      throw new Error("Native module not available");
    }
    return this.native.captureScreen();
  }

  /** List available monitors. */
  listMonitors(): MonitorInfo[] {
    if (!this.native) return [];
    return JSON.parse(this.native.listMonitors());
  }

  /** List visible windows. */
  listWindows(): WindowInfo[] {
    if (!this.native) return [];
    return JSON.parse(this.native.listWindows());
  }

  // --- Input ---

  /** Move mouse to absolute coordinates. */
  mouseMove(x: number, y: number): void {
    this.native?.mouseMove(x, y);
  }

  /** Left-click at coordinates. */
  click(x: number, y: number): void {
    this.native?.click(x, y);
  }

  /** Right-click at coordinates. */
  rightClick(x: number, y: number): void {
    this.native?.rightClick(x, y);
  }

  /** Double-click at coordinates. */
  doubleClick(x: number, y: number): void {
    this.native?.doubleClick(x, y);
  }

  /** Type text using fast unicode input. */
  typeText(text: string): void {
    this.native?.typeText(text);
  }

  /** Press a single key. */
  keyPress(key: string): void {
    this.native?.keyPress(key);
  }

  /** Press a key combination. */
  keyCombo(keys: string[]): void {
    this.native?.keyCombo(keys);
  }

  /** Scroll at current position. */
  scroll(dx: number, dy: number): void {
    this.native?.scroll(dx, dy);
  }

  /** Get current mouse cursor position as [x, y]. */
  mousePosition(): [number, number] {
    const pos = this.native?.mousePosition() ?? [0, 0];
    return [pos[0], pos[1]];
  }

  /** Drag from one point to another. */
  drag(fromX: number, fromY: number, toX: number, toY: number): void {
    this.native?.drag(fromX, fromY, toX, toY);
  }

  // --- Accessibility Actions ---

  /** Execute an action on an element via the accessibility API (more reliable than click). */
  axPerformAction(elementId: string, action: string): boolean {
    return this.native?.axPerformAction(elementId, action) ?? false;
  }

  /** Set a value directly on an element (bypasses mouse/keyboard for form filling). */
  axSetValue(elementId: string, value: string): boolean {
    return this.native?.axSetValue(elementId, value) ?? false;
  }

  /** Check if an element's value can be set directly. */
  axIsSettable(elementId: string): boolean {
    return this.native?.axIsSettable(elementId) ?? false;
  }

  /** Activate (bring to front) a macOS application by name via `open -a`. */
  activateApp(appName: string): boolean {
    return this.native?.activateApp(appName) ?? false;
  }

  /** Execute a safe shell command (allowlisted: open, osascript, defaults, etc.). */
  shellExec(command: string, args: string[]): string {
    return this.native?.shellExec(command, args) ?? '{"success":false,"stderr":"native not loaded"}';
  }

  /** Triple-click at coordinates (selects full line/paragraph). */
  tripleClick(x: number, y: number): void {
    this.native?.tripleClick(x, y);
  }

  /** Press a key down without releasing. Pair with keyUp() for independent modifier control. */
  keyDown(key: string): void {
    this.native?.keyDown(key);
  }

  /** Release a key that was previously pressed with keyDown(). */
  keyUp(key: string): void {
    this.native?.keyUp(key);
  }

  /** Paste from clipboard (Cmd+V on macOS, Ctrl+V on others). */
  paste(): void {
    this.native?.paste();
  }

  /** Select all text in the focused element (Cmd+A on macOS, Ctrl+A on others). */
  selectAll(): void {
    this.native?.selectAll();
  }

  /** Move mouse smoothly with human-like interpolation. */
  mouseMoveSmooth(x: number, y: number, durationMs: number): void {
    this.native?.mouseMoveSmooth(x, y, durationMs);
  }

  /** Get the accessibility element at screen coordinates (hit testing). */
  axElementAtPosition(x: number, y: number): ContextElement | null {
    if (!this.native) return null;
    const json = this.native.axElementAtPosition(x, y);
    if (json === "null") return null;
    return JSON.parse(json);
  }

  /** Get the menu bar structure of the focused app (command palette). */
  axGetMenuBar(): import("./types.js").MenuBarItem[] {
    if (!this.native) return [];
    return JSON.parse(this.native.axGetMenuBar());
  }

  /** Get ALL windows of the focused app (not just focused one). */
  axGetAllWindows(): ContextElement[] {
    if (!this.native) return [];
    return JSON.parse(this.native.axGetAllWindows());
  }

  // --- Gesture Observation ---

  /** Start capturing trackpad gestures for workflow recording. */
  gestureStart(): void {
    this.native?.gestureStart();
  }

  /** Drain accumulated gesture events since last call. */
  gestureDrain(): import("./types.js").GestureEvent[] {
    if (!this.native) return [];
    return JSON.parse(this.native.gestureDrain());
  }

  /** Stop capturing trackpad gestures. */
  gestureStop(): void {
    this.native?.gestureStop();
  }

  // --- CDP (extended) ---

  /** Get all cookies from the focused browser tab. */
  async cdpGetCookies(): Promise<unknown[]> {
    if (!this.native) return [];
    try {
      return JSON.parse(await this.native.cdpGetCookies());
    } catch {
      return [];
    }
  }

  /** Get a localStorage value from the focused browser tab. */
  async cdpGetLocalStorage(key: string): Promise<string | null> {
    if (!this.native) return null;
    try {
      const result = await this.native.cdpGetLocalStorage(key);
      return result === "null" ? null : result;
    } catch {
      return null;
    }
  }

  /** Get recent HTTP requests from the focused browser tab (real data from Performance API). */
  async cdpGetNetworkRequests(limit = 20): Promise<import("./types.js").HttpEvent[]> {
    if (!this.native) return [];
    try {
      return JSON.parse(await this.native.cdpGetNetworkRequests(limit));
    } catch {
      return [];
    }
  }

  /** Navigate the focused browser tab to a URL. */
  async cdpNavigate(url: string): Promise<void> {
    if (!this.native) throw new Error("Native module not available");
    await this.native.cdpNavigate(url);
  }

  // --- Knowledge ---

  /** Query the knowledge store. */
  queryKnowledge(query: string): KnowledgeFact[] {
    if (!this.native) return [];
    return JSON.parse(this.native.queryKnowledge(this.dbPath, query));
  }

  /** Add a fact to the knowledge store. */
  addKnowledge(content: string, source: string): number {
    if (!this.native) return -1;
    return this.native.addKnowledge(this.dbPath, content, source);
  }

  // --- Run Tracking ---

  /** Start tracking a workflow run. */
  startRun(workflowName: string, stepsTotal: number): number {
    if (!this.native) return -1;
    return this.native.startRun(this.dbPath, workflowName, stepsTotal);
  }

  /** Finish a tracked workflow run. */
  finishRun(runId: number, status: "completed" | "failed"): void {
    this.native?.finishRun(this.dbPath, runId, status);
  }

  /** Log a step result during a workflow run. */
  logStep(
    runId: number,
    stepIndex: number,
    stepId: string,
    action: string,
    success: boolean,
    confidence: number,
    contextSnapshot?: string,
    error?: string,
  ): number {
    if (!this.native) return -1;
    return this.native.logStep(
      this.dbPath,
      runId,
      stepIndex,
      stepId,
      action,
      success,
      confidence,
      contextSnapshot ?? null,
      error ?? null,
    );
  }

  /** Get run history, most recent first. */
  getRunHistory(limit = 20): RunRecord[] {
    if (!this.native) return [];
    return JSON.parse(this.native.getRunHistory(this.dbPath, limit));
  }

  /** Get step results for a specific run. */
  getStepResults(runId: number): StepRecord[] {
    if (!this.native) return [];
    return JSON.parse(this.native.getStepResults(this.dbPath, runId));
  }

  // --- Working Memory ---

  /** Get working memory content for a workflow. */
  getWorkingMemory(workflowName: string): string {
    if (!this.native) return "";
    const wm = JSON.parse(this.native.getWorkingMemory(this.dbPath, workflowName));
    return wm.content ?? "";
  }

  /** Update working memory for a workflow. */
  updateWorkingMemory(workflowName: string, content: string): void {
    this.native?.updateWorkingMemory(this.dbPath, workflowName, content);
  }

  // --- Observations ---

  /** Add an observation from past runs. */
  addObservation(
    workflowName: string,
    content: string,
    priority: "high" | "medium" | "low",
    sourceRunIds: number[],
  ): number {
    if (!this.native) return -1;
    return this.native.addObservation(this.dbPath, workflowName, content, priority, sourceRunIds);
  }

  /** Get active observations for a workflow. */
  getObservations(workflowName: string, limit = 50): ObservationRecord[] {
    if (!this.native) return [];
    return JSON.parse(this.native.getObservations(this.dbPath, workflowName, limit));
  }

  // --- Knowledge FTS5 ---

  /** Search knowledge using FTS5 full-text search. */
  searchKnowledge(query: string, workflowScope?: string, limit = 5): ScoredKnowledgeRecord[] {
    if (!this.native) return [];
    return JSON.parse(
      this.native.searchKnowledge(this.dbPath, query, workflowScope ?? null, limit),
    );
  }

  // --- Eviction / TTL ---

  /** Run eviction policies. Returns counts of deleted rows. */
  runEviction(runRetentionDays = 90, knowledgeRetentionDays = 365): EvictionResult {
    if (!this.native) return { superseded_observations: 0, old_runs: 0, old_knowledge: 0 };
    return JSON.parse(this.native.runEviction(this.dbPath, runRetentionDays, knowledgeRetentionDays));
  }

  // --- External Context (browser adapter → Rust pipeline) ---

  /**
   * Build a ScreenContext from externally-provided elements.
   * Routes through the Rust CEL core for unified confidence scoring,
   * element type normalization, noise filtering, and sorting.
   *
   * Used by the browser adapter so that Rust is the single source of truth
   * for context assembly — no duplicated scoring/mapping in TypeScript.
   *
   * Elements should have `element_type` set to the raw ARIA role string
   * (e.g., "textbox", "combobox"). Rust normalizes to CEL types (e.g., "input").
   */
  buildContextFromElements(
    elements: ContextElement[],
    networkEvents: HttpEvent[],
    appName: string,
    windowTitle: string,
  ): ScreenContext {
    if (!this.native) {
      // Fallback: build manually without Rust scoring
      return {
        app: appName,
        window: windowTitle,
        elements,
        http_events: networkEvents,
        timestamp_ms: Date.now(),
      };
    }
    // Sanitize elements: ensure string fields are actually strings.
    // Some websites produce elements with object values in fields Rust expects as strings
    // (e.g., aria attributes being objects, value being a complex type).
    const sanitizedElements = elements.map((el) => {
      const copy = { ...el };
      const stringFields = ["id", "element_type", "label", "value", "description", "parent_id"] as const;
      for (const f of stringFields) {
        if (copy[f] != null && typeof copy[f] !== "string") {
          (copy as any)[f] = String(copy[f]);
        }
      }
      return copy;
    });

    return JSON.parse(
      this.native.buildContextFromElements(
        JSON.stringify(sanitizedElements),
        JSON.stringify(networkEvents ?? []),
        appName,
        windowTitle,
      ),
    );
  }

  // --- Planner ---

  /** Plan a single step given a goal, current context, and step history. */
  async planStep(
    goal: string,
    context: ScreenContext,
    history: PlannerStepRecord[] = [],
    options?: {
      maxSteps?: number;
      loopWarning?: string;
      deviceBaselineJson?: string;
      model?: string;
    },
  ): Promise<PlannedStep> {
    if (!this.native) {
      throw new Error("Native module not available — planner requires cel-napi");
    }
    const contextJson = JSON.stringify(sanitizeContextForRust(context));
    const historyJson = JSON.stringify(history);
    const resultJson = await this.native.planStep(
      goal,
      contextJson,
      historyJson,
      undefined, // provider — use env default
      undefined, // apiKey
      options?.model, // model override for escalation
      undefined, // endpoint
      8192,      // maxTokens — structured output needs room for evaluation+memory+plan+action
      options?.maxSteps,
      options?.loopWarning,
      options?.deviceBaselineJson,
    );
    return JSON.parse(resultJson);
  }

  /**
   * Build the system + user prompts for planning WITHOUT calling the LLM.
   * Use this to get the exact prompts, then call planStepWithVision() separately.
   */
  buildPlanPrompt(
    goal: string,
    context: ScreenContext,
    history: PlannerStepRecord[] = [],
    options?: { maxSteps?: number; loopWarning?: string },
  ): { system: string; user: string; index_map: string[] } {
    if (!this.native) {
      throw new Error("Native module not available — buildPlanPrompt requires cel-napi");
    }
    const result = this.native.buildPlanPrompt(
      goal,
      JSON.stringify(sanitizeContextForRust(context)),
      JSON.stringify(history),
      options?.maxSteps,
      options?.loopWarning,
    );
    return JSON.parse(result);
  }

  /**
   * Plan a step with vision: sends structured context + screenshot to the LLM.
   * Used as a fallback when DOM is sparse or after consecutive failures.
   * Produces the exact same PlannedStep output as planStep().
   */
  async planStepWithVision(
    goal: string,
    context: ScreenContext,
    screenshotBase64: string,
    history: PlannerStepRecord[] = [],
    options?: { maxSteps?: number; loopWarning?: string },
  ): Promise<PlannedStep> {
    if (!this.native) {
      throw new Error("Native module not available — vision requires cel-napi");
    }
    // Get the same prompts planStep() would use (includes index_map)
    const prompts = this.buildPlanPrompt(goal, context, history, options);
    // Add vision note to user prompt
    const userWithVision = prompts.user +
      "\n\n(A screenshot of the current screen is attached. Use it to identify elements " +
      "the structured context may have missed, especially overlays, cookie banners, or modals.)";
    // Call LLM with image
    const raw = await this.native.llmCompleteWithImage(
      prompts.system,
      screenshotBase64,
      userWithVision,
    );
    const cleaned = raw.replace(/```json?\n?/g, "").replace(/```/g, "").trim();
    const step = JSON.parse(cleaned) as PlannedStep;
    // Resolve numbered indices back to real element IDs
    resolveStepIndices(step, prompts.index_map);
    return step;
  }

  // --- Context References ---

  /** Create a resilient reference from an element.
   * The reference can be used to re-find the same element in future context snapshots. */
  makeReference(element: ContextElement, screenWidth = 1920, screenHeight = 1080): ContextReference {
    if (!this.native) {
      return { element_type: element.element_type, label: element.label };
    }
    return JSON.parse(
      this.native.makeReference(JSON.stringify(element), screenWidth, screenHeight),
    );
  }

  /** Resolve a reference against a screen context snapshot.
   * Returns the best-matching element, or null if no match. */
  resolveReference(context: ScreenContext, ref_: ContextReference): ContextElement | null {
    if (!this.native) return null;
    const result = this.native.resolveReference(
      JSON.stringify(context),
      JSON.stringify(ref_),
    );
    const parsed = JSON.parse(result);
    return parsed === null ? null : parsed;
  }

  // --- Focused Context ---

  /** Get high-fidelity context for a single element by ID. */
  getContextFocused(elementId: string): FocusedContext | null {
    if (!this.native) return null;
    const result = this.native.getContextFocused(elementId);
    const parsed = JSON.parse(result);
    return parsed === null ? null : parsed;
  }

  // --- Watchdog ---

  /** Start the context watchdog for change detection. */
  startWatchdog(): void {
    this.native?.startWatchdog();
  }

  /** Poll for watchdog events. Returns events that occurred since last poll. */
  pollEvents(): CelEvent[] {
    if (!this.native) return [];
    return JSON.parse(this.native.pollEvents());
  }

  /** Stop and reset the watchdog. */
  stopWatchdog(): void {
    this.native?.stopWatchdog();
  }

  // --- Cortex (Rust perception engine) ---

  /** Boot the Rust Cortex — starts the 200ms perception tick loop. */
  bootCortex(): void {
    this.native?.bootCortex();
  }

  /** Read the current mental model as parsed JSON. Instant — shared memory, no observation. */
  readCortexModel(): unknown {
    if (!this.native) return null;
    return JSON.parse(this.native.readCortexModel());
  }

  /** Notify the Cortex that an action was taken. */
  notifyCortexAction(action: string): void {
    this.native?.notifyCortexAction(action);
  }

  /** Report a consecutive action failure to the Cortex. */
  reportCortexActionFailure(): void {
    this.native?.reportCortexActionFailure();
  }

  /** Report a successful action to the Cortex. */
  reportCortexActionSuccess(): void {
    this.native?.reportCortexActionSuccess();
  }

  /** Consume anomalies from the Cortex. Returns parsed anomaly array. */
  consumeCortexAnomalies(): unknown[] {
    if (!this.native) return [];
    return JSON.parse(this.native.consumeCortexAnomalies());
  }

  /** Check if the Rust Cortex is running. */
  isCortexRunning(): boolean {
    return this.native?.isCortexRunning() ?? false;
  }

  /** Stop the Rust Cortex. */
  stopCortex(): void {
    this.native?.stopCortex();
  }

  // --- Cortex liveness (Phase 1) ---

  /**
   * Total successful ticks since boot. Returns 0 if the Cortex isn't
   * running or the native module isn't available.
   */
  cortexTickCount(): number {
    return this.native?.cortexTickCount() ?? 0;
  }

  /**
   * Count of `refresh_now` calls that timed out waiting for a tick. A
   * rising number means the perception tick loop is stalling.
   */
  cortexStalledTicks(): number {
    return this.native?.cortexStalledTicks() ?? 0;
  }

  /**
   * Milliseconds since the last successful tick. `null` if no tick has
   * fired yet (Cortex not booted, or still starting up). A number that
   * keeps growing indicates a stalled tick loop.
   */
  cortexLastTickAgeMs(): number | null {
    return this.native?.cortexLastTickAgeMs() ?? null;
  }

  /**
   * Force an out-of-band tick and resolve when it completes. Returns the
   * tick count after the triggered tick. Rejects on timeout (default 500ms).
   */
  async cortexRefreshNow(timeoutMs?: number): Promise<number> {
    if (!this.native) {
      throw new Error("Native CEL module not available");
    }
    return this.native.cortexRefreshNow(timeoutMs);
  }

  /** Run a goal through the Rust goal-runner. */
  async runGoalRust(config: Record<string, unknown>): Promise<unknown> {
    if (!this.native) {
      throw new Error("Native CEL module not available");
    }
    return JSON.parse(await this.native.runGoalRust(JSON.stringify(config)));
  }

  // --- CDP ---

  /** Get page content from CDP if available. Returns null if no CDP target found. */
  async getCdpPageContent(): Promise<PageContent | null> {
    if (!this.native) return null;
    try {
      const result = await this.native.cdpGetPageContent();
      if (result === "null") return null;
      return JSON.parse(result);
    } catch {
      return null;
    }
  }

  /** Discover CDP targets on this machine. */
  discoverCdpTargets(): Array<{ app_name: string; pid: number; port: number; ws_url: string }> {
    if (!this.native) return [];
    try {
      return JSON.parse(this.native.cdpDiscoverTargets());
    } catch {
      return [];
    }
  }

  /** Check if CDP setup (LaunchAgent) is installed. */
  isCdpSetup(): boolean {
    return this.native?.cdpIsSetup() ?? false;
  }

  /** Execute JavaScript in the focused browser tab via CDP. */
  async cdpEvaluate(expression: string): Promise<unknown> {
    if (!this.native) {
      throw new Error("Native module not available — cdpEvaluate requires cel-napi");
    }
    const result = await this.native.cdpEvaluate(expression);
    return JSON.parse(result);
  }

  /** Add a scoped knowledge fact. */
  addScopedKnowledge(
    content: string,
    source: string,
    workflowScope?: string,
    tags?: string,
  ): number {
    if (!this.native) return -1;
    return this.native.addScopedKnowledge(
      this.dbPath, content, source, workflowScope ?? null, tags ?? null,
    );
  }

  // --- LLM ---

  /** Send a text-only LLM completion. Uses env vars for provider config if not specified. */
  async llmComplete(systemPrompt: string, userPrompt: string, maxTokens?: number): Promise<string> {
    if (!this.native) {
      throw new Error("Native module not available — llmComplete requires cel-napi");
    }
    return this.native.llmComplete(
      systemPrompt,
      userPrompt,
      undefined, // provider — use env default
      undefined, // apiKey
      undefined, // model
      undefined, // endpoint
      maxTokens,
    );
  }

  /**
   * Send a role-aware LLM completion.
   * The role determines which env vars are used (e.g., CEL_LLM_VALIDATOR_PROVIDER).
   * Valid roles: "planner", "observer", "vision", "general", "validator", "localizer", "orchestrator".
   */
  async llmCompleteWithRole(
    systemPrompt: string,
    userPrompt: string,
    role: string,
    maxTokens?: number,
  ): Promise<string> {
    if (!this.native) {
      throw new Error("Native module not available — llmCompleteWithRole requires cel-napi");
    }
    return this.native.llmCompleteWithRole(systemPrompt, userPrompt, role, maxTokens);
  }

  /** Send an LLM completion with an attached image. Uses env vars for provider config if not specified. */
  async llmCompleteWithImage(
    systemPrompt: string,
    imageBase64: string,
    userPrompt: string,
    maxTokens?: number,
  ): Promise<string> {
    if (!this.native) {
      throw new Error("Native module not available — llmCompleteWithImage requires cel-napi");
    }
    return this.native.llmCompleteWithImage(
      systemPrompt,
      imageBase64,
      userPrompt,
      undefined, // provider
      undefined, // apiKey
      undefined, // model
      undefined, // endpoint
      maxTokens,
    );
  }

  /**
   * Decompose a complex goal into advisory milestones.
   * Returns feasibility assessment and milestone list for multi-step tracking.
   */
  async decomposeGoal(
    goal: string,
    context: ScreenContext,
    totalStepBudget: number,
    historyAdvice?: string,
  ): Promise<{ feasible: boolean; feasibility_confidence: number; feasibility_reasoning: string; missing_prerequisites: string[]; milestones: Array<{ label: string; description: string; step_budget: number }>; reasoning: string }> {
    if (!this.native) {
      throw new Error("Native module not available — decomposeGoal requires cel-napi");
    }
    const contextJson = JSON.stringify(sanitizeContextForRust(context));
    const resultJson = await this.native.decomposeGoal(goal, contextJson, totalStepBudget, historyAdvice);
    return JSON.parse(resultJson);
  }

  /**
   * Send a multi-turn LLM completion with a conversation thread.
   * Messages are role/content pairs (system, user, assistant).
   * Uses the Planner role for provider resolution.
   */
  async llmCompleteWithMessages(
    messages: Array<{ role: string; content: string }>,
    maxTokens?: number,
  ): Promise<string> {
    if (!this.native) {
      throw new Error("Native module not available — llmCompleteWithMessages requires cel-napi");
    }
    return this.native.llmCompleteWithMessages(
      JSON.stringify(messages),
      maxTokens,
    );
  }
}
