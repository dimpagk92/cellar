/**
 * Cortex — Always-on perception engine for CEL.
 *
 * The human brain doesn't request visual input. The retina is always firing.
 * The visual cortex is always processing. When you reach for a cup, you
 * already know where it is. Perception is decoupled from action.
 *
 * The Cortex maintains a continuously-updated mental model via a background
 * loop that processes event streams from the Rust watchdog. Consumers
 * (goal-runner, MCP tools) read FROM the model — they never trigger new
 * observations. The model is always fresh.
 *
 * Perception hierarchy (streams first, vision last):
 *   1. EVENT STREAMS — always on, free, structured (pollEvents)
 *   2. ACCESSIBILITY TREE — on significant events only (getContext)
 *   3. QUICK CONTEXT — cheap app/window check between events (getQuickContext)
 *   4. VISION — expensive fallback, only when streams can't resolve (flag only)
 */

import type { ContextProvider } from "./interfaces/context-provider.js";
import type { EventSource } from "./interfaces/event-source.js";
import type {
  ScreenContext,
  CelEvent,
  MentalModel,
  TemporalFlags,
  ElementStability,
  PerceptionDiff,
  Anomaly,
  FreshnessAssessment,
  DiffSummary,
  ActionOutcome,
} from "./types.js";
import { diffContexts, isDiffSignificant, type ContextDiff } from "./context-differ.js";
import { isSkeletonScreen, skeletonWaitMs, hasActiveSpinner } from "./skeleton-detector.js";
import { contextFingerprint } from "./goal-runner/helpers.js";
import { enrichMentalModel } from "./cortex-insight.js";

// ─── Constants ──────────────────────────────────────────────────────────────

/** Cortex tick interval in ms. */
const TICK_INTERVAL_MS = 200;

/** How many cycles an element must survive unchanged to be "stable". */
const STABLE_THRESHOLD = 5;

/** Max recent diffs to keep. */
const MAX_RECENT_DIFFS = 10;

/** Max focus trail entries. */
const MAX_FOCUS_TRAIL = 20;

/** Max anomalies in queue before oldest are dropped. */
const MAX_ANOMALY_QUEUE = 50;

/** Confidence decay rate per ms when no update occurs. */
// Note: CONFIDENCE_DECAY_PER_MS was removed — confidence is always 1.0 after
// each successful tick. The ageMs field on the model indicates staleness instead.

/** Minimum actionable elements before vision is flagged as needed. */
const SPARSE_CONTEXT_THRESHOLD = 5;
const SOFT_STALE_MS = 1500;
const HARD_STALE_MS = 5000;
const SOFT_STALE_CONFIDENCE = 0.75;
const HARD_STALE_CONFIDENCE = 0.4;

// ─── Event classification ───────────────────────────────────────────────────

/** Events that warrant a full accessibility tree read. */
const SIGNIFICANT_EVENTS = new Set<CelEvent["type"]>([
  "TreeChanged",
  "ValueChanged",
  "WindowCreated",
  "SheetCreated",
  "LayoutChanged",
]);

/** Events that indicate potential anomalies. */
const ANOMALY_EVENTS = new Set<CelEvent["type"]>([
  "SheetCreated",
  "AppActivated",
  "WindowCreated",
]);

// ─── Instance Registry ──────────────────────────────────────────────────────
//
// Previously a strict singleton (only one cortex allowed). Now supports
// multiple concurrent instances via a registry. The default/primary cortex
// is the first one booted (for backwards compatibility).

const _activeInstances = new Map<string, Cortex>();
let _defaultCortexId: string | null = null;

/** Check if any cortex is currently running. */
export function isCortexActive(): boolean {
  return _activeInstances.size > 0;
}

/** Get the default (first-booted) cortex instance. Returns null if none is running. */
export function getActiveCortex(): Cortex | null {
  if (!_defaultCortexId) return null;
  return _activeInstances.get(_defaultCortexId) ?? null;
}

