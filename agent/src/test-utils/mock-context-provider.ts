/**
 * Mock ContextProvider for testing.
 *
 * Returns canned ScreenContext data. All method calls are recorded
 * for assertion in tests.
 */

import type { ContextProvider } from "../interfaces/context-provider.js";
import type {
  ScreenContext,
  ContextElement,
  ContextReference,
  FocusedContext,
  HttpEvent,
  MenuBarItem,
} from "../types.js";
import type { MonitorInfo, WindowInfo } from "../cel-bindings.js";

/** Creates a minimal empty ScreenContext. */
export function emptyContext(overrides?: Partial<ScreenContext>): ScreenContext {
  return {
    app: "",
    window: "",
    elements: [],
    timestamp_ms: Date.now(),
    ...overrides,
  };
}

/** Creates a ScreenContext with some elements for testing. */
export function sampleContext(overrides?: Partial<ScreenContext>): ScreenContext {
  return {
    app: "TestApp",
    window: "Test Window",
    elements: [
      {
        id: "a11y:1",
        element_type: "button",
        label: "Submit",
        bounds: { x: 100, y: 200, width: 80, height: 30 },
        state: { visible: true, enabled: true, focused: false },
        actions: ["press"],
        confidence: 0.95,
        source: "accessibility_tree",
      },
      {
        id: "a11y:2",
        element_type: "input",
        label: "Email",
        value: "",
        bounds: { x: 100, y: 100, width: 200, height: 30 },
        state: { visible: true, enabled: true, focused: true },
        actions: ["focus", "setValue"],
        confidence: 0.95,
        source: "accessibility_tree",
      },
    ] as ContextElement[],
    timestamp_ms: Date.now(),
    ...overrides,
  };
}

export interface MockContextProviderOptions {
  /** Context to return from getContext(). Defaults to sampleContext(). */
  context?: ScreenContext;
  /** Context to return from getQuickContext(). Defaults to emptyContext(). */
  quickContext?: ScreenContext;
}

/** Create a type-safe mock ContextProvider. */
export function createMockContextProvider(
  options?: MockContextProviderOptions,
): ContextProvider & { calls: Record<string, unknown[][]> } {
  const ctx = options?.context ?? sampleContext();
  const quickCtx = options?.quickContext ?? emptyContext({ app: ctx.app, window: ctx.window });
  const calls: Record<string, unknown[][]> = {};

  function track(method: string, args: unknown[]) {
    if (!calls[method]) calls[method] = [];
    calls[method].push(args);
  }

  return {
    calls,
    getContext() {
      track("getContext", []);
      return ctx;
    },
    getQuickContext() {
      track("getQuickContext", []);
      return quickCtx;
    },
    getContextFocused(elementId: string) {
      track("getContextFocused", [elementId]);
      return null;
    },
    captureScreen() {
      track("captureScreen", []);
      return Buffer.from("fake-png");
    },
    listMonitors(): MonitorInfo[] {
      track("listMonitors", []);
      return [{ id: 0, name: "Main", x: 0, y: 0, width: 1920, height: 1080, is_primary: true }];
    },
    listWindows(): WindowInfo[] {
      track("listWindows", []);
      return [];
    },
    mousePosition(): [number, number] {
      track("mousePosition", []);
      return [0, 0];
    },
    buildContextFromElements(elements, networkEvents, appName, windowTitle) {
      track("buildContextFromElements", [elements, networkEvents, appName, windowTitle]);
      return { app: appName, window: windowTitle, elements, timestamp_ms: Date.now() };
    },
    makeReference(element) {
      track("makeReference", [element]);
      return { element_type: element.element_type, label: element.label };
    },
    resolveReference(_context, _ref) {
      track("resolveReference", [_context, _ref]);
      return null;
    },
    axGetMenuBar(): MenuBarItem[] {
      track("axGetMenuBar", []);
      return [];
    },
    axGetAllWindows(): ContextElement[] {
      track("axGetAllWindows", []);
      return [];
    },
  };
}
