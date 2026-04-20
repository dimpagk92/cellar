/**
 * Rust Adapter Bridge — wraps Rust-native adapters (via cel-napi) as TS AdapterInstances.
 *
 * Rust adapters implement the `Adapter` trait (connect/disconnect/get_elements/execute_action)
 * in adapter-common. This bridge maps those to the TS AdapterInstance interface so they
 * can be registered in the AdapterRegistry and produce AdapterCapabilities for the kernel.
 *
 * License: MIT
 */

import type { AdapterCapabilities } from "./types.js";
import type { AdapterInstance, AdapterManifest, AdapterState, AdapterPlatform } from "./adapter-registry.js";
import type { ScreenContext, PlannedAction, ContextElement } from "../types.js";

/**
 * CEL NAPI bindings for adapter lifecycle.
 * These map to functions in cel/cel-napi/src/adapter_registry.rs.
 */
export interface NapiAdapterBindings {
  registerAdapter(name: string): void;
  connectAdapter(name: string): Promise<void>;
  disconnectAdapter(name: string): Promise<void>;
  probeAdapter(name: string): Promise<boolean>;
  adapterGetElements(name: string): Promise<string>; // JSON ContextElement[]
  adapterExecuteAction(name: string, action: string, params: string): Promise<string>; // JSON result
  adapterInfo(name: string): string; // JSON AdapterInfo
}

/**
 * Bridge a Rust-native adapter to the TS AdapterInstance interface.
 *
 * Usage:
 *   const napi = require("cel-napi");
 *   const excel = new RustAdapterBridge(napi, {
 *     name: "excel", displayName: "Microsoft Excel",
 *     platforms: ["windows", "macos"],
 *     supportedActionTypes: new Set(["click", "read_cell", "write_cell"]),
 *     requiresApp: "Microsoft Excel",
 *   });
 *   registry.register(excel);
 */
export class RustAdapterBridge implements AdapterInstance {
  readonly manifest: AdapterManifest;
  state: AdapterState = "disconnected";

  constructor(
    private napi: NapiAdapterBindings,
    manifest: AdapterManifest,
  ) {
    this.manifest = manifest;
  }

  async connect(): Promise<void> {
    this.state = "connecting";
    try {
      this.napi.registerAdapter(this.manifest.name);
      await this.napi.connectAdapter(this.manifest.name);
      this.state = "connected";
    } catch (e) {
      this.state = "error";
      throw e;
    }
  }

  async disconnect(): Promise<void> {
    try {
      await this.napi.disconnectAdapter(this.manifest.name);
    } catch { /* best-effort */ }
    this.state = "disconnected";
  }

  async probe(): Promise<boolean> {
    try {
      return await this.napi.probeAdapter(this.manifest.name);
    } catch {
      return false;
    }
  }

  buildCapabilities(): AdapterCapabilities {
    const { napi, manifest } = this;
    const name = manifest.name;

    return {
      readContext: async (): Promise<ScreenContext> => {
        const json = await napi.adapterGetElements(name);
        const elements: ContextElement[] = JSON.parse(json);
        return {
          app: manifest.requiresApp ?? manifest.displayName,
          window: manifest.displayName,
          elements,
          timestamp_ms: Date.now(),
        };
      },

      executeStructured: async (action: PlannedAction, _context: ScreenContext): Promise<boolean> => {
        // Map PlannedAction to the adapter's action/params format
        const actionName = action.type;
        const params = JSON.stringify(action);
        try {
          const result = await napi.adapterExecuteAction(name, actionName, params);
          const parsed = JSON.parse(result);
          return parsed.success !== false;
        } catch {
          return false;
        }
      },

      // Rust native adapters use direct API calls with high confidence (0.95+).
      // No LLM-based semantic disambiguation needed.
      resolveSemantic: async (): Promise<PlannedAction | null> => null,

      // Native adapters don't need vision fallback — they have direct API access.
      captureScreenshot: async (): Promise<Buffer> => Buffer.from([]),
    };
  }

  async healthCheck(): Promise<boolean> {
    return this.probe();
  }
}
