import type { Anomaly, MentalModel } from "./types.js";
import { enrichMentalModel } from "./cortex-insight.js";

export function normalizeCortexModel(raw: unknown): MentalModel | null {
  if (!raw || typeof raw !== "object") return null;
  const parsed = typeof raw === "string" ? JSON.parse(raw) : raw as Record<string, any>;

  if (parsed.current_context && !parsed.currentContext) parsed.currentContext = parsed.current_context;
  if (parsed.focused_element && !parsed.focusedElement) parsed.focusedElement = parsed.focused_element;
  if (parsed.recent_diffs && !parsed.recentDiffs) parsed.recentDiffs = parsed.recent_diffs;
  if (parsed.last_diff_summary && !parsed.lastDiffSummary) parsed.lastDiffSummary = parsed.last_diff_summary;
  if (parsed.vision_needed !== undefined && parsed.visionNeeded === undefined) parsed.visionNeeded = parsed.vision_needed;
  if (parsed.age_ms !== undefined && parsed.ageMs === undefined) parsed.ageMs = parsed.age_ms;
  if (parsed.cycle_count !== undefined && parsed.cycleCount === undefined) parsed.cycleCount = parsed.cycle_count;
  if (parsed.uptime_ms !== undefined && parsed.uptimeMs === undefined) parsed.uptimeMs = parsed.uptime_ms;
  if (parsed.stream_status && !parsed.streamStatus) parsed.streamStatus = parsed.stream_status;
  if (parsed.active_adapters && !parsed.activeAdapters) parsed.activeAdapters = parsed.active_adapters;
  if (parsed.element_adapter_index && !parsed.elementAdapterIndex) {
    parsed.elementAdapterIndex = parsed.element_adapter_index;
  }
  if (parsed.source_summary && !parsed.sourceSummary) parsed.sourceSummary = parsed.source_summary;
  if (parsed.recent_transition !== undefined && parsed.recentTransition === undefined) {
    parsed.recentTransition = parsed.recent_transition;
  }
  if (parsed.likely_blocker !== undefined && parsed.likelyBlocker === undefined) {
    parsed.likelyBlocker = parsed.likely_blocker;
  }
  if (parsed.suggested_next_step !== undefined && parsed.suggestedNextStep === undefined) {
    parsed.suggestedNextStep = parsed.suggested_next_step;
  }

  if (parsed.temporal) {
    const t = parsed.temporal;
    if (t.idle_since !== undefined && t.idleSince === undefined) t.idleSince = t.idle_since;
    if (t.focus_trail !== undefined && t.focusTrail === undefined) t.focusTrail = t.focus_trail;
    if (t.stagnant_cycles !== undefined && t.stagnantCycles === undefined) t.stagnantCycles = t.stagnant_cycles;
    if (t.error_persisting !== undefined && t.errorPersisting === undefined) t.errorPersisting = t.error_persisting;
    if (t.loading?.duration_ms !== undefined && t.loading.durationMs === undefined) t.loading.durationMs = t.loading.duration_ms;
    if (t.errorPersisting?.duration_ms !== undefined && t.errorPersisting.durationMs === undefined) {
      t.errorPersisting.durationMs = t.errorPersisting.duration_ms;
    }
  }

  if (parsed.freshness) {
    const f = parsed.freshness;
    if (f.last_update_ms !== undefined && f.lastUpdateMs === undefined) f.lastUpdateMs = f.last_update_ms;
    if (f.last_event_ms !== undefined && f.lastEventMs === undefined) f.lastEventMs = f.last_event_ms;
    if (f.last_significant_event_ms !== undefined && f.lastSignificantEventMs === undefined) {
      f.lastSignificantEventMs = f.last_significant_event_ms;
    }
  }

  if (parsed.currentContext) {
    const ctx = parsed.currentContext;
    if (ctx.timestamp_ms === undefined && ctx.timestampMs !== undefined) ctx.timestamp_ms = ctx.timestampMs;
    if (Array.isArray(ctx.elements)) {
      for (const element of ctx.elements) {
        if (element.elementType !== undefined && element.element_type === undefined) {
          element.element_type = element.elementType;
        }
      }
    }
  }

  if (parsed.streamStatus) {
    const s = parsed.streamStatus;
    if (s.audio_capture !== undefined && s.audioCapture === undefined) {
      s.audioCapture = s.audio_capture;
    }
  }

  if (parsed.stability) {
    parsed.stability.stable = new Set(parsed.stability.stable ?? []);
    parsed.stability.volatile = new Set(parsed.stability.volatile ?? []);
  }

  if (parsed.semantic) {
    const semantic = parsed.semantic;
    if (semantic.current_activity !== undefined && semantic.currentActivity === undefined) {
      semantic.currentActivity = semantic.current_activity;
    }
    if (semantic.recent_transition !== undefined && semantic.recentTransition === undefined) {
      semantic.recentTransition = semantic.recent_transition;
    }
    if (semantic.likely_blocker !== undefined && semantic.likelyBlocker === undefined) {
      semantic.likelyBlocker = semantic.likely_blocker;
    }
    if (semantic.suggested_next_step !== undefined && semantic.suggestedNextStep === undefined) {
      semantic.suggestedNextStep = semantic.suggested_next_step;
    }
    if (semantic.task_phase !== undefined && semantic.taskPhase === undefined) {
      semantic.taskPhase = semantic.task_phase;
    }
  }

  return enrichMentalModel(parsed as MentalModel);
}

export function normalizeCortexAnomalies(raw: unknown): Anomaly[] {
  if (!raw) return [];
  const parsed = typeof raw === "string" ? JSON.parse(raw) : raw;
  return Array.isArray(parsed) ? parsed as Anomaly[] : [];
}
