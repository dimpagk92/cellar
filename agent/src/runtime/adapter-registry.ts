/**
 * Adapter Registry — formalizes the adapter capability system.
 *
 * Adapters register with a static manifest (supported actions, platforms, target app)
 * and provide a buildCapabilities() method that produces the AdapterCapabilities
 * the kernel expects. The registry supports:
 * - Capability discovery (findByActionType, findByApp)
 * - Lifecycle management (connect, disconnect, probe)
 * - Cross-app hot-swap (when the cortex detects a crossAppShift)
 *
 * License: MIT
 */

import type { AdapterCapabilities } from "./types.js";
import type { PlannedAction } from "../types.js";

// ── Adapter Manifest ───────────────────────────────────────────────────────

/** Supported platforms. */
export type AdapterPlatform = "macos" | "windows" | "linux";

/** Lifecycle state of an adapter instance. */
export type AdapterState = "disconnected" | "connecting" | "connected" | "error";

/**
 * Static metadata declaring the adapter's identity and capabilities.
 * Provided at registration time; does not change at runtime.
 */
export interface AdapterManifest {
  /** Unique adapter name (e.g., "browser", "excel", "sap-gui"). */
  name: string;
  /** Human-readable display name (e.g., "Browser (CDP + Playwright)"). */
  displayName: string;
  /** Platforms this adapter supports. */
  platforms: AdapterPlatform[];
  /** Action types this adapter can execute (click, type, navigate, read_cell, etc.). */
  supportedActionTypes: Set<string>;
  /** If set, the adapter targets a specific application (e.g., "Microsoft Excel"). */
  requiresApp?: string;
  /** Application name patterns for auto-detection (matched against ScreenContext.app). */
  appPatterns?: RegExp[];
}

// ── Adapter Instance ───────────────────────────────────────────────────────

/**
 * Runtime adapter instance with lifecycle and capability building.
 * Each adapter implements this interface to plug into the registry.
 */
export interface AdapterInstance {
  /** Static manifest (identity, supported actions, platform). */
  readonly manifest: AdapterManifest;
  /** Current lifecycle state. */
  state: AdapterState;

  /** Connect to the target application. */
  connect(): Promise<void>;
  /** Disconnect and release resources. */
  disconnect(): Promise<void>;
  /** Check if the target application is running and reachable. */
  probe(): Promise<boolean>;
  /** Build the AdapterCapabilities for the kernel. */
  buildCapabilities(): AdapterCapabilities;
  /** Health check — returns true if the adapter is operational. */
  healthCheck(): Promise<boolean>;
}

// ── Adapter Registry ───────────────────────────────────────────────────────

/**
 * Central registry for adapter instances.
 *
 * The registry does NOT own adapter construction — adapters are created
 * externally and registered here. This keeps the registry decoupled from
 * adapter-specific dependencies (CDP, COM, etc.).
 */
export class AdapterRegistry {
  private adapters = new Map<string, AdapterInstance>();
  private activeName: string | null = null;

  /** Register an adapter. Throws if name already registered. */
  register(adapter: AdapterInstance): void {
    const name = adapter.manifest.name;
    if (this.adapters.has(name)) {
      throw new Error(`Adapter "${name}" is already registered`);
    }
    this.adapters.set(name, adapter);
  }

  /** Unregister an adapter by name. Disconnects if connected. */
  async unregister(name: string): Promise<void> {
    const adapter = this.adapters.get(name);
    if (!adapter) return;
    if (adapter.state === "connected") {
      await adapter.disconnect();
    }
    if (this.activeName === name) {
      this.activeName = null;
    }
    this.adapters.delete(name);
  }

  /** Get an adapter by name. */
  get(name: string): AdapterInstance | undefined {
    return this.adapters.get(name);
  }

  /** List all registered adapter names. */
  list(): string[] {
    return Array.from(this.adapters.keys());
  }

