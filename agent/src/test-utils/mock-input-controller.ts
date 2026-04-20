/**
 * Mock InputController for testing.
 *
 * Records all input calls for assertion. No actual input is injected.
 */

import type { InputController } from "../interfaces/input-controller.js";

export interface InputCall {
  method: string;
  args: unknown[];
  timestamp: number;
}

/** Create a type-safe mock InputController that records all calls. */
export function createMockInputController(): InputController & {
  calls: InputCall[];
  reset(): void;
} {
  const calls: InputCall[] = [];

  function track(method: string, ...args: unknown[]) {
    calls.push({ method, args, timestamp: Date.now() });
  }

  return {
    calls,
    reset() { calls.length = 0; },

    mouseMove(x, y) { track("mouseMove", x, y); },
    click(x, y) { track("click", x, y); },
    rightClick(x, y) { track("rightClick", x, y); },
    doubleClick(x, y) { track("doubleClick", x, y); },
    typeText(text) { track("typeText", text); },
    keyPress(key) { track("keyPress", key); },
    keyCombo(keys) { track("keyCombo", keys); },
    scroll(dx, dy) { track("scroll", dx, dy); },
    drag(fromX, fromY, toX, toY) { track("drag", fromX, fromY, toX, toY); },
    tripleClick(x, y) { track("tripleClick", x, y); },
    keyDown(key) { track("keyDown", key); },
    keyUp(key) { track("keyUp", key); },
    paste() { track("paste"); },
    selectAll() { track("selectAll"); },
    mouseMoveSmooth(x, y, durationMs) { track("mouseMoveSmooth", x, y, durationMs); },
    axPerformAction(elementId, action) {
      track("axPerformAction", elementId, action);
      return true;
    },
    axSetValue(elementId, value) {
      track("axSetValue", elementId, value);
      return true;
    },
    axIsSettable(_elementId) {
      track("axIsSettable", _elementId);
      return true;
    },
    axElementAtPosition(_x, _y) {
      track("axElementAtPosition", _x, _y);
      return null;
    },
    activateApp(appName) {
      track("activateApp", appName);
      return true;
    },
  };
}
