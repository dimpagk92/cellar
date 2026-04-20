/**
 * CEL Interfaces — composable abstractions for the Cel runtime.
 *
 * Instead of depending on the monolithic Cel class, consumers should
 * import only the interfaces they need. This enables:
 * - Type-safe mocking in tests (no `as any` casts)
 * - Dependency injection for custom implementations
 * - Clear documentation of what each module actually uses
 *
 * The Cel class implements all 6 interfaces for backwards compatibility.
 */

export type { ContextProvider } from "./context-provider.js";
export type { InputController } from "./input-controller.js";
export type { Planner } from "./planner.js";
export type { KnowledgeStore } from "./knowledge-store.js";
export type { BrowserBridge } from "./browser-bridge.js";
export type { EventSource } from "./event-source.js";

/**
 * CelComposite — the full Cel interface as a type intersection.
 * Use this where you genuinely need all capabilities (e.g., MCP server entry point).
 * Prefer narrower interfaces in all other cases.
 */
export type CelComposite =
  import("./context-provider.js").ContextProvider &
  import("./input-controller.js").InputController &
  import("./planner.js").Planner &
  import("./knowledge-store.js").KnowledgeStore &
  import("./browser-bridge.js").BrowserBridge &
  import("./event-source.js").EventSource & {
    /** Whether the native module is available. */
    readonly isNativeAvailable: boolean;
    /** Get CEL version. */
    version(): string;
  };
