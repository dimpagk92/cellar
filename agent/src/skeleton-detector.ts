/**
 * Skeleton screen detection — heuristics to identify loading/skeleton states.
 *
 * Avoids acting on pages that are still loading by detecting common patterns:
 * - Many elements but almost no text content (placeholder skeletons)
 * - Very few interactive elements relative to total (loading state)
 * - Elements with aria-busy="true" or aria-live="polite" (loading indicators)
 */

import type { ScreenContext } from "./types.js";

/** Minimum elements to consider skeleton detection meaningful. */
const MIN_ELEMENTS_FOR_DETECTION = 5;

/** If text-bearing ratio is below this, likely skeleton placeholders. */
const TEXT_RATIO_THRESHOLD = 0.15;

/** If interactive ratio is below this, likely in loading state. */
const INTERACTIVE_RATIO_THRESHOLD = 0.05;

/** Interactive element types. */
const INTERACTIVE_TYPES = new Set([
  "button",
  "link",
  "input",
  "textarea",
  "select",
  "checkbox",
  "radio",
  "switch",
  "slider",
  "menu_item",
  "tab",
  "combobox",
]);

/** Default wait when skeleton detected. */
const DEFAULT_SKELETON_WAIT_MS = 2000;
/** Extended wait when strong skeleton signals. */
const EXTENDED_SKELETON_WAIT_MS = 4000;

/**
 * Detect if a context looks like a skeleton/loading screen.
 */
export function isSkeletonScreen(context: ScreenContext): boolean {
  const elements = context.elements;
  if (elements.length < MIN_ELEMENTS_FOR_DETECTION) {
    return false;
  }

  // Check for explicit loading indicators
  const hasAriaBusy = elements.some(
    (el) => el.properties?.["aria-busy"] === "true" || el.properties?.["busy"] === "true",
  );
  if (hasAriaBusy) return true;

  // Check text-bearing ratio
  const textBearingCount = elements.filter(
    (el) => (el.label && el.label.trim().length > 0) || (el.value && el.value.trim().length > 0),
  ).length;
  const textRatio = textBearingCount / elements.length;

  // Check interactive element ratio
  const interactiveCount = elements.filter((el) =>
    INTERACTIVE_TYPES.has(el.element_type),
  ).length;
  const interactiveRatio = interactiveCount / elements.length;

  // Skeleton: many elements but almost no text AND very few interactive elements
  if (textRatio < TEXT_RATIO_THRESHOLD && interactiveRatio < INTERACTIVE_RATIO_THRESHOLD) {
    return true;
  }

  // Check for aria-live regions (often used for loading status)
  const hasAriaLiveWithLoading = elements.some((el) => {
    const ariaLive = el.properties?.["aria-live"];
    if (!ariaLive) return false;
    const text = (el.label ?? "") + (el.description ?? "");
    return /loading|spinner|please wait/i.test(text);
  });
  if (hasAriaLiveWithLoading) return true;

  return false;
}

/**
 * Suggested wait time in milliseconds based on skeleton detection signals.
 * Returns 0 if no skeleton/loading state detected.
 */
export function skeletonWaitMs(context: ScreenContext): number {
  const elements = context.elements;
  if (elements.length < MIN_ELEMENTS_FOR_DETECTION) {
    return 0;
  }

  // Strong signal: explicit aria-busy
  const hasAriaBusy = elements.some(
    (el) => el.properties?.["aria-busy"] === "true" || el.properties?.["busy"] === "true",
  );
  if (hasAriaBusy) return EXTENDED_SKELETON_WAIT_MS;

  // Check ratios
  const textBearingCount = elements.filter(
    (el) => (el.label && el.label.trim().length > 0) || (el.value && el.value.trim().length > 0),
  ).length;
  const textRatio = textBearingCount / elements.length;

  const interactiveCount = elements.filter((el) =>
    INTERACTIVE_TYPES.has(el.element_type),
  ).length;
  const interactiveRatio = interactiveCount / elements.length;

  if (textRatio < TEXT_RATIO_THRESHOLD && interactiveRatio < INTERACTIVE_RATIO_THRESHOLD) {
    return DEFAULT_SKELETON_WAIT_MS;
  }

  // Aria-live loading
  const hasAriaLiveWithLoading = elements.some((el) => {
    const ariaLive = el.properties?.["aria-live"];
    if (!ariaLive) return false;
    const text = (el.label ?? "") + (el.description ?? "");
    return /loading|spinner|please wait/i.test(text);
  });
  if (hasAriaLiveWithLoading) return DEFAULT_SKELETON_WAIT_MS;

  return 0;
}

// ─── Spinner / Loading Bar Detection ─────────────────────────────────────

/** Patterns that indicate active loading in element labels. */
const SPINNER_LABEL_PATTERNS = [
  /loading/i,
  /spinner/i,
  /progress/i,
  /indeterminate/i,
  /please wait/i,
  /fetching/i,
  /buffering/i,
];

/** Element types that are explicit loading indicators. */
const LOADING_ELEMENT_TYPES = new Set([
  "progress_indicator",
  "busy_indicator",
  "progressbar",
  "activity_indicator",
]);

/**
 * Detect if the context has an active spinner, progress bar, or loading indicator.
 * More targeted than isSkeletonScreen — detects individual loading elements
 * even when the page is otherwise populated.
 */
export function hasActiveSpinner(context: ScreenContext): boolean {
  return context.elements.some((el) => {
    // Explicit loading element types
    if (LOADING_ELEMENT_TYPES.has(el.element_type)) {
      return el.state?.visible !== false;
    }

    // Check label patterns on visible elements
    const label = el.label ?? "";
    if (label.length === 0) return false;

    return (
      el.state?.visible !== false &&
      SPINNER_LABEL_PATTERNS.some((p) => p.test(label))
    );
  });
}
