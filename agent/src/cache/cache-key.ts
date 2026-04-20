/**
 * Cache Key Generation
 *
 * Computes deterministic SHA-256 cache keys from (instruction, stateFingerprint, variableKeys).
 * Variable VALUES are excluded from the key — only names are hashed — so sensitive
 * data (passwords, tokens) never appears in cache keys or filenames.
 */

import { createHash } from "node:crypto";

/**
 * Compute a deterministic cache key for an action or goal.
 *
 * @param instruction - The natural-language instruction/goal
 * @param stateFingerprint - URL, window title, or other state identifier
 * @param variableKeys - Sorted list of variable placeholder names (values excluded)
 * @returns SHA-256 hex string
 */
export function computeCacheKey(
  instruction: string,
  stateFingerprint: string,
  variableKeys: string[] = [],
): string {
  const normalized = {
    instruction: normalizeInstruction(instruction),
    stateFingerprint: normalizeFingerprint(stateFingerprint),
    variableKeys: [...variableKeys].sort(),
  };
  const canonical = JSON.stringify(normalized);
  return createHash("sha256").update(canonical).digest("hex");
}

/** Normalize instruction: trim whitespace, collapse internal whitespace, lowercase. */
function normalizeInstruction(instruction: string): string {
  return instruction.trim().replace(/\s+/g, " ").toLowerCase();
}

/**
 * Normalize a state fingerprint (typically a URL):
 * - Strip fragment (#...)
 * - Sort query parameters for consistency
 * - Lowercase protocol and host
 */
function normalizeFingerprint(fp: string): string {
  // Try to parse as URL for normalization
  try {
    const url = new URL(fp);
    url.hash = "";
    // Sort search params
    const params = [...url.searchParams.entries()].sort(([a], [b]) =>
      a.localeCompare(b),
    );
    url.search = "";
    for (const [key, value] of params) {
      url.searchParams.append(key, value);
    }
    return url.toString();
  } catch {
    // Not a URL — return trimmed lowercase
    return fp.trim().toLowerCase();
  }
}