/** Get a specific cortex instance by ID. */
export function getCortexById(id: string): Cortex | null {
  return _activeInstances.get(id) ?? null;
}

/** Get all active cortex instance IDs. */
export function getActiveCortexIds(): string[] {
  return [..._activeInstances.keys()];
}

let _nextCortexId = 1;

// ─── Helpers ────────────────────────────────────────────────────────────────

function toPerceptionDiff(diff: ContextDiff): PerceptionDiff {
  return {
    addedCount: diff.added.length,
    removedCount: diff.removed.length,
    changedCount: diff.changed.length,
    unchangedCount: diff.unchangedCount,
    addedLabels: diff.added.slice(0, 10).map((el) => el.label ?? el.id),
    changedLabels: diff.changed.slice(0, 10).map((c) => c.element.label ?? c.element.id),
  };
}

function detectAnomaliesFromEvents(events: CelEvent[], expectedApp: string): Anomaly[] {
  const anomalies: Anomaly[] = [];
  const now = Date.now();

  for (const event of events) {
    if (event.type === "SheetCreated") {
      anomalies.push({
        type: "dialog",
        description: "A dialog or sheet appeared unexpectedly",
        timestamp: now,
      });
    }
    if (event.type === "AppActivated" && "app_name" in event && event.app_name !== expectedApp) {
      anomalies.push({
        type: "app_switch",
        title: (event as { app_name?: string }).app_name ?? undefined,
        description: `App switched to "${(event as { app_name?: string }).app_name}" (expected "${expectedApp}")`,
        timestamp: now,
      });
    }
  }

  return anomalies;
}

function detectAnomaliesFromContext(context: ScreenContext): Anomaly[] {
  const anomalies: Anomaly[] = [];
  const now = Date.now();

  for (const el of context.elements) {
    const label = (el.label ?? "").toLowerCase();
    const elType = el.element_type.toLowerCase();

    if (elType === "alert" || elType === "dialog") {
      anomalies.push({
        type: "dialog",
        title: el.label ?? undefined,
        description: `Dialog detected: "${el.label ?? "unknown"}"`,
        timestamp: now,
        elementIds: [el.id],
      });
    }

    if (label.includes("error") || label.includes("failed") || label.includes("exception")) {
      anomalies.push({
        type: "error",
        title: el.label ?? undefined,
        description: `Error element: "${el.label}"`,
        timestamp: now,
        elementIds: [el.id],
      });
    }

    if (
      (label.includes("sign in") || label.includes("log in") || label.includes("authenticate")) &&
      (elType === "dialog" || elType === "sheet" || elType === "window")
    ) {
      anomalies.push({
        type: "auth_prompt",
        title: el.label ?? undefined,
        description: `Auth prompt: "${el.label}"`,
        timestamp: now,
        elementIds: [el.id],
      });
    }
  }

  return anomalies;
}

// ─── Cortex ─────────────────────────────────────────────────────────────────

/** Options for creating a Cortex. */
export interface CortexOptions {
  /**
   * Custom context provider. When set, the cortex calls this instead of
   * cel.getContext(). This makes the cortex adapter-agnostic — it can
   * wrap a browser adapter's context pipeline, not just the desktop a11y tree.
   *
   * If not provided, defaults to cel.getContext() (native desktop).
   */
  getContext?: () => ScreenContext | Promise<ScreenContext>;
  /**
   * Custom quick context provider (cheap app/window check).
   * Defaults to cel.getQuickContext().
   */
  getQuickContext?: () => ScreenContext;
  /** Tick interval in ms. Default 200. */
  tickIntervalMs?: number;
  /** Age threshold for soft-stale classification. Default 1500ms. */
  softStaleMs?: number;
  /** Age threshold for hard-stale classification. Default 5000ms. */
  hardStaleMs?: number;
  /** Confidence threshold for soft-stale classification. Default 0.75. */
  softStaleConfidence?: number;
  /** Confidence threshold for hard-stale classification. Default 0.4. */
  hardStaleConfidence?: number;
}