  /** List all registered adapters with their manifests and state. */
  listAll(): Array<{ name: string; displayName: string; state: AdapterState; app?: string }> {
    return Array.from(this.adapters.values()).map((a) => ({
      name: a.manifest.name,
      displayName: a.manifest.displayName,
      state: a.state,
      app: a.manifest.requiresApp,
    }));
  }

  /**
   * Find adapters that support a given action type.
   * Useful for determining which adapters can handle a PlannedAction.
   */
  findByActionType(actionType: string): AdapterInstance[] {
    return Array.from(this.adapters.values()).filter(
      (a) => a.manifest.supportedActionTypes.has(actionType),
    );
  }

  /**
   * Find an adapter by target application name.
   * Matches against manifest.requiresApp and manifest.appPatterns.
   * Used for cross-app hot-swap when the cortex detects a crossAppShift.
   */
  findByApp(appName: string): AdapterInstance | undefined {
    const lower = appName.toLowerCase();
    for (const adapter of this.adapters.values()) {
      // Exact match on requiresApp
      if (adapter.manifest.requiresApp?.toLowerCase() === lower) {
        return adapter;
      }
      // Pattern match on appPatterns
      if (adapter.manifest.appPatterns?.some((p) => p.test(appName))) {
        return adapter;
      }
    }
    return undefined;
  }

  /**
   * Find the best adapter for a planned action, considering action type
   * and optionally the target app context.
   */
  findForAction(action: PlannedAction, appName?: string): AdapterInstance | undefined {
    // If a specific adapter is named in the action, use it
    if ("adapter" in action && typeof (action as any).adapter === "string") {
      return this.adapters.get((action as any).adapter);
    }
    // If app context is available, try app-specific adapter first
    if (appName) {
      const appAdapter = this.findByApp(appName);
      if (appAdapter?.manifest.supportedActionTypes.has(action.type)) {
        return appAdapter;
      }
    }
    // Fall back to any adapter that supports this action type
    const candidates = this.findByActionType(action.type);
    // Prefer connected adapters
    const connected = candidates.filter((a) => a.state === "connected");
    return connected[0] ?? candidates[0];
  }

  /** Get the currently active adapter. */
  getActive(): AdapterInstance | undefined {
    return this.activeName ? this.adapters.get(this.activeName) : undefined;
  }

  /** Get the name of the currently active adapter. */
  getActiveName(): string | null {
    return this.activeName;
  }

  /**
   * Set the active adapter. Connects if not already connected.
   * Returns the adapter's capabilities for immediate use.
   */
  async setActive(name: string): Promise<AdapterCapabilities> {
    const adapter = this.adapters.get(name);
    if (!adapter) {
      throw new Error(`Adapter "${name}" is not registered`);
    }
    if (adapter.state !== "connected") {
      await adapter.connect();
    }
    this.activeName = name;
    return adapter.buildCapabilities();
  }

  /**
   * Get capabilities from the active adapter.
   * Throws if no adapter is active.
   */
  getActiveCapabilities(): AdapterCapabilities {
    const adapter = this.getActive();
    if (!adapter) {
      throw new Error("No active adapter — call setActive() first");
    }
    if (adapter.state !== "connected") {
      throw new Error(`Active adapter "${this.activeName}" is not connected (state: ${adapter.state})`);
    }
    return adapter.buildCapabilities();
  }

  /**
   * Hot-swap the active adapter based on a detected app change.
   * Returns the new capabilities, or null if no adapter matches the app.
   */
  async swapForApp(appName: string): Promise<AdapterCapabilities | null> {
    const adapter = this.findByApp(appName);
    if (!adapter) return null;
    // Don't swap if already active
    if (adapter.manifest.name === this.activeName) {
      return adapter.buildCapabilities();
    }
    return this.setActive(adapter.manifest.name);
  }

  /** Disconnect all adapters and clear the registry. */
  async dispose(): Promise<void> {
    for (const adapter of this.adapters.values()) {
      if (adapter.state === "connected") {
        try { await adapter.disconnect(); } catch { /* best-effort */ }
      }
    }
    this.adapters.clear();
    this.activeName = null;
  }
}
