/**
 * InputController — mouse, keyboard, and accessibility input.
 *
 * Abstracts all input-injection operations from the Cel god class.
 * Consumers that only need to execute actions should depend on this
 * interface, not the full Cel class.
 */

import type { ContextElement } from "../types.js";

export interface InputController {
  /** Move mouse to absolute coordinates. */
  mouseMove(x: number, y: number): void;

  /** Left-click at coordinates. */
  click(x: number, y: number): void;

  /** Right-click at coordinates. */
  rightClick(x: number, y: number): void;

  /** Double-click at coordinates. */
  doubleClick(x: number, y: number): void;

  /** Type text using fast unicode input. */
  typeText(text: string): void;

  /** Press a single key. */
  keyPress(key: string): void;

  /** Press a key combination. */
  keyCombo(keys: string[]): void;

  /** Scroll at current position. */
  scroll(dx: number, dy: number): void;

  /** Drag from one point to another. */
  drag(fromX: number, fromY: number, toX: number, toY: number): void;

  /** Triple-click at coordinates (selects full line/paragraph). */
  tripleClick(x: number, y: number): void;

  /** Press a key down without releasing. */
  keyDown(key: string): void;

  /** Release a key that was previously pressed with keyDown(). */
  keyUp(key: string): void;

  /** Paste from clipboard. */
  paste(): void;

  /** Select all text in the focused element. */
  selectAll(): void;

  /** Move mouse smoothly with human-like interpolation. */
  mouseMoveSmooth(x: number, y: number, durationMs: number): void;

  /** Execute an action on an element via the accessibility API. */
  axPerformAction(elementId: string, action: string): boolean;

  /** Set a value directly on an element (bypasses mouse/keyboard). */
  axSetValue(elementId: string, value: string): boolean;

  /** Check if an element's value can be set directly. */
  axIsSettable(elementId: string): boolean;

  /** Get the accessibility element at screen coordinates (hit testing). */
  axElementAtPosition(x: number, y: number): ContextElement | null;

  /** Activate (bring to front) a macOS application by name. */
  activateApp(appName: string): boolean;
}
