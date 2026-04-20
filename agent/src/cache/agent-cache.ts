/**
 * Agent Cache — Multi-Step Workflow Replay
 *
 * Caches entire goal execution sequences (multiple steps) for deterministic replay.
 * Separate from ActCache because it stores the full step sequence with pre/post
 * state fingerprints for navigation validation during replay.
 *
 * Self-healing per step: if a cached step fails during replay, the self-healer
 * attempts repair, and the cache entry is updated with the healed action.
 */

import type { PlannedAction } from "../types.js";
import { computeCacheKey } from "./cache-key.js";
import type { CacheStorage } from "./cache-storage.js";

/** A single cached step in a workflow replay sequence. */
export interface CachedStep {
  /** The planned action to execute. */
  action: PlannedAction;
  /** State fingerprint (URL, window title) before this step. */
  preFingerprint: string;
  /** State fingerprint after this step completed successfully. */
  postFingerprint: string;
  /** Runtime variables at time of recording. */
  variables: Record<string, string>;
}

/** A complete cached agent workflow. */
export interface AgentCacheEntry {
  version: 1;
  key: string;
  goal: string;
  startFingerprint: string;
  steps: CachedStep[];
  totalMs: number;
  createdAt: number;
  lastUsedAt: number;
  hitCount: number;
}

/**
 * The AgentCache manages cached multi-step workflow sequences.
 * Reuses the same CacheStorage interface as ActCache but stores
 * in a separate namespace (`~/.cellar/cache/agent/`).
 */
export class AgentCache {
  constructor(private storage: CacheStorage) {}

  /**
   * Look up a cached workflow for a given goal + starting state.
   */
  async lookup(
    goal: string,
    startFingerprint: string,
  ): Promise<AgentCacheEntry | null> {
    const key = computeCacheKey(goal, startFingerprint);

    const raw = await this.storage.get(key);
    if (!raw) return null;

    // Coerce the generic CacheEntry to AgentCacheEntry via the stored data
    const entry = raw as unknown as AgentCacheEntry;
    if (!entry.steps || !Array.isArray(entry.steps)) return null;

    return entry;
  }

  /**
   * Store a completed workflow execution for future replay.
   */
  async store(
    goal: string,
    startFingerprint: string,
    steps: CachedStep[],
    totalMs: number,
  ): Promise<string> {
    const key = computeCacheKey(goal, startFingerprint);

    const entry: AgentCacheEntry = {
      version: 1,
      key,
      goal,
      startFingerprint,
      steps,
      totalMs,
      createdAt: Date.now(),
      lastUsedAt: Date.now(),
      hitCount: 0,
    };

    // Store as a generic CacheEntry shape (compatible with CacheStorage)
    await this.storage.set(key, entry as unknown as import("./cache-storage.js").CacheEntry);
    return key;
  }

  /**
   * Repair a specific step in a cached workflow (called after self-healing).
   */
  async repairStep(
    key: string,
    stepIndex: number,
    newStep: CachedStep,
  ): Promise<void> {
    const raw = await this.storage.get(key);
    if (!raw) return;

    const entry = raw as unknown as AgentCacheEntry;
    if (!entry.steps || stepIndex < 0 || stepIndex >= entry.steps.length) return;

    entry.steps[stepIndex] = newStep;
    await this.storage.set(key, entry as unknown as import("./cache-storage.js").CacheEntry);
  }

  /** Delete a cached workflow. */
  async invalidate(key: string): Promise<void> {
    await this.storage.delete(key);
  }
}
