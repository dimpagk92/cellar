/**
 * Canary sampling for the tier-replan rollout.
 *
 * Given a goal identifier and a target percentage, deterministically decide
 * whether this goal falls into the enabled cohort. Same goal ID → same
 * decision (hash-based), so repeated runs of the same workflow stay in the
 * same bucket.
 *
 * Usage pattern:
 *   const cohort = canaryCohort(config.workflowName ?? config.goal, 10);
 *   const effectiveConfig = {
 *     ...config,
 *     enableTierReplan: cohort === "enabled" ? true : config.enableTierReplan,
 *   };
 *   await runGoal(cel, effectiveConfig, callbacks);
 *
 * Uses FNV-1a 32-bit hash — good-enough distribution, no crypto dependency,
 * deterministic across restarts, ~0 overhead.
 */

export type CanaryCohort = "enabled" | "control";

/**
 * Deterministically bucket a goal into the enabled cohort if its hash falls
 * below the percentage threshold. `percentage` is 0-100.
 *
 *   canaryCohort("hotel-booking", 10) → "control"   (always, hash falls above 10%)
 *   canaryCohort("hotel-booking", 50) → "enabled"   (hash consistent across calls)
 *
 * Pass `0` for no rollout, `100` for full rollout.
 */
export function canaryCohort(key: string, percentage: number): CanaryCohort {
  if (percentage <= 0) return "control";
  if (percentage >= 100) return "enabled";
  const bucket = hashToPercent(key);
  return bucket < percentage ? "enabled" : "control";
}

/**
 * FNV-1a 32-bit hash mapped to 0-99 inclusive. Distribution is acceptable
 * for rollout cohorts; not cryptographically strong. Stable across Node
 * restarts because it operates on bytes, not object identity.
 */
function hashToPercent(key: string): number {
  let hash = 0x811c9dc5; // FNV offset basis
  for (let i = 0; i < key.length; i++) {
    hash ^= key.charCodeAt(i);
    // 16777619 — FNV prime. Math.imul keeps this as i32 (avoids precision loss
    // at the 32-bit boundary that would otherwise sometimes bias the bucket).
    hash = Math.imul(hash, 0x01000193);
  }
  // Convert to unsigned 32-bit, modulo 100.
  return ((hash >>> 0) % 100);
}

/**
 * Resolve the canary percentage from environment. Operators set
 * `CELLAR_TIER_REPLAN_PCT=25` to enable 25% of goals; unset → 0.
 * Values outside [0,100] are clamped.
 */
export function resolveCanaryPercentage(envVar = "CELLAR_TIER_REPLAN_PCT"): number {
  if (typeof process === "undefined") return 0;
  const raw = process.env[envVar];
  if (!raw) return 0;
  const n = Number.parseInt(raw, 10);
  if (Number.isNaN(n)) return 0;
  return Math.max(0, Math.min(100, n));
}

/**
 * Convenience: override a GoalRunnerConfig's tier-replan flag based on canary
 * cohort. Returns a new config object; does not mutate input.
 */
export function applyCanaryOverride<
  T extends { goal: string; workflowName?: string; enableTierReplan?: boolean },
>(config: T, percentage: number = resolveCanaryPercentage()): T {
  const key = config.workflowName ?? config.goal;
  const cohort = canaryCohort(key, percentage);
  if (cohort === "enabled" && !config.enableTierReplan) {
    return { ...config, enableTierReplan: true };
  }
  return config;
}
