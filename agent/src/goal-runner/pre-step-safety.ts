/**
 * Safety layer for feasibility pre_step auto-execution.
 *
 * The feasibility LLM can return pre_steps like `["open Chrome"]` which, when
 * `enableFeasibilityPreSteps=true`, the runner types into the user's real
 * desktop via Spotlight. This module enforces the mitigations from
 * docs/security-replan-hardening.md so an attacker controlling LLM output
 * (via prompt injection in page content) cannot coerce the agent into
 * opening sensitive apps.
 *
 * Boundaries enforced:
 *   1. MAX_PRE_STEPS_PER_GOAL — hard cap on how many pre_steps fire per run
 *   2. APP_ALLOWLIST — only named browsers are valid targets
 *   3. SENSITIVE_APP_BLOCKLIST — explicit deny for credential/mail apps
 *   4. Regex is strict (1-40 chars, alphanumeric + space/dot/hyphen only)
 *   5. Array length cap on the pre_steps list itself
 *
 * Mitigations NOT handled here (because they're a different layer):
 *   - Config injection guard (operators shouldn't take `enable*` from untrusted input)
 *   - Prompt injection detection (addressed at the LLM provider / system prompt level)
 */

/** Max pre_step executions per `runGoal()` call. Hard cap on exposure. */
export const MAX_PRE_STEPS_PER_GOAL = 1;

/** Max length of the pre_steps array the LLM can return. Excess is truncated. */
export const MAX_PRE_STEPS_ARRAY_LENGTH = 3;

/**
 * Allowlist — only these apps may be opened by pre_step auto-execution. Case-
 * insensitive exact match against the extracted app name. Keep this list
 * intentionally narrow; adding entries requires a security re-review.
 */
export const APP_ALLOWLIST = new Set([
  "chrome",
  "google chrome",
  "safari",
  "firefox",
  "edge",
  "microsoft edge",
  "arc",
  "brave",
  "chromium",
  "opera",
  "vivaldi",
]);

/**
 * Blocklist — explicit deny even if the allowlist changes. Protects against
 * well-known sensitive app names appearing via prompt injection.
 */
export const APP_BLOCKLIST = new Set([
  "passwords",
  "keychain access",
  "keychain",
  "1password",
  "bitwarden",
  "lastpass",
  "mail",
  "messages",
  "terminal",
  "iterm",
  "iterm2",
  "system preferences",
  "system settings",
]);

/**
 * Strict pre_step regex. Matches only "open X" / "launch X" / "start X"
 * where X is 1-40 characters from a conservative charset (alphanumeric,
 * space, dot, hyphen). Blocks shell metacharacters, paths, URLs, and
 * anything resembling a command with trailing clauses.
 */
export const PRE_STEP_REGEX = /^(?:open|launch|start)\s+([A-Za-z0-9][A-Za-z0-9 .\-]{0,39})\s*$/i;

/**
 * Classify a candidate pre_step string against the safety boundaries.
 * Returns the normalized app name if it passes, or a rejection reason.
 */
export type PreStepDecision =
  | { kind: "allow"; appName: string }
  | { kind: "reject"; reason: string; raw: string };

export function evaluatePreStep(raw: string): PreStepDecision {
  const trimmed = String(raw ?? "").trim();
  const match = PRE_STEP_REGEX.exec(trimmed);
  if (!match) {
    return { kind: "reject", reason: "did not match strict pre_step regex", raw: trimmed };
  }
  const appName = match[1].trim();
  const lowered = appName.toLowerCase();
  if (APP_BLOCKLIST.has(lowered)) {
    return { kind: "reject", reason: `app "${appName}" is on the sensitive-app blocklist`, raw: trimmed };
  }
  if (!APP_ALLOWLIST.has(lowered)) {
    return { kind: "reject", reason: `app "${appName}" is not on the browser allowlist`, raw: trimmed };
  }
  return { kind: "allow", appName };
}

/**
 * Filter a pre_steps array down to the allowed subset, enforcing all the
 * boundaries in one place. Returns at most MAX_PRE_STEPS_PER_GOAL allowed
 * entries even if more pass the safety check.
 */
export function filterPreSteps(
  preSteps: unknown,
): { allowed: Array<{ raw: string; appName: string }>; rejected: Array<{ raw: string; reason: string }> } {
  const allowed: Array<{ raw: string; appName: string }> = [];
  const rejected: Array<{ raw: string; reason: string }> = [];
  if (!Array.isArray(preSteps)) return { allowed, rejected };

  const truncated = preSteps.slice(0, MAX_PRE_STEPS_ARRAY_LENGTH);
  for (const step of truncated) {
    if (allowed.length >= MAX_PRE_STEPS_PER_GOAL) {
      rejected.push({ raw: String(step), reason: `exceeds MAX_PRE_STEPS_PER_GOAL (${MAX_PRE_STEPS_PER_GOAL})` });
      continue;
    }
    const decision = evaluatePreStep(String(step));
    if (decision.kind === "allow") {
      allowed.push({ raw: decision.appName, appName: decision.appName });
    } else {
      rejected.push({ raw: decision.raw, reason: decision.reason });
    }
  }
  return { allowed, rejected };
}
