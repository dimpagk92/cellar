/**
 * Action Cache
 *
 * Caches action sequences keyed by (instruction, stateFingerprint, variableKeys).
 * On cache hit, actions are replayed deterministically with zero LLM calls.
 *
 * Variable support: %placeholder% patterns in action text fields are stored
 * in the cache and resolved at runtime from the variables map.
 *
 * Self-healing integration: when a cached action fails, the cache entry can
 * be updated with the repaired action via `repair()`.
 */

import type { PlannedAction } from "../types.js";
import { computeCacheKey } from "./cache-key.js";
import type { CacheStorage, CacheEntry, CachedAction } from "./cache-storage.js";

/** Variable placeholder pattern: %variableName% */
const VARIABLE_PATTERN = /%([a-zA-Z_][\w-]*)%/g;

/**
 * The ActCache manages cached action sequences for goal execution.
 */
export class ActCache {
  constructor(private storage: CacheStorage) {}

  /**
   * Look up cached actions for a given instruction + state.
   * Returns resolved actions (with variables substituted) or null on miss.
   */
  async lookup(
    instruction: string,
    stateFingerprint: string,
    variables: Record<string, string> = {},
  ): Promise<{ key: string; actions: PlannedAction[] } | null> {
    const variableKeys = Object.keys(variables);
    const key = computeCacheKey(instruction, stateFingerprint, variableKeys);

    const entry = await this.storage.get(key);
    if (!entry) return null;

    // Verify variable keys match
    const entryKeys = [...entry.variableKeys].sort();
    const currentKeys = [...variableKeys].sort();
    if (JSON.stringify(entryKeys) !== JSON.stringify(currentKeys)) {
      return null; // Variable shape changed — cache invalid
    }

    // Resolve variables in cached actions
    const actions = entry.actions.map((cached) =>
      resolveVariables(cached, variables),
    );

    return { key, actions };
  }

  /**
   * Store a sequence of actions in the cache.
   * Extracts variable placeholders from action text fields.
   */
  async store(
    instruction: string,
    stateFingerprint: string,
    variables: Record<string, string> = {},
    actions: PlannedAction[],
  ): Promise<string> {
    const variableKeys = Object.keys(variables);
    const key = computeCacheKey(instruction, stateFingerprint, variableKeys);

    // Convert actions to cached format with placeholder extraction
    const cachedActions: CachedAction[] = actions.map((action) =>
      extractPlaceholders(action, variables),
    );

    const entry: CacheEntry = {
      version: 1,
      key,
      instruction,
      stateFingerprint,
      variableKeys,
      actions: cachedActions,
      createdAt: Date.now(),
      lastUsedAt: Date.now(),
      hitCount: 0,
    };

    await this.storage.set(key, entry);
    return key;
  }

  /**
   * Repair a specific action in a cache entry (called after self-healing).
   * Replaces the action at the given index with the repaired version.
   */
  async repair(
    key: string,
    stepIndex: number,
    newAction: PlannedAction,
    variables: Record<string, string> = {},
  ): Promise<void> {
    const entry = await this.storage.get(key);
    if (!entry) return;
    if (stepIndex < 0 || stepIndex >= entry.actions.length) return;

    entry.actions[stepIndex] = extractPlaceholders(newAction, variables);
    await this.storage.set(key, entry);
  }

  /** Delete a cache entry. */
  async invalidate(key: string): Promise<void> {
    await this.storage.delete(key);
  }

  /** Check if a cache entry exists. */
  async has(
    instruction: string,
    stateFingerprint: string,
    variables: Record<string, string> = {},
  ): Promise<boolean> {
    const key = computeCacheKey(
      instruction,
      stateFingerprint,
      Object.keys(variables),
    );
    return this.storage.has(key);
  }
}

/**
 * Extract variable placeholders from an action.
 * Replaces actual variable values with %placeholder% tokens.
 */
function extractPlaceholders(
  action: PlannedAction,
  variables: Record<string, string>,
): CachedAction {
  const placeholders: Record<string, string> = {};
  const actionCopy = JSON.parse(JSON.stringify(action)) as Record<string, unknown>;

  // For type actions, replace variable values with placeholders in the text field
  if (actionCopy.type === "type" && typeof actionCopy.text === "string") {
    let text = actionCopy.text as string;
    for (const [key, value] of Object.entries(variables)) {
      if (value && text.includes(value)) {
        text = text.replaceAll(value, `%${key}%`);
        placeholders[`%${key}%`] = key;
      }
    }
    actionCopy.text = text;
  }

  return {
    action: actionCopy,
    variablePlaceholders: placeholders,
  };
}

/**
 * Resolve variable placeholders in a cached action back to actual values.
 */
function resolveVariables(
  cached: CachedAction,
  variables: Record<string, string>,
): PlannedAction {
  const actionCopy = JSON.parse(JSON.stringify(cached.action)) as Record<string, unknown>;

  if (actionCopy.type === "type" && typeof actionCopy.text === "string") {
    let text = actionCopy.text as string;
    text = text.replace(VARIABLE_PATTERN, (_match, key) => {
      return variables[key] ?? `%${key}%`;
    });
    actionCopy.text = text;
  }

  return actionCopy as unknown as PlannedAction;
}