/** Minimal CEL capability set needed by the Cortex. */
type CortexDeps = ContextProvider & EventSource;

export class Cortex {
  /** Unique instance ID for this cortex. */
  readonly id: string;

  private cel: CortexDeps;
  private contextProvider: () => ScreenContext | Promise<ScreenContext>;
  private quickContextProvider: () => ScreenContext;
  private tickMs: number;
  private timer: ReturnType<typeof setInterval> | null = null;
  private bootTime = 0;
  private lastUpdateTime = 0;
  private lastTickTime = 0;
  private lastContextHash = 0;
  private expectedApp = "";
  private running = false;
  private lastEventTime = 0;
  private lastSignificantEventTime = 0;
  private softStaleMs: number;
  private hardStaleMs: number;
  private softStaleConfidence: number;
  private hardStaleConfidence: number;

  // Element tracking for stability classification
  private elementSeenCount = new Map<string, number>();
  private elementLastSeen = new Set<string>();

  // Consecutive action failures (set externally via reportActionFailure)
  private consecutiveActionFailures = 0;

  /** Whether an active spinner/loading indicator was detected last tick. */
  private _spinnerDetected = false;

  /** The mental model — always current. Read this, never call getContext(). */
  readonly model: MentalModel;

  constructor(cel: CortexDeps, options?: CortexOptions & { id?: string }) {
    this.id = options?.id ?? `cortex-${_nextCortexId++}`;
    this.cel = cel;
    this.contextProvider = options?.getContext ?? (() => cel.getContext());
    this.quickContextProvider = options?.getQuickContext ?? (() => cel.getQuickContext());
    this.tickMs = options?.tickIntervalMs ?? TICK_INTERVAL_MS;
    this.softStaleMs = options?.softStaleMs ?? SOFT_STALE_MS;
    this.hardStaleMs = options?.hardStaleMs ?? HARD_STALE_MS;
    this.softStaleConfidence = options?.softStaleConfidence ?? SOFT_STALE_CONFIDENCE;
    this.hardStaleConfidence = options?.hardStaleConfidence ?? HARD_STALE_CONFIDENCE;
    this.model = {
      currentContext: { app: "", window: "", elements: [], timestamp_ms: 0 },
      focusedElement: null,
      recentDiffs: [],
      temporal: {
        loading: null,
        errorPersisting: null,
        idleSince: null,
        focusTrail: [],
        stagnantCycles: 0,
      },
      stability: {
        stable: new Set(),
        volatile: new Set(),
      },
      anomalyQueue: [],
      confidence: 0,
      visionNeeded: false,
      ageMs: 0,
      cycleCount: 0,
      uptimeMs: 0,
      freshness: undefined,
      lastDiffSummary: null,
    };
  }

  /** Boot the cortex — starts the background perception loop. */
  async boot(): Promise<void> {
    if (_activeInstances.has(this.id)) {
      throw new Error(`Cortex "${this.id}" is already running.`);
    }

    this.bootTime = Date.now();
    this.lastTickTime = Date.now();
    this.running = true;
    _activeInstances.set(this.id, this);
    if (!_defaultCortexId) _defaultCortexId = this.id;

    // Start Rust watchdog for event streams
    this.cel.startWatchdog();

    // Initial context capture — bootstrap the mental model
    const initialContext = await Promise.resolve(this.contextProvider());
    (this.model as { currentContext: ScreenContext }).currentContext = initialContext;
    this.lastUpdateTime = Date.now();
    this.lastContextHash = contextFingerprint(initialContext);
    this.expectedApp = initialContext.app;
    (this.model as { confidence: number }).confidence = 1.0;
    this.lastEventTime = this.lastUpdateTime;
    this.lastSignificantEventTime = 0;

    // Track initial elements
    for (const el of initialContext.elements) {
      this.elementSeenCount.set(el.id, 1);
      this.elementLastSeen.add(el.id);
    }

    // Find initial focused element
    this.updateFocusedElement(initialContext);
    enrichMentalModel(this.model);

    // Start the background loop
    this.timer = setInterval(() => this.tick(), this.tickMs);
  }

