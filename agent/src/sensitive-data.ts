/**
 * Sensitive data masking — prevents secrets from leaking into LLM prompts.
 *
 * Replaces known secret values with <secret>{key}</secret> placeholders before
 * sending to the LLM, and restores them when executing actions.
 */

import type { ContextElement, PlannedAction } from "./types.js";

export interface SensitiveDataConfig {
  /** Key-value pairs of sensitive data. Keys are labels, values are the actual secrets. */
  secrets: Record<string, string>;
  /** Allowed domains for domain-scoped secrets. */
  allowedDomains?: string[];
}

/** Fields on input-like elements that indicate sensitive data. */
const SENSITIVE_INPUT_TYPES = new Set([
  "password",
  "secret",
  "hidden",
]);

/** Attribute names/patterns that suggest sensitive fields. */
const SENSITIVE_ATTR_PATTERNS = [
  /password/i,
  /secret/i,
  /token/i,
  /api[_-]?key/i,
  /credential/i,
  /ssn/i,
  /credit[_-]?card/i,
  /cvv/i,
];

export class SensitiveDataMasker {
  private secrets: Map<string, string>;
  private allowedDomains: Set<string>;

  constructor(config: SensitiveDataConfig) {
    // Sort by value length descending so longer secrets are matched first
    // (prevents partial masking of overlapping values).
    const sorted = Object.entries(config.secrets).sort(
      ([, a], [, b]) => b.length - a.length,
    );
    this.secrets = new Map(sorted);
    this.allowedDomains = new Set(config.allowedDomains ?? []);
  }

  /** Replace secret values with <secret>{key}</secret> placeholders. */
  mask(text: string): string {
    let result = text;
    for (const [key, value] of this.secrets) {
      if (value.length === 0) continue;
      // Use split+join for global replace without regex special char issues
      result = result.split(value).join(`<secret>${key}</secret>`);
    }
    return result;
  }

  /** Replace <secret>{key}</secret> placeholders with actual values. */
  unmask(text: string): string {
    let result = text;
    for (const [key, value] of this.secrets) {
      const placeholder = `<secret>${key}</secret>`;
      result = result.split(placeholder).join(value);
    }
    return result;
  }

  /**
   * Detect if a context element represents a sensitive field.
   * Returns the detected sensitivity type (e.g., "password") or null.
   */
  detectSensitiveField(element: ContextElement): string | null {
    // Check input type
    const inputType = element.properties?.["input_type"] ?? element.properties?.["type"];
    if (inputType && SENSITIVE_INPUT_TYPES.has(inputType.toLowerCase())) {
      return inputType.toLowerCase();
    }

    // Check element_type
    if (element.element_type === "password") {
      return "password";
    }

    // Check label and description
    const searchableText = [
      element.label ?? "",
      element.description ?? "",
      element.element_type,
      ...(element.properties ? Object.keys(element.properties) : []),
      ...(element.properties ? Object.values(element.properties) : []),
    ].join(" ");

    for (const pattern of SENSITIVE_ATTR_PATTERNS) {
      if (pattern.test(searchableText)) {
        return pattern.source.replace(/[/\\i]/g, "").toLowerCase();
      }
    }

    return null;
  }

  /** Mask an action's text fields before sending to LLM. */
  maskAction(action: PlannedAction): PlannedAction {
    switch (action.type) {
      case "type":
        return { ...action, text: this.mask(action.text) };
      case "set_value":
        return { ...action, value: this.mask(action.value) };
      case "done":
        return { ...action, summary: this.mask(action.summary) };
      case "fail":
        return { ...action, reason: this.mask(action.reason) };
      default:
        return action;
    }
  }

  /** Unmask an action's text fields after receiving from LLM. */
  unmaskAction(action: PlannedAction): PlannedAction {
    switch (action.type) {
      case "type":
        return { ...action, text: this.unmask(action.text) };
      case "set_value":
        return { ...action, value: this.unmask(action.value) };
      case "done":
        return { ...action, summary: this.unmask(action.summary) };
      case "fail":
        return { ...action, reason: this.unmask(action.reason) };
      default:
        return action;
    }
  }
}
