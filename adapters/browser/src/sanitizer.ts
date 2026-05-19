/**
 * Sanitizer — prevents prompt injection by cleaning element labels and values.
 *
 * Runs as a post-processing step on every ContextElement[] before
 * it leaves the browser adapter.
 *
 * License: MIT
 */

import type { ContextElement } from "@cellar/agent/runtime";

const MAX_LABEL_LENGTH = 200;
const MAX_VALUE_LENGTH = 500;
const SUSPICIOUS_CONFIDENCE_PENALTY = 0.1;
const LABEL_HEAD_LENGTH = 140;
const LABEL_TAIL_LENGTH = 40;
const VALUE_HEAD_LENGTH = 380;
const VALUE_TAIL_LENGTH = 80;

/** Known LLM instruction injection patterns. */
const INJECTION_PATTERNS = [
  /\[INST\]/gi,
  /\[\/INST\]/gi,
  /<\|system\|>/gi,
  /<\|user\|>/gi,
  /<\|assistant\|>/gi,
  /<<SYS>>/gi,
  /<<\/SYS>>/gi,
  /<\/s>/gi,
  /\[SYSTEM\]/gi,
  /\[\/SYSTEM\]/gi,
  /```\s*(?:system|ignore|override|forget|disregard)/gi,
  /IMPORTANT:\s*ignore\s*(?:previous|above|all)/gi,
  /you\s+are\s+now\s+(?:a|an|in)\s+/gi,
  /ignore\s+(?:all\s+)?(?:previous|prior|above)\s+instructions/gi,
  /disregard\s+(?:all\s+)?(?:previous|prior|above)/gi,
];

/** Strip control characters except tab and newline. */
function stripControlChars(text: string): string {
  return text.replace(/[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]/g, "");
}

/** Collapse multiple whitespace characters into single spaces. */
function collapseWhitespace(text: string): string {
  return text.replace(/\s+/g, " ").trim();
}

/** Check if text contains injection patterns. Returns true if suspicious. */
function containsInjection(text: string): boolean {
  return INJECTION_PATTERNS.some((pattern) => pattern.test(text));
}

/** Remove injection patterns from text. */
function removeInjectionPatterns(text: string): string {
  let cleaned = text;
  for (const pattern of INJECTION_PATTERNS) {
    cleaned = cleaned.replace(pattern, "");
  }
  return cleaned;
}

/** Escape backticks to prevent markdown code injection. */
function escapeBackticks(text: string): string {
  return text.replace(/`/g, "'");
}

/** Preserve both the prefix and suffix of long text to keep identities like emails, IDs, and versions. */
function smartTruncate(text: string, maxLength: number, headLength: number, tailLength: number): string {
  if (text.length <= maxLength) return text;

  const ellipsis = "...";
  const available = maxLength - ellipsis.length;
  const safeHead = Math.min(headLength, Math.max(0, available - tailLength));
  const safeTail = Math.min(tailLength, Math.max(0, available - safeHead));

  if (safeTail <= 0) {
    return text.slice(0, maxLength - ellipsis.length) + ellipsis;
  }

  return text.slice(0, safeHead) + ellipsis + text.slice(text.length - safeTail);
}

/** Sanitize a single text field. Returns [cleanedText, isSuspicious]. */
function sanitizeText(
  text: string,
  maxLength: number,
  options?: { headLength?: number; tailLength?: number },
): [string, boolean, string | null] {
  let suspicious = false;

  // Check for injection before cleaning
  if (containsInjection(text)) {
    suspicious = true;
  }

  let cleaned = stripControlChars(text);
  cleaned = removeInjectionPatterns(cleaned);
  cleaned = collapseWhitespace(cleaned);
  cleaned = escapeBackticks(cleaned);

  // Truncate
  let fullText: string | null = null;
  if (cleaned.length > maxLength) {
    fullText = cleaned;
    cleaned = smartTruncate(
      cleaned,
      maxLength,
      options?.headLength ?? Math.max(0, maxLength - 20),
      options?.tailLength ?? 0,
    );
  }

  return [cleaned, suspicious, fullText];
}

/** Sanitize an array of ContextElements in-place. */
export function sanitizeElements(elements: ContextElement[]): ContextElement[] {
  for (const el of elements) {
    let suspicious = false;
    el.properties ??= {};

    if (el.label) {
      const [cleaned, isSuspicious, fullText] = sanitizeText(el.label, MAX_LABEL_LENGTH, {
        headLength: LABEL_HEAD_LENGTH,
        tailLength: LABEL_TAIL_LENGTH,
      });
      el.label = cleaned || undefined;
      suspicious = suspicious || isSuspicious;
      if (fullText) {
        el.properties.full_label = fullText;
        el.properties.label_truncated = "true";
      }
    }

    if (el.value) {
      const [cleaned, isSuspicious, fullText] = sanitizeText(el.value, MAX_VALUE_LENGTH, {
        headLength: VALUE_HEAD_LENGTH,
        tailLength: VALUE_TAIL_LENGTH,
      });
      el.value = cleaned || undefined;
      suspicious = suspicious || isSuspicious;
      if (fullText) {
        el.properties.full_value = fullText;
        el.properties.value_truncated = "true";
      }
    }

    if (el.description) {
      const [cleaned, isSuspicious, fullText] = sanitizeText(
        el.description,
        MAX_LABEL_LENGTH,
        {
          headLength: LABEL_HEAD_LENGTH,
          tailLength: LABEL_TAIL_LENGTH,
        },
      );
      el.description = cleaned || undefined;
      suspicious = suspicious || isSuspicious;
      if (fullText) {
        el.properties.full_description = fullText;
        el.properties.description_truncated = "true";
      }
    }

    if (suspicious) {
      el.confidence = Math.max(0, el.confidence - SUSPICIOUS_CONFIDENCE_PENALTY);
    }
  }

  return elements;
}