  /** Shutdown the cortex — stops the background loop and cleans up. */
  shutdown(): void {
    if (!this.running) return;

    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }

    this.cel.stopWatchdog();
    this.running = false;

    _activeInstances.delete(this.id);
    if (_defaultCortexId === this.id) {
      // Promote next instance or clear
      _defaultCortexId = _activeInstances.size > 0
        ? _activeInstances.keys().next().value ?? null
        : null;
    }
  }

  /** Is the cortex currently running? */
  isRunning(): boolean {
    return this.running;
  }

  /**
   * Notify the cortex that an action was taken.
   * The next tick will treat incoming events as post-action feedback.
   */
  notifyAction(action: string, _target?: string): void {
    // Reset idle tracking — something just happened
    (this.model.temporal as TemporalFlags).idleSince = null;
    (this.model.temporal as TemporalFlags).stagnantCycles = 0;
    // Force a full context refresh on next tick
    this.lastContextHash = 0;
  }

  /** Report a consecutive action failure (used by goal-runner). */
  reportActionFailure(): void {
    this.consecutiveActionFailures++;
  }

  /** Report a successful action (resets failure counter). */
  reportActionSuccess(): void {
    this.consecutiveActionFailures = 0;
  }

  /** Whether an active spinner is currently detected. */
  get spinnerDetected(): boolean {
    return this._spinnerDetected;
  }

  /** Consume anomalies from the queue (drains them). */
  consumeAnomalies(): Anomaly[] {
    const anomalies = [...this.model.anomalyQueue];
    (this.model as { anomalyQueue: Anomaly[] }).anomalyQueue = [];
    return anomalies;
  }

  /** Current freshness classification for routing decisions. */
  readFreshness(): FreshnessAssessment {
    const now = Date.now();
    const ageMs = this.lastUpdateTime ? now - this.lastUpdateTime : Number.POSITIVE_INFINITY;
    const confidence = this.model.confidence ?? 0;
    const causes = new Set<FreshnessAssessment["causes"][number]>();
    let state: FreshnessAssessment["state"] = "fresh";

    if (this.lastSignificantEventTime >= this.lastUpdateTime && this.lastSignificantEventTime > 0) {
      causes.add("event");
      state = "hard-stale";
    }
    if (ageMs >= this.hardStaleMs) {
      causes.add("time");
      state = "hard-stale";
    } else if (ageMs >= this.softStaleMs && state !== "hard-stale") {
      causes.add("time");
      state = "soft-stale";
    }
    if (confidence <= this.hardStaleConfidence) {
      causes.add("confidence");
      state = "hard-stale";
    } else if (confidence <= this.softStaleConfidence && state === "fresh") {
      causes.add("confidence");
      state = "soft-stale";
    }

    return {
      state,
      causes: [...causes],
      ageMs,
      confidence,
      lastUpdateMs: this.lastUpdateTime,
      lastEventMs: this.lastEventTime || null,
      lastSignificantEventMs: this.lastSignificantEventTime || null,
    };
  }

  /** Latest diff summary, if any. */
  readDiffSummary(): DiffSummary | null {
    return this.model.lastDiffSummary ?? null;
  }

  /** Ingest an action outcome so freshness/anomaly state can react immediately. */
  ingestActionOutcome(outcome: ActionOutcome): void {
    const timestamp = outcome.timestamp ?? Date.now();
    if (outcome.success) this.reportActionSuccess();
    else this.reportActionFailure();

    if (outcome.contradiction) {
      this.lastSignificantEventTime = timestamp;
    }

    if (outcome.sideEffectSummary) {
      const queue = this.model.anomalyQueue as Anomaly[];
      queue.push({
        type: "unexpected_navigation",
        description: outcome.sideEffectSummary,
        timestamp,
      });
      while (queue.length > MAX_ANOMALY_QUEUE) queue.shift();
    }

    if (outcome.verified === false) {
      (this.model as { confidence: number }).confidence = Math.min(this.model.confidence, 0.35);
    }

    enrichMentalModel(this.model);
  }

  // ─── Background tick ────────────────────────────────────────────────

  private ticking = false;

  private async tick(): Promise<void> {
    if (!this.running || this.ticking) return;
    this.ticking = true;

    try {
      await this.tickInner();
    } finally {
      this.ticking = false;
    }
  }

  private async tickInner(): Promise<void> {
    const now = Date.now();

    // 1. Drain event streams from Rust watchdog
    const events = this.cel.pollEvents();
    if (events.length > 0) this.lastEventTime = now;

    // 2. Classify: significant or noise?
    const hasSignificant = events.some((e) => SIGNIFICANT_EVENTS.has(e.type));
    if (hasSignificant) this.lastSignificantEventTime = now;

    // 3. Get context based on significance
    let newContext: ScreenContext | null = null;

    if (hasSignificant) {
      // Significant change — full context read
      newContext = await Promise.resolve(this.contextProvider());

      // Handle skeleton screens (loading states)
      if (isSkeletonScreen(newContext)) {
        const waitMs = skeletonWaitMs(newContext);
        if (waitMs > 0) {
          // Don't block the tick — flag loading instead
          (this.model.temporal as TemporalFlags).loading = {
            detected: true,
            durationMs: this.model.temporal.loading?.durationMs
              ? this.model.temporal.loading.durationMs + this.tickMs
              : 0,
          };
          // Still update the model with skeleton context
        }
      } else if (this.model.temporal.loading) {
        // Loading cleared
        (this.model.temporal as TemporalFlags).loading = null;
      }
    } else if (events.length > 0) {
      // Minor events (focus change, app switch) — quick check
      const quick = this.quickContextProvider();
      if (quick.app !== this.model.currentContext.app || quick.window !== this.model.currentContext.window) {
        // App or window changed — need full read
        newContext = await Promise.resolve(this.contextProvider());
      }
      // Otherwise: model is still valid, just process the events
    }
    // No events at all: model stays as-is, confidence decays

    // 4. Diff against current model
    let rawDiff: ContextDiff | null = null;
    let perceptionDiff: PerceptionDiff | null = null;

    if (newContext) {
      rawDiff = diffContexts(this.model.currentContext, newContext);
      if (isDiffSignificant(rawDiff)) {
        perceptionDiff = toPerceptionDiff(rawDiff);
      }
    }

    // 5. Apply diff to mental model
    if (newContext) {
      // Detect major transitions (app switch, page navigation) — reset tracking
      const appChanged = newContext.app !== this.model.currentContext.app;
      const windowChanged = newContext.window !== this.model.currentContext.window;
      if (appChanged || windowChanged) {
        this.resetTracking();
        if (appChanged) this.expectedApp = newContext.app;
      }

      (this.model as { currentContext: ScreenContext }).currentContext = newContext;
      this.lastUpdateTime = now;
      this.lastContextHash = contextFingerprint(newContext);
      (this.model as { confidence: number }).confidence = 1.0;
      if (hasSignificant) this.lastSignificantEventTime = 0;

      // Update focused element
      this.updateFocusedElement(newContext);
    }

    // Store diff in rolling window
    if (perceptionDiff) {
      const diffs = this.model.recentDiffs as PerceptionDiff[];
      diffs.push(perceptionDiff);
      if (diffs.length > MAX_RECENT_DIFFS) diffs.shift();
    }
    (this.model as { lastDiffSummary?: DiffSummary | null }).lastDiffSummary = perceptionDiff
      ? {
          addedCount: perceptionDiff.addedCount,
          removedCount: perceptionDiff.removedCount,
          changedCount: perceptionDiff.changedCount,
          unchangedCount: perceptionDiff.unchangedCount,
        }
      : this.model.lastDiffSummary ?? null;

    // 6. Update temporal patterns
    this.updateTemporalFlags(events, newContext, perceptionDiff);

    // 7. Classify element stability
    if (newContext) {
      this.updateElementStability(newContext);
    }

    // 8. Detect anomalies → push to queue
    const eventAnomalies = detectAnomaliesFromEvents(events, this.expectedApp);
    const contextAnomalies = newContext ? detectAnomaliesFromContext(newContext) : [];
    const newAnomalies = [...eventAnomalies, ...contextAnomalies];

    if (newAnomalies.length > 0) {
      const queue = this.model.anomalyQueue as Anomaly[];
      // Dedup: don't push if same type+description already in queue (within last 5s)
      const now2 = Date.now();
      for (const anomaly of newAnomalies) {
        const isDuplicate = queue.some(
          (q) => q.type === anomaly.type && q.description === anomaly.description && now2 - q.timestamp < 5000,
        );
        if (!isDuplicate) {
          queue.push(anomaly);
        }
      }
      // TTL: remove anomalies older than 30 seconds
      const ttlCutoff = now2 - 30_000;
      while (queue.length > 0 && queue[0].timestamp < ttlCutoff) queue.shift();
      // Cap queue size as safety net
      while (queue.length > MAX_ANOMALY_QUEUE) queue.shift();
    }

    // 8b. Spinner detection
    this._spinnerDetected = hasActiveSpinner(this.model.currentContext);

    // Note: dismissable-dialog detection was removed — overlay/cookie banner
    // handling lives in the browser adapter (overlay-detector.ts) where the
    // DOM/CSS/CMP signals actually exist. The cortex still reports generic
    // "dialog" anomalies via anomaly.rs / detectAnomaliesFromContext.

    // 9. Confidence: the tick completed successfully — the model is valid.
    // The tick ran, we checked events, we got context if needed.
    // If nothing changed, that means the model IS correct (confidence = 1.0).
    // Confidence is always 1.0 after a successful tick. It's set to < 1.0
    // only by external code if the cortex is known to be impaired.
    (this.model as { confidence: number }).confidence = 1.0;

    // 10. Vision needed flag
    const actionableCount = this.model.currentContext.elements.filter(
      (el) => el.state?.enabled && el.state?.visible && (el.actions?.length ?? 0) > 0,
    ).length;
    (this.model as { visionNeeded: boolean }).visionNeeded =
      actionableCount < SPARSE_CONTEXT_THRESHOLD ||
      this.consecutiveActionFailures >= 2;

    // 11. Update meta counters
    (this.model as { cycleCount: number }).cycleCount++;
    (this.model as { ageMs: number }).ageMs = now - this.lastUpdateTime;
    (this.model as { uptimeMs: number }).uptimeMs = now - this.bootTime;
    enrichMentalModel(this.model);
  }

  /**
   * Reset tracking state — called on major transitions (app switch, page navigation).
   * Clears stability counts, diffs, temporal flags, and anomaly queue so the
   * cortex doesn't carry stale knowledge from a previous context.
   */
  private resetTracking(): void {
    this.elementSeenCount.clear();
    this.elementLastSeen.clear();
    this.consecutiveActionFailures = 0;

    // Clear rolling data
    (this.model.recentDiffs as PerceptionDiff[]).length = 0;
    (this.model as { anomalyQueue: Anomaly[] }).anomalyQueue = [];

    // Reset stability
    (this.model.stability as ElementStability).stable = new Set();
    (this.model.stability as ElementStability).volatile = new Set();

    // Reset temporal (keep focusTrail — it's useful across transitions)
    const temporal = this.model.temporal as TemporalFlags;
    temporal.loading = null;
    temporal.errorPersisting = null;
    temporal.idleSince = null;
    temporal.stagnantCycles = 0;
  }

  // ─── Internal helpers ───────────────────────────────────────────────

  private updateFocusedElement(context: ScreenContext): void {
    let focused: { id: string; label?: string } | null = null;
    for (const el of context.elements) {
      if (el.state?.focused) {
        focused = { id: el.id, label: el.label };
        break;
      }
    }
    (this.model as { focusedElement: typeof focused }).focusedElement = focused;

    // Update focus trail
    if (focused) {
      const trail = this.model.temporal.focusTrail as string[];
      const label = focused.label ?? focused.id;
      if (trail.length === 0 || trail[trail.length - 1] !== label) {
        trail.push(label);
        if (trail.length > MAX_FOCUS_TRAIL) trail.shift();
      }
    }
  }

  private updateTemporalFlags(
    events: CelEvent[],
    newContext: ScreenContext | null,
    diff: PerceptionDiff | null,
  ): void {
    const temporal = this.model.temporal as TemporalFlags;
    const now = Date.now();

    // Stagnant cycles (no significant change)
    if (!diff) {
      temporal.stagnantCycles++;
    } else {
      temporal.stagnantCycles = 0;
    }

    // Idle detection
    if (diff || events.length > 0) {
      temporal.idleSince = null;
    } else if (temporal.idleSince === null) {
      temporal.idleSince = now;
    }

    // Error persistence
    if (newContext) {
      const hasError = newContext.elements.some((el) => {
        const label = (el.label ?? "").toLowerCase();
        return label.includes("error") || label.includes("failed") || label.includes("exception");
      });

      if (hasError) {
        if (temporal.errorPersisting) {
          temporal.errorPersisting.durationMs += this.tickMs;
        } else {
          const errorEl = newContext.elements.find((el) => {
            const label = (el.label ?? "").toLowerCase();
            return label.includes("error") || label.includes("failed");
          });
          temporal.errorPersisting = {
            detected: true,
            durationMs: 0,
            message: errorEl?.label ?? undefined,
          };
        }
      } else {
        temporal.errorPersisting = null;
      }
    }

    // Loading persistence (skeleton detection is handled in main tick)
    if (temporal.loading?.detected) {
      temporal.loading.durationMs += this.tickMs;
    }
  }

  private updateElementStability(context: ScreenContext): void {
    const currentIds = new Set<string>();

    for (const el of context.elements) {
      currentIds.add(el.id);

      // Cap at STABLE_THRESHOLD + 1 to prevent unbounded growth on long runs
      const prev = this.elementSeenCount.get(el.id) ?? 0;
      const count = Math.min(prev + 1, STABLE_THRESHOLD + 1);
      this.elementSeenCount.set(el.id, count);
    }

    // Elements no longer present — remove from tracking
    for (const id of this.elementLastSeen) {
      if (!currentIds.has(id)) {
        this.elementSeenCount.delete(id);
      }
    }
    this.elementLastSeen = currentIds;

    // Hard cap: prune oldest entries if map grows beyond 2000
    // (shouldn't happen with the above cleanup, but safety net for edge cases)
    if (this.elementSeenCount.size > 2000) {
      const excess = this.elementSeenCount.size - 1500;
      const iter = this.elementSeenCount.keys();
      for (let i = 0; i < excess; i++) {
        const next = iter.next();
        if (next.done) break;
        this.elementSeenCount.delete(next.value);
      }
    }

    // Classify stability
    const stable = new Set<string>();
    const volatile = new Set<string>();

    for (const [id, count] of this.elementSeenCount) {
      if (count >= STABLE_THRESHOLD) {
        stable.add(id);
      }
    }

    // Volatile: elements that appeared in recent diffs' addedLabels frequently
    // (simplified: elements seen < 2 cycles are volatile)
    for (const id of currentIds) {
      const count = this.elementSeenCount.get(id) ?? 0;
      if (count <= 1) {
        volatile.add(id);
      }
    }

    (this.model.stability as ElementStability).stable = stable;
    (this.model.stability as ElementStability).volatile = volatile;
  }
}
