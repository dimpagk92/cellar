/**
 * ContextProvider — read screen state.
 *
 * Abstracts all context-reading operations from the Cel god class.
 * Consumers that only need to read screen state should depend on this
 * interface, not the full Cel class.
 */

import type {
  ScreenContext,
  ContextElement,
  ContextReference,
  FocusedContext,
  HttpEvent,
  MenuBarItem,
} from "../types.js";
import type { MonitorInfo, WindowInfo } from "../cel-bindings.js";

export interface ContextProvider {
  /** Get the unified screen context (full accessibility tree walk). */
  getContext(): ScreenContext;

  /** Get minimal context: app name + window title only. No tree walk (~50ms). */
  getQuickContext(): ScreenContext;

  /** Get high-fidelity context for a single element by ID. */
  getContextFocused(elementId: string): FocusedContext | null;

  /** Capture a screenshot as PNG buffer. */
  captureScreen(): Buffer;

  /** List available monitors. */
  listMonitors(): MonitorInfo[];

  /** List visible windows. */
  listWindows(): WindowInfo[];

  /** Get current mouse cursor position as [x, y]. */
  mousePosition(): [number, number];

  /**
   * Build a ScreenContext from externally-provided elements.
   * Routes through the Rust CEL core for unified confidence scoring,
   * element type normalization, noise filtering, and sorting.
   */
  buildContextFromElements(
    elements: ContextElement[],
    networkEvents: HttpEvent[],
    appName: string,
    windowTitle: string,
  ): ScreenContext;

  /** Create a resilient reference from an element. */
  makeReference(
    element: ContextElement,
    screenWidth?: number,
    screenHeight?: number,
  ): ContextReference;

  /** Resolve a reference against a screen context snapshot. */
  resolveReference(
    context: ScreenContext,
    ref: ContextReference,
  ): ContextElement | null;

  /** Get the menu bar structure of the focused app. */
  axGetMenuBar(): MenuBarItem[];

  /** Get ALL windows of the focused app (not just focused one). */
  axGetAllWindows(): ContextElement[];
}
